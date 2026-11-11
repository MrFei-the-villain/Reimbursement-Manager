"""Configuration loading with defaults, folder creation and path resolution."""
import json
import os
import sys
from pathlib import Path


def _resolve_base_dir() -> Path:
    """Where the app keeps its data (config.json, folders, receipts.xlsx).

    Order: REIMBURSE_DATA_DIR env var -> PyInstaller frozen exe
    (%APPDATA%\\ReimbursementManager) -> running from source (project dir).
    """
    env = os.environ.get("REIMBURSE_DATA_DIR")
    if env:
        base = Path(env).resolve()
        base.mkdir(parents=True, exist_ok=True)
        return base
    if getattr(sys, "frozen", False):
        appdata = os.environ.get("APPDATA") or str(Path.home() / "AppData" / "Roaming")
        base = Path(appdata) / "ReimbursementManager"
        base.mkdir(parents=True, exist_ok=True)
        return base
    return Path(__file__).resolve().parent.parent


BASE_DIR = _resolve_base_dir()
CONFIG_PATH = BASE_DIR / "config.json"

DEFAULT_CONFIG = {
    "folders": {
        "inbox": "./inbox",
        "processed": "./processed",
        "errors": "./errors",
    },
    "excel_path": "./receipts.xlsx",
    "default_currency": "CNY",
    "reimbursement_url": "",
    "local_ocr": {
        "min_avg_confidence": 0.75,
    },
    "cloud_ocr": {
        "enabled": False,
        "fallback_on_low_confidence": True,
        "base_url": "https://api.openai.com/v1",
        "api_key": "",
        "model": "gpt-4o-mini",
        "timeout_seconds": 60,
    },
}


def _merge(default: dict, loaded: dict) -> dict:
    """Deep-merge loaded config over defaults so new keys are always present."""
    out = dict(default)
    for key, value in loaded.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


def load_config() -> dict:
    """Load config.json (creating it from defaults on first run), resolve all
    relative paths against the project directory and create the folders."""
    if CONFIG_PATH.exists():
        with open(CONFIG_PATH, "r", encoding="utf-8") as f:
            loaded = json.load(f)
        cfg = _merge(DEFAULT_CONFIG, loaded)
    else:
        cfg = json.loads(json.dumps(DEFAULT_CONFIG))  # deep copy
        with open(CONFIG_PATH, "w", encoding="utf-8") as f:
            json.dump(cfg, f, indent=2, ensure_ascii=False)

    # Resolve relative paths against the project directory
    for name in ("inbox", "processed", "errors"):
        cfg["folders"][name] = (BASE_DIR / cfg["folders"][name]).resolve()
    cfg["excel_path"] = (BASE_DIR / cfg["excel_path"]).resolve()

    # Create working folders
    for name in ("inbox", "processed", "errors"):
        cfg["folders"][name].mkdir(parents=True, exist_ok=True)

    # Cloud OCR is only usable when an API key is configured
    cloud = cfg["cloud_ocr"]
    if cloud.get("enabled") and not cloud.get("api_key"):
        cloud["enabled"] = False

    return cfg
