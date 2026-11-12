//! Watched-folder service: notify observer + single serial worker thread.
//! Port of python-edition app/watcher.py.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::event::{EventKind, ModifyKind};
use notify::{RecursiveMode, Watcher};

use crate::cloud::CloudVisionEngine;
use crate::config::Config;
use crate::models::ReceiptData;
use crate::ocr::LocalOcrEngine;
use crate::pipeline::{is_image, process_image, retry_pending, wait_until_stable, ProcessOutcome};
use crate::store::ExcelStore;

/// Recent log lines shown in the GUI activity feed.
#[derive(Clone)]
pub struct ActivityLog(Arc<Mutex<VecDeque<String>>>);

impl ActivityLog {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::with_capacity(200))))
    }

    pub fn log(&self, level: &str, msg: &str) {
        let line = format!(
            "{} {} {}",
            chrono::Local::now().format("%H:%M:%S"),
            level,
            msg
        );
        if let Ok(mut q) = self.0.lock() {
            q.push_back(line);
            while q.len() > 200 {
                q.pop_front();
            }
        }
    }

    pub fn info(&self, msg: &str) {
        self.log("INFO", msg);
    }

    pub fn warning(&self, msg: &str) {
        self.log("WARNING", msg);
    }

    pub fn error(&self, msg: &str) {
        self.log("ERROR", msg);
    }

    /// The most recent `n` lines, oldest first.
    pub fn recent(&self, n: usize) -> Vec<String> {
        self.0
            .lock()
            .map(|q| q.iter().rev().take(n).rev().cloned().collect())
            .unwrap_or_default()
    }
}

impl Default for ActivityLog {
    fn default() -> Self {
        Self::new()
    }
}

type PendingList = Arc<Mutex<Vec<(ReceiptData, PathBuf)>>>;
type CloudSlot = Arc<RwLock<Option<CloudVisionEngine>>>;

pub struct WatcherService {
    pub queue_tx: Sender<PathBuf>,
    pub pending: PendingList,
    inbox: PathBuf,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl WatcherService {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        cfg: Arc<RwLock<Config>>,
        store: Arc<ExcelStore>,
        local: Arc<LocalOcrEngine>,
        cloud: CloudSlot,
        activity: ActivityLog,
    ) -> Result<Self, notify::Error> {
        let inbox = cfg.read().unwrap().inbox.clone();

        let (tx, rx) = std::sync::mpsc::channel::<PathBuf>();

        // Restart-safe: reprocess anything left over in the inbox
        let mut leftovers = list_images(&inbox);
        leftovers.sort();
        for path in &leftovers {
            let _ = tx.send(path.clone());
        }
        if !leftovers.is_empty() {
            activity.info(&format!(
                "Found {} leftover image(s) in the inbox to process.",
                leftovers.len()
            ));
        }

        let mut watcher = notify::recommended_watcher({
            let tx = tx.clone();
            let activity = activity.clone();
            move |res: Result<notify::Event, notify::Error>| {
                let Ok(event) = res else { return };
                let relevant = matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_))
                );
                if !relevant {
                    return;
                }
                for path in event.paths {
                    if is_image(&path) {
                        activity.info(&format!(
                            "Detected new image: {}",
                            path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                        ));
                        let _ = tx.send(path);
                    }
                }
            }
        })?;
        watcher.watch(&inbox, RecursiveMode::NonRecursive)?;

        let pending: PendingList = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));

        let worker_pending = Arc::clone(&pending);
        let worker_stop = Arc::clone(&stop);
        let worker_running = Arc::clone(&running);
        let worker_activity = activity.clone();
        let worker = std::thread::spawn(move || {
            // Dropping the watcher removes the watch, so keep it alive here.
            worker_loop(
                rx,
                watcher,
                cfg,
                store,
                local,
                cloud,
                worker_pending,
                worker_stop,
                worker_activity,
            );
            worker_running.store(false, Ordering::Relaxed);
        });

        activity.info(&format!(
            "Watching '{}' — drop receipt photos here.",
            inbox.display()
        ));

        Ok(Self {
            queue_tx: tx,
            pending,
            inbox,
            stop,
            running,
            worker: Some(worker),
        })
    }

    /// Enqueue every image currently in the inbox; returns the count.
    pub fn scan_now(&self) -> usize {
        let images = list_images(&self.inbox);
        for path in &images {
            let _ = self.queue_tx.send(path.clone());
        }
        images.len()
    }

    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().map(|p| p.len()).unwrap_or(0)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn list_images(inbox: &Path) -> Vec<PathBuf> {
    let mut images = Vec::new();
    if let Ok(entries) = std::fs::read_dir(inbox) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && is_image(&path) {
                images.push(path);
            }
        }
    }
    images
}

#[allow(clippy::too_many_arguments)]
fn worker_loop(
    rx: Receiver<PathBuf>,
    _watcher: notify::RecommendedWatcher,
    cfg: Arc<RwLock<Config>>,
    store: Arc<ExcelStore>,
    local: Arc<LocalOcrEngine>,
    cloud: CloudSlot,
    pending: PendingList,
    stop: Arc<AtomicBool>,
    activity: ActivityLog,
) {
    let mut last_retry = Instant::now() - Duration::from_secs(60);
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(path) => {
                if !path.exists() {
                    continue;
                }
                if !wait_until_stable(&path, Duration::from_secs(1), Duration::from_secs(30)) {
                    activity.warning(&format!(
                        "Skipped {}: file disappeared before copy finished.",
                        name_of(&path)
                    ));
                    continue;
                }
                let snapshot_cfg = cfg.read().unwrap().clone();
                let cloud_engine = cloud.read().unwrap().clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_image(
                        &path,
                        &snapshot_cfg,
                        &store,
                        &local,
                        cloud_engine.as_ref(),
                    )
                }));
                match result {
                    Ok(ProcessOutcome::ExcelLocked(receipt)) => {
                        if let Ok(mut p) = pending.lock() {
                            p.push((receipt, path));
                        }
                    }
                    Ok(ProcessOutcome::Done) => {}
                    Err(_) => {
                        activity.error(&format!(
                            "Unexpected error while processing {}",
                            name_of(&path)
                        ));
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let pending_len = pending.lock().map(|p| p.len()).unwrap_or(0);
        if pending_len > 0 && last_retry.elapsed() >= Duration::from_secs(60) {
            let snapshot_cfg = cfg.read().unwrap().clone();
            if let Ok(mut p) = pending.lock() {
                retry_pending(&mut p, &snapshot_cfg, &store);
            }
            last_retry = Instant::now();
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}
