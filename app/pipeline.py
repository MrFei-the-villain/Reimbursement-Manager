"""Processing pipeline: image -> OCR -> parse -> Excel row -> move file."""
import logging
import shutil
import time
from pathlib import Path

from .excel import ExcelStore, FileLockedError
from .models import ReceiptData
from .ocr_cloud import CloudVisionEngine
from .ocr_local import LocalOCREngine
from .parser import parse_receipt

log = logging.getLogger(__name__)

IMAGE_EXTS = {".jpg", ".jpeg", ".png", ".bmp", ".webp", ".tif", ".tiff"}


def is_image(path: Path) -> bool:
    return path.suffix.lower() in IMAGE_EXTS


def build_engines(cfg: dict):
    """Return (local_engine, cloud_engine_or_None)."""
    local = LocalOCREngine()
    cloud = None
    cloud_cfg = cfg.get("cloud_ocr", {})
    if cloud_cfg.get("enabled"):
        cloud = CloudVisionEngine(cloud_cfg)
        log.info("Cloud OCR fallback enabled (model: %s)", cloud.model)
    return local, cloud


def _merge(local: ReceiptData, cloud: ReceiptData) -> ReceiptData:
    """Cloud result wins for every field it provides."""
    merged = ReceiptData(
        date=cloud.date or local.date,
        merchant=cloud.merchant or local.merchant,
        category=cloud.category or local.category,
        amount=cloud.amount if cloud.amount is not None else local.amount,
        tax=cloud.tax if cloud.tax is not None else local.tax,
        currency=cloud.currency or local.currency,
        source_file=local.source_file,
        raw_text=cloud.raw_text or local.raw_text,
        confidence=local.confidence,
        engine="cloud",
    )
    merged.needs_review = merged.amount is None or merged.date is None
    return merged


def _move_with_timestamp(src: Path, dest_dir: Path) -> Path:
    dest_dir.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d_%H%M%S")
    dest = dest_dir / f"{stamp}_{src.name}"
    counter = 1
    while dest.exists():
        dest = dest_dir / f"{stamp}_{counter}_{src.name}"
        counter += 1
    shutil.move(str(src), str(dest))
    return dest


def _move_to_errors(src: Path, cfg: dict, error: Exception):
    log.error("Failed to process %s: %s", src.name, error)
    dest = _move_with_timestamp(src, cfg["folders"]["errors"])
    log_path = dest.with_suffix(".txt")
    log_path.write_text(
        f"File: {src.name}\nError: {type(error).__name__}: {error}\n",
        encoding="utf-8",
    )
    log.error("Moved to %s (log: %s)", dest, log_path.name)


def process_image(path: Path, cfg: dict, store: ExcelStore,
                  local_engine: LocalOCREngine,
                  cloud_engine: CloudVisionEngine | None) -> ReceiptData | None:
    """Process one image end-to-end.

    Returns:
      - None on success or unrecoverable error (image already moved), or
      - the ReceiptData when the Excel file was locked (image kept in place so
        the caller can retry the save later).
    """
    receipt = None
    try:
        raw = local_engine.extract(path)
        receipt = parse_receipt(raw.raw_text, raw.confidence, cfg)
    except Exception as e:  # noqa: BLE001 - any OCR failure lands here
        log.warning("Local OCR failed on %s: %s", path.name, e)

    cloud_cfg = cfg.get("cloud_ocr", {})
    use_cloud = cloud_engine is not None and (
        receipt is None
        or (receipt.needs_review and cloud_cfg.get("fallback_on_low_confidence", True))
    )
    if use_cloud:
        try:
            cloud_receipt = cloud_engine.extract(path)
            receipt = _merge(receipt, cloud_receipt) if receipt else cloud_receipt
        except Exception as e:  # noqa: BLE001
            log.warning("Cloud OCR failed on %s: %s", path.name, e)

    if receipt is None:
        # Both engines failed -> errors folder
        _move_to_errors(path, cfg, RuntimeError("OCR produced no result (local and cloud)"))
        return None

    receipt.source_file = path.name
    try:
        store.append(receipt)
    except FileLockedError as e:
        log.warning("%s Keeping %s in the inbox for retry.", e, path.name)
        return receipt

    dest = _move_with_timestamp(path, cfg["folders"]["processed"])
    review = " [NEEDS REVIEW]" if receipt.needs_review else ""
    log.info("Done: %s -> %s%s", path.name, dest.name, review)
    return None


def retry_pending(pending: list, cfg: dict, store: ExcelStore):
    """Retry saving receipts that previously failed because Excel was locked.

    `pending` is a list of (ReceiptData, source_path) tuples; successfully
    saved entries are removed from the list.
    """
    still_pending = []
    for receipt, src in pending:
        try:
            store.append(receipt)
        except FileLockedError:
            still_pending.append((receipt, src))
            continue
        _move_with_timestamp(src, cfg["folders"]["processed"])
        log.info("Saved delayed receipt for %s", src.name)
    pending[:] = still_pending


def wait_until_stable(path: Path, interval: float = 1.0, timeout: float = 30.0) -> bool:
    """Wait until the file size is unchanged across two checks (copy finished)."""
    start = time.time()
    last_size = -1
    stable = 0
    while time.time() - start < timeout:
        try:
            size = path.stat().st_size
        except OSError:
            return False
        if size == last_size and size > 0:
            stable += 1
            if stable >= 2:
                return True
        else:
            stable = 0
        last_size = size
        time.sleep(interval)
    return path.exists()
