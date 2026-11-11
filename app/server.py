"""Local HTTP JSON API that hosts the watcher and serves the Tauri GUI."""
import json
import logging
import shutil
import threading
import webbrowser
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlparse

from .config import CONFIG_PATH, load_config
from .excel import ExcelStore, FileLockedError
from .models import ReceiptData
from .ocr_cloud import CloudVisionEngine
from .pipeline import IMAGE_EXTS
from .watcher import WatcherService

log = logging.getLogger(__name__)

# Recent log lines shown in the GUI activity feed
ACTIVITY = deque(maxlen=200)

_CLOUD_KEYS = {"enabled", "fallback_on_low_confidence", "base_url",
               "api_key", "model", "timeout_seconds"}


class _ActivityHandler(logging.Handler):
    def __init__(self):
        super().__init__(level=logging.INFO)
        self.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(message)s", "%H:%M:%S"))

    def emit(self, record):
        try:
            ACTIVITY.append(self.format(record))
        except Exception:  # noqa: BLE001 - logging must never crash the app
            pass


class AppState:
    def __init__(self):
        self.cfg = None
        self.store: ExcelStore | None = None
        self.watcher: WatcherService | None = None
        self.cloud_engine: CloudVisionEngine | None = None
        self.engines_ready = False


def _receipt_dict(row_num: int, r: ReceiptData) -> dict:
    return {
        "row": row_num,          # Excel row number (header is row 1)
        "num": row_num - 1,      # display number
        "date": r.date,
        "currency": r.currency,
        "amount": r.amount,
        "merchant": r.merchant,  # name of the place
        "needs_review": r.needs_review,
    }


def _unique_inbox_dest(cfg: dict, filename: str) -> Path:
    inbox: Path = cfg["folders"]["inbox"]
    safe = Path(filename).name or "upload.jpg"
    dest = inbox / safe
    counter = 1
    while dest.exists():
        dest = inbox / f"{dest.stem}_{counter}{dest.suffix}"
        counter += 1
    return dest


def _make_handler(app_state: AppState):
    class ApiHandler(BaseHTTPRequestHandler):
        state = app_state

        # -------------------------------------------------------- helpers

        def log_message(self, fmt, *args):  # silence default request logging
            pass

        def _cors(self):
            origin = self.headers.get("Origin", "*")
            self.send_header("Access-Control-Allow-Origin", origin)
            self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            self.send_header("Access-Control-Allow-Headers", "Content-Type, X-Filename")

        def _send(self, code: int, obj):
            body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
            self.send_response(code)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self._cors()
            self.end_headers()
            self.wfile.write(body)

        def _read_body(self) -> bytes:
            length = int(self.headers.get("Content-Length", 0))
            return self.rfile.read(length) if length else b""

        def _read_json(self) -> dict:
            try:
                return json.loads(self._read_body().decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                return {}

        # -------------------------------------------------------- methods

        def do_OPTIONS(self):
            self.send_response(204)
            self._cors()
            self.end_headers()

        def do_GET(self):
            path = urlparse(self.path).path
            if path == "/api/health":
                self._send(200, {"status": "ok", "engines_ready": self.state.engines_ready})
            elif path == "/api/receipts":
                rows = self.state.store.list_rows()
                self._send(200, {"receipts": [_receipt_dict(n, r) for n, r in rows]})
            elif path == "/api/config":
                cfg = self.state.cfg
                self._send(200, {
                    "default_currency": cfg["default_currency"],
                    "reimbursement_url": cfg.get("reimbursement_url", ""),
                    "cloud_ocr": {k: cfg["cloud_ocr"][k] for k in _CLOUD_KEYS},
                    "excel_path": str(cfg["excel_path"]),
                    "folders": {k: str(v) for k, v in cfg["folders"].items()},
                })
            elif path == "/api/status":
                cfg = self.state.cfg
                watcher = self.state.watcher

                def _count(folder):
                    return sum(1 for f in folder.iterdir() if f.is_file())

                self._send(200, {
                    "engines_ready": self.state.engines_ready,
                    "watcher_running": watcher.running if watcher else False,
                    "inbox": _count(cfg["folders"]["inbox"]),
                    "processed": _count(cfg["folders"]["processed"]),
                    "errors": _count(cfg["folders"]["errors"]),
                    "pending": len(watcher.pending) if watcher else 0,
                    "recent": list(ACTIVITY)[-50:],
                })
            else:
                self._send(404, {"error": "not found"})

        def do_POST(self):
            path = urlparse(self.path).path
            if path == "/api/config":
                self._post_config()
            elif path == "/api/scan":
                if not self.state.engines_ready:
                    self._send(503, {"error": "engines still loading"})
                    return
                queued = self.state.watcher.scan_now()
                self._send(200, {"queued": queued})
            elif path == "/api/add-path":
                self._post_add_path()
            elif path == "/api/upload":
                self._post_upload()
            elif path.startswith("/api/receipts/"):
                self._post_edit(path.rsplit("/", 1)[-1])
            elif path == "/api/open-portal":
                url = (self.state.cfg.get("reimbursement_url") or "").strip()
                if not url.lower().startswith(("http://", "https://")):
                    self._send(400, {"error": "no reimbursement site configured in Settings"})
                    return
                webbrowser.open(url)
                log.info("Opened reimbursement site: %s", url)
                self._send(200, {"ok": True, "url": url})
            else:
                self._send(404, {"error": "not found"})

        # ------------------------------------------------------ handlers

        def _post_config(self):
            data = self._read_json()
            # Persist to disk, preserving stored (relative) folder paths
            if CONFIG_PATH.exists():
                disk = json.loads(CONFIG_PATH.read_text(encoding="utf-8"))
            else:
                disk = {}
            if "default_currency" in data:
                disk["default_currency"] = data["default_currency"]
            if "reimbursement_url" in data:
                disk["reimbursement_url"] = data["reimbursement_url"]
            if isinstance(data.get("cloud_ocr"), dict):
                cloud = {k: v for k, v in data["cloud_ocr"].items() if k in _CLOUD_KEYS}
                disk.setdefault("cloud_ocr", {}).update(cloud)
            CONFIG_PATH.write_text(
                json.dumps(disk, indent=2, ensure_ascii=False), encoding="utf-8")

            # Update in-memory config and swap the cloud engine
            cfg = self.state.cfg
            if "default_currency" in data:
                cfg["default_currency"] = data["default_currency"]
            if "reimbursement_url" in data:
                cfg["reimbursement_url"] = data["reimbursement_url"]
            if isinstance(data.get("cloud_ocr"), dict):
                for k, v in data["cloud_ocr"].items():
                    if k in _CLOUD_KEYS:
                        cfg["cloud_ocr"][k] = v
            cloud_cfg = cfg["cloud_ocr"]
            if cloud_cfg.get("enabled") and not cloud_cfg.get("api_key"):
                cloud_cfg["enabled"] = False
            engine = CloudVisionEngine(cloud_cfg) if cloud_cfg.get("enabled") else None
            self.state.cloud_engine = engine
            if self.state.watcher:
                self.state.watcher.set_cloud_engine(engine)
            log.info("Config updated (cloud OCR %s)",
                     "enabled" if engine else "disabled")
            self._send(200, {"ok": True, "cloud_enabled": bool(engine)})

        def _post_add_path(self):
            data = self._read_json()
            src = Path(data.get("path", ""))
            if not src.is_file():
                self._send(400, {"error": f"file not found: {src}"})
                return
            if src.suffix.lower() not in IMAGE_EXTS:
                self._send(400, {"error": f"unsupported image type: {src.suffix}"})
                return
            dest = _unique_inbox_dest(self.state.cfg, src.name)
            shutil.copy2(src, dest)
            log.info("Added %s to the inbox", dest.name)
            self._send(200, {"ok": True, "queued": dest.name})

        def _post_upload(self):
            filename = unquote(self.headers.get("X-Filename", "upload.jpg"))
            if Path(filename).suffix.lower() not in IMAGE_EXTS:
                self._send(400, {"error": "unsupported image type"})
                return
            body = self._read_body()
            if not body:
                self._send(400, {"error": "empty upload"})
                return
            dest = _unique_inbox_dest(self.state.cfg, filename)
            dest.write_bytes(body)
            log.info("Uploaded %s to the inbox", dest.name)
            self._send(200, {"ok": True, "queued": dest.name})

        def _post_edit(self, row_str: str):
            if not row_str.isdigit():
                self._send(400, {"error": "invalid row"})
                return
            row = int(row_str)
            current = dict((n, r) for n, r in self.state.store.list_rows()).get(row)
            if current is None:
                self._send(404, {"error": f"row {row} not found"})
                return
            data = self._read_json()

            def _num(value):
                try:
                    return float(value)
                except (TypeError, ValueError):
                    return None

            updated = ReceiptData(
                date=data.get("date") or current.date,
                currency=data.get("currency") or current.currency,
                amount=_num(data.get("amount")),
                merchant=data.get("merchant") or current.merchant,
                source_file=current.source_file,
                raw_text=current.raw_text,
                needs_review=False,  # manually reviewed via the GUI
            )
            try:
                self.state.store.update_row(row, updated)
            except FileLockedError:
                self._send(423, {"error": "receipts.xlsx is open in Excel — close it and retry"})
                return
            self._send(200, {"ok": True})

    return ApiHandler


def _load_engines_and_watch(state: AppState):
    from .pipeline import build_engines  # deferred: heavy import
    local, cloud = build_engines(state.cfg)
    state.cloud_engine = cloud
    state.watcher = WatcherService(state.cfg, state.store, local, cloud)
    state.watcher.start()
    state.engines_ready = True
    log.info("OCR engines ready — watcher is running")


def main(port: int):
    logging.getLogger().addHandler(_ActivityHandler())

    state = AppState()
    state.cfg = load_config()
    state.store = ExcelStore(state.cfg["excel_path"])

    server = ThreadingHTTPServer(("127.0.0.1", port), _make_handler(state))
    server.daemon_threads = True

    # Heavy model loading happens after the server is already answering /api/health
    threading.Thread(target=_load_engines_and_watch, args=(state,), daemon=True).start()

    log.info("Backend API listening on http://127.0.0.1:%d", port)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        if state.watcher:
            state.watcher.stop()
        server.server_close()
