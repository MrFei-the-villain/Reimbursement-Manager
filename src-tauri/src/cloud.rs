//! Cloud OCR engine using an OpenAI-compatible vision chat-completions API.
//! Port of python-edition app/ocr_cloud.py.

use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::config::CloudOcrConfig;
use crate::models::ReceiptData;

const SYSTEM_PROMPT: &str = "You are a receipt data extraction engine. Extract structured data \
from the receipt image and respond ONLY with a JSON object with these keys: \
\"date\" (YYYY-MM-DD or null), \"merchant\" (string or null), \
\"amount\" (total paid, number or null), \"tax\" (number or null), \
\"currency\" (ISO code such as CNY/USD/EUR or null), \
\"category\" (one of: Meal, Transport, Accommodation, Office Supplies, \
Communication, Entertainment, Other), \
\"raw_text\" (full text you can read on the receipt). \
Use null for fields you cannot determine. Respond with JSON only.";

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => "image/jpeg",
    }
}

#[derive(Clone)]
pub struct CloudVisionEngine {
    cfg: CloudOcrConfig,
}

impl CloudVisionEngine {
    pub fn new(cfg: CloudOcrConfig) -> Self {
        Self { cfg }
    }

    pub fn fallback_enabled(&self) -> bool {
        self.cfg.fallback_on_low_confidence
    }

    pub fn extract(&self, image_path: &Path) -> Result<ReceiptData, String> {
        let data = std::fs::read(image_path).map_err(|e| format!("reading image: {e}"))?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        let ext = image_path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let mime = mime_for(&ext);

        let payload = json!({
            "model": self.cfg.model,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Extract the receipt data from this image."},
                        {"type": "image_url",
                         "image_url": {"url": format!("data:{mime};base64,{b64}")}},
                    ],
                },
            ],
        });

        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let api_key = self.cfg.api_key.clone();
        let timeout = Duration::from_secs(self.cfg.timeout_seconds);

        let body: Value = tauri::async_runtime::block_on(async {
            let client = reqwest::Client::new();
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&payload)
                .timeout(timeout)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let resp = resp
                .error_for_status()
                .map_err(|e| format!("API error: {e}"))?;
            resp.json::<Value>()
                .await
                .map_err(|e| format!("invalid API response: {e}"))
        })?;

        let content = body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("API response is missing message content")?;
        let fields: Value =
            serde_json::from_str(content).map_err(|e| format!("invalid JSON from model: {e}"))?;

        Ok(ReceiptData {
            date: str_field(&fields, "date"),
            merchant: str_field(&fields, "merchant"),
            amount: num_field(&fields, "amount"),
            tax: num_field(&fields, "tax"),
            currency: fields["currency"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("CNY")
                .to_string(),
            raw_text: fields["raw_text"].as_str().unwrap_or("").to_string(),
            confidence: None,
            needs_review: true,
            engine: "cloud".into(),
        })
    }
}

fn str_field(fields: &Value, key: &str) -> Option<String> {
    fields[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn num_field(fields: &Value, key: &str) -> Option<f64> {
    match &fields[key] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}
