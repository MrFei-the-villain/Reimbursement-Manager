"""Watched-folder service: watchdog observer + single serial worker thread."""
import logging
import queue
import threading
import time
from pathlib import Path

from watchdog.events import FileSystemEventHandler
from watchdog.observers import Observer

from .excel import ExcelStore
from .ocr_cloud import CloudVisionEngine
from .ocr_local import LocalOCREngine
from .pipeline import is_image, process_image, wait_until_stable

log = logging.getLogger(__name__)


class _ImageEventHandler(FileSystemEventHandler):
    def __init__(self, q: queue.Queue):
        self.q = q

    def on_created(self, event):
        if not event.is_directory:
            self._push(event.src_path)

    def on_moved(self, event):
        if not event.is_directory:
            self._push(event.dest_path)

    def _push(self, path_str):
        path = Path(path_str)
        if is_image(path):
            log.info("Detected new image: %s", path.name)
            self.q.put(path)


class WatcherService:
    """Runs the inbox watcher (observer + worker) on background threads."""

    def __init__(self, cfg: dict, store: ExcelStore,
                 local_engine: LocalOCREngine,
                 cloud_engine: CloudVisionEngine | None):
        self.cfg = cfg
        self.store = store
        self.local_engine = local_engine
        self.cloud_engine = cloud_engine
        self.q: queue.Queue = queue.Queue()
        self.stop_event = threading.Event()
        self.pending = []  # [(ReceiptData, source_path)] waiting for the Excel lock
        self.observer: Observer | None = None
        self.worker_thread: threading.Thread | None = None

    # ------------------------------------------------------------ control

    def start(self):
        inbox: Path = self.cfg["folders"]["inbox"]
        # Restart-safe: reprocess anything left over in the inbox
        leftovers = sorted(p for p in inbox.iterdir() if p.is_file() and is_image(p))
        for path in leftovers:
            self.q.put(path)
        if leftovers:
            log.info("Found %d leftover image(s) in the inbox to process.", len(leftovers))

        self.observer = Observer()
        self.observer.schedule(_ImageEventHandler(self.q), str(inbox), recursive=False)
        self.observer.start()
        self.worker_thread = threading.Thread(target=self._worker, daemon=True)
        self.worker_thread.start()
        log.info("Watching '%s' — drop receipt photos here.", inbox)

    def stop(self):
        self.stop_event.set()
        if self.observer is not None:
            self.observer.stop()
            self.observer.join()
        if self.worker_thread is not None:
            self.worker_thread.join(timeout=10)

    @property
    def running(self) -> bool:
        return self.worker_thread is not None and self.worker_thread.is_alive()

    def scan_now(self) -> int:
        """Enqueue every image currently in the inbox; returns the count."""
        inbox: Path = self.cfg["folders"]["inbox"]
        images = [p for p in inbox.iterdir() if p.is_file() and is_image(p)]
        for path in images:
            self.q.put(path)
        return len(images)

    def set_cloud_engine(self, cloud_engine: CloudVisionEngine | None):
        self.cloud_engine = cloud_engine

    # ------------------------------------------------------------- worker

    def _worker(self):
        last_retry = 0.0
        while not self.stop_event.is_set():
            try:
                path = self.q.get(timeout=5.0)
            except queue.Empty:
                path = None
            if path is not None and path.exists():
                if not wait_until_stable(path):
                    log.warning("Skipped %s: file disappeared before copy finished.", path.name)
                    continue
                try:
                    receipt = process_image(
                        path, self.cfg, self.store, self.local_engine, self.cloud_engine)
                except Exception as e:  # noqa: BLE001 - never kill the worker
                    log.exception("Unexpected error while processing %s: %s", path.name, e)
                    receipt = None
                if receipt is not None:  # Excel was locked
                    self.pending.append((receipt, path))
            if self.pending and time.time() - last_retry >= 60:
                self._retry_pending()
                last_retry = time.time()

    def _retry_pending(self):
        from .pipeline import retry_pending
        retry_pending(self.pending, self.cfg, self.store)


def run_watcher(cfg: dict, store: ExcelStore, local_engine: LocalOCREngine,
                cloud_engine: CloudVisionEngine | None):
    """Blocking CLI entry: run the service until Ctrl+C."""
    service = WatcherService(cfg, store, local_engine, cloud_engine)
    service.start()
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        log.info("Shutting down…")
        service.stop()
