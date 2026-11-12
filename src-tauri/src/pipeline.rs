//! Processing pipeline: image -> OCR -> parse -> Excel row -> move file.
//! Port of python-edition app/pipeline.py.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cloud::CloudVisionEngine;
use crate::config::Config;
use crate::models::ReceiptData;
use crate::ocr::LocalOcrEngine;
use crate::parser::parse_receipt;
use crate::store::{ExcelStore, StoreError};

pub const IMAGE_EXTS: [&str; 7] = [".jpg", ".jpeg", ".png", ".bmp", ".webp", ".tif", ".tiff"];

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .map(|e| {
            let ext = format!(".{}", e.to_string_lossy().to_lowercase());
            IMAGE_EXTS.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

pub enum ProcessOutcome {
    /// Saved + moved (or moved to errors) — nothing left to do.
    Done,
    /// Excel was locked: the row is returned and the image stays in place
    /// so the caller can retry the save later.
    ExcelLocked(ReceiptData),
}

/// Cloud result wins for every field it provides.
fn merge(local: ReceiptData, cloud: ReceiptData) -> ReceiptData {
    let mut merged = ReceiptData {
        date: cloud.date.or(local.date),
        merchant: cloud.merchant.or(local.merchant),
        amount: cloud.amount.or(local.amount),
        tax: cloud.tax.or(local.tax),
        currency: if cloud.currency.is_empty() {
            local.currency
        } else {
            cloud.currency
        },
        raw_text: if cloud.raw_text.is_empty() {
            local.raw_text
        } else {
            cloud.raw_text
        },
        confidence: local.confidence,
        needs_review: false,
        engine: "cloud".into(),
    };
    merged.needs_review = merged.amount.is_none() || merged.date.is_none();
    merged
}

fn move_with_timestamp(src: &Path, dest_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let mut dest = dest_dir.join(format!("{stamp}_{}", file_name(src)));
    let mut counter = 1;
    while dest.exists() {
        dest = dest_dir.join(format!("{stamp}_{counter}_{}", file_name(src)));
        counter += 1;
    }
    if std::fs::rename(src, &dest).is_err() {
        // cross-device fallback
        std::fs::copy(src, &dest)?;
        std::fs::remove_file(src)?;
    }
    Ok(dest)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn move_to_errors(src: &Path, cfg: &Config, error: &str) {
    let dest = match move_with_timestamp(src, &cfg.errors) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("could not move {} to errors: {e}", file_name(src));
            return;
        }
    };
    let log_path = dest.with_extension("txt");
    let _ = std::fs::write(
        &log_path,
        format!("File: {}\nError: {}\n", file_name(src), error),
    );
}

/// Process one image end-to-end. See [`ProcessOutcome`].
pub fn process_image(
    path: &Path,
    cfg: &Config,
    store: &ExcelStore,
    local: &LocalOcrEngine,
    cloud: Option<&CloudVisionEngine>,
) -> ProcessOutcome {
    let mut receipt = match local.extract(path) {
        Ok(raw) => Some(parse_receipt(
            &raw.text,
            raw.avg_confidence,
            &cfg.default_currency,
            cfg.min_avg_confidence,
        )),
        Err(e) => {
            println!("Local OCR failed on {}: {e}", file_name(path));
            None
        }
    };

    let use_cloud = cloud.is_some()
        && match &receipt {
            None => true,
            Some(r) => {
                r.needs_review
                    && cloud.map(|c| c.fallback_enabled()).unwrap_or(true)
            }
        };
    if use_cloud {
        match cloud.unwrap().extract(path) {
            Ok(cloud_receipt) => {
                receipt = Some(match receipt {
                    Some(local_receipt) => merge(local_receipt, cloud_receipt),
                    None => cloud_receipt,
                });
            }
            Err(e) => println!("Cloud OCR failed on {}: {e}", file_name(path)),
        }
    }

    let receipt = match receipt {
        Some(r) => r,
        None => {
            // Both engines failed -> errors folder
            move_to_errors(path, cfg, "OCR produced no result (local and cloud)");
            return ProcessOutcome::Done;
        }
    };

    match store.append(receipt.clone()) {
        Ok(()) => {}
        // Excel was locked — keep the image in the inbox for retry.
        Err(StoreError::FileLocked(msg)) => {
            println!("{msg}. Keeping {} in the inbox for retry.", file_name(path));
            return ProcessOutcome::ExcelLocked(receipt);
        }
        // Unexpected store error: keep the image in place for the next retry.
        Err(e) => {
            println!("Failed to save {}: {e}", file_name(path));
            return ProcessOutcome::ExcelLocked(receipt);
        }
    }

    if let Ok(dest) = move_with_timestamp(path, &cfg.processed) {
        let review = if receipt.needs_review { " [NEEDS REVIEW]" } else { "" };
        println!("Done: {} -> {}{}", file_name(path), file_name(&dest), review);
    }
    ProcessOutcome::Done
}

/// Retry saving receipts that previously failed because Excel was locked.
/// Successfully saved entries are removed from `pending`.
pub fn retry_pending(
    pending: &mut Vec<(ReceiptData, PathBuf)>,
    cfg: &Config,
    store: &ExcelStore,
) {
    let mut still_pending = Vec::new();
    for (receipt, src) in pending.drain(..) {
        match store.append(receipt.clone()) {
            Ok(()) => {
                let _ = move_with_timestamp(&src, &cfg.processed);
                println!("Saved delayed receipt for {}", file_name(&src));
            }
            Err(StoreError::FileLocked(_)) => still_pending.push((receipt, src)),
            Err(_) => still_pending.push((receipt, src)),
        }
    }
    *pending = still_pending;
}

/// Wait until the file size is unchanged across two checks (copy finished).
pub fn wait_until_stable(path: &Path, interval: Duration, timeout: Duration) -> bool {
    let start = Instant::now();
    let mut last_size: i64 = -1;
    let mut stable = 0;
    while start.elapsed() < timeout {
        let size = match std::fs::metadata(path) {
            Ok(m) => m.len() as i64,
            Err(_) => return false,
        };
        if size == last_size && size > 0 {
            stable += 1;
            if stable >= 2 {
                return true;
            }
        } else {
            stable = 0;
        }
        last_size = size;
        std::thread::sleep(interval);
    }
    path.exists()
}
