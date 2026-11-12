//! Configuration loading with defaults, folder creation and path resolution.
//! Port of python-edition app/config.py. The on-disk config.json keeps the
//! same shape (and relative folder paths) as the Python edition so both can
//! share a data directory.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const CONFIG_FILE: &str = "config.json";

#[derive(Debug, Clone)]
pub struct CloudOcrConfig {
    pub fallback_on_low_confidence: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub inbox: PathBuf,
    pub processed: PathBuf,
    pub errors: PathBuf,
    pub excel_path: PathBuf,
    pub default_currency: String,
    pub reimbursement_url: String,
    pub min_avg_confidence: f32,
    /// `None` when cloud OCR is disabled or has no API key.
    pub cloud: Option<CloudOcrConfig>,
}

/// Where the app keeps its data (config.json, folders, receipts.xlsx).
///
/// Order: REIMBURSE_DATA_DIR env var -> packaged exe
/// (%APPDATA%\ReimbursementManager) -> dev run (project dir).
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("REIMBURSE_DATA_DIR") {
        let base = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&base);
        return base;
    }
    if cfg!(debug_assertions) {
        // dev: the project root (parent of src-tauri, known at compile time)
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let base = PathBuf::from(appdata).join("ReimbursementManager");
        let _ = std::fs::create_dir_all(&base);
        base
    }
}

pub fn default_raw() -> Value {
    json!({
        "folders": {"inbox": "./inbox", "processed": "./processed", "errors": "./errors"},
        "excel_path": "./receipts.xlsx",
        "default_currency": "CNY",
        "reimbursement_url": "",
        "local_ocr": {"min_avg_confidence": 0.75},
        "cloud_ocr": {
            "enabled": false,
            "fallback_on_low_confidence": true,
            "base_url": "https://api.openai.com/v1",
            "api_key": "",
            "model": "gpt-4o-mini",
            "timeout_seconds": 60
        }
    })
}

/// Deep-merge `loaded` over `default` so new keys are always present.
pub fn deep_merge(default: &Value, loaded: &Value) -> Value {
    match (default, loaded) {
        (Value::Object(d), Value::Object(l)) => {
            let mut out = d.clone();
            for (k, v) in l {
                let merged = match out.get(k) {
                    Some(dv) if dv.is_object() && v.is_object() => deep_merge(dv, v),
                    _ => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        (_, loaded) => loaded.clone(),
    }
}

fn read_raw(base: &Path) -> Value {
    let path = base.join(CONFIG_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(loaded) = serde_json::from_str::<Value>(&text) {
            return deep_merge(&default_raw(), &loaded);
        }
    }
    // First run: create the file from defaults
    let cfg = default_raw();
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap_or_default());
    cfg
}

fn resolve_rel(base: &Path, rel: &str) -> PathBuf {
    let p = PathBuf::from(rel);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// Load config.json (creating it from defaults on first run), resolve all
/// relative paths against the data directory and create the folders.
pub fn load_config(base: &Path) -> Config {
    let raw = read_raw(base);
    let folders = raw.get("folders").cloned().unwrap_or_else(|| json!({}));

    let inbox = resolve_rel(base, folders["inbox"].as_str().unwrap_or("./inbox"));
    let processed = resolve_rel(base, folders["processed"].as_str().unwrap_or("./processed"));
    let errors = resolve_rel(base, folders["errors"].as_str().unwrap_or("./errors"));
    let excel_path = resolve_rel(base, raw["excel_path"].as_str().unwrap_or("./receipts.xlsx"));
    for dir in [&inbox, &processed, &errors] {
        let _ = std::fs::create_dir_all(dir);
    }

    let cloud_raw = &raw["cloud_ocr"];
    let mut cloud_enabled = cloud_raw["enabled"].as_bool().unwrap_or(false);
    let api_key = cloud_raw["api_key"].as_str().unwrap_or("").to_string();
    // Cloud OCR is only usable when an API key is configured
    if api_key.is_empty() {
        cloud_enabled = false;
    }
    let cloud = if cloud_enabled {
        Some(CloudOcrConfig {
            fallback_on_low_confidence: cloud_raw["fallback_on_low_confidence"]
                .as_bool()
                .unwrap_or(true),
            base_url: cloud_raw["base_url"]
                .as_str()
                .unwrap_or("https://api.openai.com/v1")
                .to_string(),
            api_key,
            model: cloud_raw["model"]
                .as_str()
                .unwrap_or("gpt-4o-mini")
                .to_string(),
            timeout_seconds: cloud_raw["timeout_seconds"].as_u64().unwrap_or(60),
        })
    } else {
        None
    };

    Config {
        inbox,
        processed,
        errors,
        excel_path,
        default_currency: raw["default_currency"]
            .as_str()
            .unwrap_or("CNY")
            .to_string(),
        reimbursement_url: raw["reimbursement_url"].as_str().unwrap_or("").to_string(),
        min_avg_confidence: raw["local_ocr"]["min_avg_confidence"]
            .as_f64()
            .unwrap_or(0.75) as f32,
        cloud,
    }
}

/// Config as exposed to the UI (same shape as the Python `GET /api/config`).
pub fn ui_config(cfg: &Config) -> Value {
    let cloud = cfg.cloud.as_ref();
    json!({
        "default_currency": cfg.default_currency,
        "reimbursement_url": cfg.reimbursement_url,
        "cloud_ocr": {
            "enabled": cloud.is_some(),
            "fallback_on_low_confidence": cloud.map(|c| c.fallback_on_low_confidence).unwrap_or(true),
            "base_url": cloud.map(|c| c.base_url.clone()).unwrap_or_default(),
            "api_key": cloud.map(|c| c.api_key.clone()).unwrap_or_default(),
            "model": cloud.map(|c| c.model.clone()).unwrap_or_default(),
            "timeout_seconds": cloud.map(|c| c.timeout_seconds).unwrap_or(60),
        },
        "excel_path": cfg.excel_path.display().to_string(),
        "folders": {
            "inbox": cfg.inbox.display().to_string(),
            "processed": cfg.processed.display().to_string(),
            "errors": cfg.errors.display().to_string(),
        },
    })
}

const CLOUD_KEYS: [&str; 6] = [
    "enabled",
    "fallback_on_low_confidence",
    "base_url",
    "api_key",
    "model",
    "timeout_seconds",
];

/// Persist a UI settings patch to config.json (same key restrictions as the
/// Python `POST /api/config`: only default_currency, reimbursement_url and
/// the cloud_ocr block are writable).
pub fn apply_ui_patch(base: &Path, patch: &Value) -> Result<(), String> {
    let path = base.join(CONFIG_FILE);
    let mut disk: Value = if let Ok(text) = std::fs::read_to_string(&path) {
        serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    if let Some(v) = patch.get("default_currency") {
        disk["default_currency"] = v.clone();
    }
    if let Some(v) = patch.get("reimbursement_url") {
        disk["reimbursement_url"] = v.clone();
    }
    if let Some(cloud_patch) = patch.get("cloud_ocr").filter(|v| v.is_object()) {
        for key in CLOUD_KEYS {
            if let Some(v) = cloud_patch.get(key) {
                disk["cloud_ocr"][key] = v.clone();
            }
        }
    }

    std::fs::write(&path, serde_json::to_string_pretty(&disk).unwrap_or_default())
        .map_err(|e| format!("could not write config.json: {e}"))
}
