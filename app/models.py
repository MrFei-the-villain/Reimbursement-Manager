"""Data model for a single receipt record."""
from dataclasses import dataclass, field


@dataclass
class ReceiptData:
    date: str | None = None          # normalized YYYY-MM-DD
    merchant: str | None = None
    category: str = ""
    amount: float | None = None      # total amount
    tax: float | None = None
    currency: str = "CNY"
    source_file: str = ""
    raw_text: str = ""
    confidence: float | None = None  # local OCR average score (0-1)
    needs_review: bool = False
    engine: str = "local"            # "local" or "cloud"
