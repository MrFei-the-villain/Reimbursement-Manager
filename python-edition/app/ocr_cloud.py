"""Cloud OCR engine using an OpenAI-compatible vision chat-completions API."""
import base64
import json
import logging
from pathlib import Path

import requests

from .models import ReceiptData
from .ocr_base import OCREngine

log = logging.getLogger(__name__)

_MIME_TYPES = {
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".png": "image/png",
    ".webp": "image/webp",
    ".bmp": "image/bmp",
    ".tif": "image/tiff",
    ".tiff": "image/tiff",
}

_SYSTEM_PROMPT = (
    "You are a receipt data extraction engine. Extract structured data from "
    "the receipt image and respond ONLY with a JSON object with these keys: "
    '"date" (YYYY-MM-DD or null), "merchant" (string or null), '
    '"amount" (total paid, number or null), "tax" (number or null), '
    '"currency" (ISO code such as CNY/USD/EUR or null), '
    '"category" (one of: Meal, Transport, Accommodation, Office Supplies, '
    "Communication, Entertainment, Other), "
    '"raw_text" (full text you can read on the receipt). '
    "Use null for fields you cannot determine. Respond with JSON only."
)


class CloudVisionEngine(OCREngine):
    name = "cloud"

    def __init__(self, settings: dict):
        self.base_url = str(settings.get("base_url", "")).rstrip("/")
        self.api_key = settings.get("api_key", "")
        self.model = settings.get("model", "gpt-4o-mini")
        self.timeout = int(settings.get("timeout_seconds", 60))

    def extract(self, image_path: Path) -> ReceiptData:
        data = image_path.read_bytes()
        b64 = base64.b64encode(data).decode("ascii")
        mime = _MIME_TYPES.get(image_path.suffix.lower(), "image/jpeg")

        payload = {
            "model": self.model,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": _SYSTEM_PROMPT},
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Extract the receipt data from this image."},
                        {"type": "image_url",
                         "image_url": {"url": f"data:{mime};base64,{b64}"}},
                    ],
                },
            ],
        }
        resp = requests.post(
            f"{self.base_url}/chat/completions",
            headers={"Authorization": f"Bearer {self.api_key}"},
            json=payload,
            timeout=self.timeout,
        )
        resp.raise_for_status()
        content = resp.json()["choices"][0]["message"]["content"]
        fields = json.loads(content)

        def _num(value):
            if value is None or value == "":
                return None
            try:
                return float(value)
            except (TypeError, ValueError):
                return None

        return ReceiptData(
            date=fields.get("date") or None,
            merchant=fields.get("merchant") or None,
            category=fields.get("category") or "",
            amount=_num(fields.get("amount")),
            tax=_num(fields.get("tax")),
            currency=fields.get("currency") or "CNY",
            raw_text=fields.get("raw_text") or "",
            engine="cloud",
        )
