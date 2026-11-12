//! Reimbursement Manager — Rust edition.
//! The whole backend (OCR, parser, Excel store, folder watcher) now runs
//! in-process as Tauri commands; there is no sidecar HTTP server.

mod cloud;
mod config;
mod models;
mod ocr;
mod parser;
mod pipeline;
mod store;
mod watcher;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
    Manager, WindowEvent,
};

use crate::cloud::CloudVisionEngine;
use crate::config::Config;
use crate::ocr::LocalOcrEngine;
use crate::pipeline::is_image;
use crate::store::{ExcelStore, StoreError};
use crate::watcher::{ActivityLog, WatcherService};

pub struct AppState {
    pub base_dir: PathBuf,
    pub cfg: Arc<RwLock<Config>>,
    pub store: Arc<ExcelStore>,
    pub local: Arc<RwLock<Option<Arc<LocalOcrEngine>>>>,
    pub cloud: Arc<RwLock<Option<CloudVisionEngine>>>,
    pub watcher: Mutex<Option<WatcherService>>,
    pub engines_ready: Arc<AtomicBool>,
    pub activity: ActivityLog,
}

impl AppState {
    fn watcher_running(&self) -> bool {
        self.watcher
            .lock()
            .map(|w| w.as_ref().map(|w| w.running()).unwrap_or(false))
            .unwrap_or(false)
    }
}

fn count_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).filter(|e| e.path().is_file()).count())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- commands

#[tauri::command]
fn get_receipts(state: tauri::State<AppState>) -> Value {
    let receipts: Vec<Value> = state
        .store
        .list_rows()
        .into_iter()
        .map(|(row, r)| {
            json!({
                "row": row,               // Excel row number (header is row 1)
                "num": row - 1,           // display number
                "date": r.date,
                "currency": r.currency,
                "amount": r.amount,
                "merchant": r.merchant,   // name of the place
                "needs_review": r.needs_review,
            })
        })
        .collect();
    json!({ "receipts": receipts })
}

#[tauri::command]
fn get_status(state: tauri::State<AppState>) -> Value {
    let cfg = state.cfg.read().unwrap();
    json!({
        "engines_ready": state.engines_ready.load(Ordering::Relaxed),
        "watcher_running": state.watcher_running(),
        "inbox": count_files(&cfg.inbox),
        "processed": count_files(&cfg.processed),
        "errors": count_files(&cfg.errors),
        "pending": state.watcher.lock().map(|w| w.as_ref().map(WatcherService::pending_count).unwrap_or(0)).unwrap_or(0),
        "recent": state.activity.recent(50),
    })
}

#[tauri::command]
fn get_config(state: tauri::State<AppState>) -> Value {
    config::ui_config(&state.cfg.read().unwrap())
}

#[tauri::command]
fn set_config(state: tauri::State<AppState>, patch: Value) -> Result<Value, String> {
    config::apply_ui_patch(&state.base_dir, &patch)?;

    // Reload in-memory config and swap the cloud engine
    let new_cfg = config::load_config(&state.base_dir);
    let cloud_enabled = new_cfg.cloud.is_some();
    let engine = new_cfg.cloud.clone().map(CloudVisionEngine::new);
    *state.cfg.write().unwrap() = new_cfg;
    *state.cloud.write().unwrap() = engine;
    state.activity.info(&format!(
        "Config updated (cloud OCR {})",
        if cloud_enabled { "enabled" } else { "disabled" }
    ));
    Ok(json!({ "ok": true, "cloud_enabled": cloud_enabled }))
}

fn unique_inbox_dest(cfg: &Config, filename: &str) -> PathBuf {
    let safe = std::path::Path::new(filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "upload.jpg".to_string());
    let mut dest = cfg.inbox.join(&safe);
    let mut counter = 1;
    while dest.exists() {
        let stem = std::path::Path::new(&safe)
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = std::path::Path::new(&safe)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        dest = cfg.inbox.join(format!("{stem}_{counter}{ext}"));
        counter += 1;
    }
    dest
}

#[tauri::command]
fn add_path(state: tauri::State<AppState>, path: String) -> Result<Value, String> {
    let src = PathBuf::from(&path);
    if !src.is_file() {
        return Err(format!("file not found: {path}"));
    }
    if !is_image(&src) {
        return Err(format!(
            "unsupported image type: .{}",
            src.extension().map(|e| e.to_string_lossy()).unwrap_or_default()
        ));
    }
    let cfg = state.cfg.read().unwrap();
    let dest = unique_inbox_dest(&cfg, &src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
    std::fs::copy(&src, &dest).map_err(|e| format!("copy failed: {e}"))?;
    state.activity.info(&format!(
        "Added {} to the inbox",
        dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    ));
    Ok(json!({
        "ok": true,
        "queued": dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }))
}

#[tauri::command]
fn upload_image(
    state: tauri::State<AppState>,
    filename: String,
    data: Vec<u8>,
) -> Result<Value, String> {
    let ext = std::path::Path::new(&filename)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if !crate::pipeline::IMAGE_EXTS.contains(&ext.as_str()) {
        return Err("unsupported image type".into());
    }
    if data.is_empty() {
        return Err("empty upload".into());
    }
    let cfg = state.cfg.read().unwrap();
    let dest = unique_inbox_dest(&cfg, &filename);
    std::fs::write(&dest, &data).map_err(|e| format!("write failed: {e}"))?;
    state.activity.info(&format!(
        "Uploaded {} to the inbox",
        dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    ));
    Ok(json!({
        "ok": true,
        "queued": dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }))
}

#[tauri::command]
fn edit_receipt(state: tauri::State<AppState>, row: u32, patch: Value) -> Result<(), String> {
    let current = state
        .store
        .list_rows()
        .into_iter()
        .find(|(r, _)| *r == row)
        .map(|(_, d)| d)
        .ok_or_else(|| format!("row {row} not found"))?;

    let amount = match patch.get("amount") {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    };

    let str_or_current = |key: &str, cur: Option<&String>| -> String {
        patch
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| cur.cloned())
            .unwrap_or_default()
    };

    let updated = models::ReceiptData {
        date: Some(str_or_current("date", current.date.as_ref())),
        currency: str_or_current("currency", Some(&current.currency)),
        merchant: Some(str_or_current("merchant", current.merchant.as_ref())),
        amount,
        tax: current.tax,
        raw_text: current.raw_text.clone(),
        confidence: current.confidence,
        needs_review: false, // manually reviewed via the GUI
        engine: current.engine,
    };
    state.store.update_row(row, updated).map_err(|e| match e {
        StoreError::FileLocked(_) => {
            "receipts.xlsx is open in Excel — close it and retry".to_string()
        }
        other => other.to_string(),
    })
}

#[tauri::command]
fn open_portal(state: tauri::State<AppState>) -> Result<Value, String> {
    let url = state.cfg.read().unwrap().reimbursement_url.trim().to_string();
    if !(url.to_lowercase().starts_with("http://")
        || url.to_lowercase().starts_with("https://"))
    {
        return Err("no reimbursement site configured in Settings".into());
    }
    open::that(&url).map_err(|e| format!("could not open browser: {e}"))?;
    state.activity.info(&format!("Opened reimbursement site: {url}"));
    Ok(json!({ "ok": true, "url": url }))
}

#[tauri::command]
fn scan_now(state: tauri::State<AppState>) -> Result<Value, String> {
    if !state.engines_ready.load(Ordering::Relaxed) {
        return Err("engines still loading".into());
    }
    let queued = state
        .watcher
        .lock()
        .map_err(|_| "watcher unavailable")?
        .as_ref()
        .map(WatcherService::scan_now)
        .unwrap_or(0);
    Ok(json!({ "queued": queued }))
}

// ------------------------------------------------------------------ setup

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Heavy model loading happens after the window is already visible.
fn load_engines(app: tauri::AppHandle) {
    let state = app.state::<AppState>();

    let local = match app.path().resolve("resources/ocr", tauri::path::BaseDirectory::Resource) {
        Ok(dir) => LocalOcrEngine::new(
            &dir.join("text-detection.onnx"),
            &dir.join("text-recognition.onnx"),
        ),
        Err(e) => Err(format!("resolving OCR resources: {e}")),
    };

    match local {
        Ok(engine) => {
            let local = Arc::new(engine);
            *state.local.write().unwrap() = Some(Arc::clone(&local));
            let result = WatcherService::start(
                Arc::clone(&state.cfg),
                Arc::clone(&state.store),
                local,
                Arc::clone(&state.cloud),
                state.activity.clone(),
            );
            match result {
                Ok(service) => {
                    *state.watcher.lock().unwrap() = Some(service);
                    state.engines_ready.store(true, Ordering::Relaxed);
                    state.activity.info("OCR engines ready — watcher is running");
                }
                Err(e) => state
                    .activity
                    .error(&format!("Could not start inbox watcher: {e}")),
            }
        }
        Err(e) => state.activity.error(&format!("Failed to init OCR engine: {e}")),
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_receipts,
            get_status,
            get_config,
            set_config,
            add_path,
            upload_image,
            edit_receipt,
            open_portal,
            scan_now
        ])
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .setup(|app| {
            let base_dir = config::data_dir();
            let cfg = Arc::new(RwLock::new(config::load_config(&base_dir)));
            let store = Arc::new(ExcelStore::new(cfg.read().unwrap().excel_path.clone()).map_err(
                |e| -> Box<dyn std::error::Error> { e.to_string().into() },
            )?);
            let cloud = Arc::new(RwLock::new(
                cfg.read().unwrap().cloud.clone().map(CloudVisionEngine::new),
            ));

            app.manage(AppState {
                base_dir,
                cfg,
                store,
                local: Arc::new(RwLock::new(None)),
                cloud,
                watcher: Mutex::new(None),
                engines_ready: Arc::new(AtomicBool::new(false)),
                activity: ActivityLog::new(),
            });

            let handle = app.handle().clone();
            std::thread::spawn(move || load_engines(handle));

            // System tray
            let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let open_folder =
                MenuItem::with_id(app, "open_folder", "Open Data Folder", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &open_folder, &quit])?;

            tauri::tray::TrayIconBuilder::with_id("main-tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Reimbursement Manager")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "open_folder" => {
                        let dir = app.state::<AppState>().base_dir.clone();
                        let _ = std::process::Command::new("explorer").arg(dir).spawn();
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray: hide the window, the watcher keeps running
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, _event| {});
}
