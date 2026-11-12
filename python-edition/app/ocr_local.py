"""Local OCR engine based on RapidOCR (bundled PP-OCR Chinese+English models)."""
import logging
from pathlib import Path

from .models import ReceiptData
from .ocr_base import OCREngine

log = logging.getLogger(__name__)


def _extract_texts_scores(result):
    """Return (texts, scores) from a RapidOCR result.

    Supports both the new (>=2.0) RapidOCROutput object and the legacy
    list-of-[box, text, score] format.
    """
    if result is None:
        return [], []
    if hasattr(result, "txts"):
        texts = list(result.txts or [])
        scores = list(getattr(result, "scores", None) or [])
        return texts, scores
    texts, scores = [], []
    for item in result:
        # legacy format: [box, text, score]
        texts.append(item[1])
        scores.append(item[2] if len(item) > 2 else None)
    return texts, scores


class LocalOCREngine(OCREngine):
    name = "local"

    def __init__(self):
        from rapidocr import RapidOCR  # deferred: model load takes a few seconds
        self._engine = RapidOCR()

    def extract(self, image_path: Path) -> ReceiptData:
        result = self._engine(str(image_path))
        texts, scores = _extract_texts_scores(result)
        raw_text = "\n".join(texts)
        numeric_scores = [s for s in scores if s is not None]
        avg = sum(numeric_scores) / len(numeric_scores) if numeric_scores else None
        return ReceiptData(raw_text=raw_text, confidence=avg, engine="local")
