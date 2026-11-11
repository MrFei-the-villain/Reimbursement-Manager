"""Abstract OCR engine interface."""
from abc import ABC, abstractmethod
from pathlib import Path

from .models import ReceiptData


class OCREngine(ABC):
    """An engine that turns a receipt image into a ReceiptData (raw or parsed)."""

    name: str = "base"

    @abstractmethod
    def extract(self, image_path: Path) -> ReceiptData:
        """Process the image and return a ReceiptData.

        Raises an exception when the image cannot be processed at all.
        """
