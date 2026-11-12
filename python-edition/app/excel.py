"""Excel-backed receipt store (openpyxl) with totals row and lock detection."""
import logging
import threading
from pathlib import Path

from openpyxl import Workbook, load_workbook
from openpyxl.styles import Alignment, Font, PatternFill

from .models import ReceiptData

log = logging.getLogger(__name__)

SHEET_NAME = "Receipts"
HEADERS = ["Date", "Currency", "Amount", "Place"]
_WIDTHS = [14, 10, 12, 36]

_HEADER_FILL = PatternFill("solid", fgColor="4472C4")
_HEADER_FONT = Font(bold=True, color="FFFFFF")
_REVIEW_FILL = PatternFill("solid", fgColor="FFC7CE")
_TOTAL_FONT = Font(bold=True)
_TOTAL_FILL = PatternFill("solid", fgColor="D9E1F2")

# Column indexes (1-based)
_C_DATE, _C_CUR, _C_AMOUNT, _C_PLACE = range(1, 5)
_TOTAL_LABEL = "TOTAL"


class FileLockedError(Exception):
    """The Excel file is locked, e.g. currently open in Excel/WPS."""


class ExcelStore:
    def __init__(self, path: Path):
        self.path = Path(path)
        self._lock = threading.RLock()
        if not self.path.exists():
            self._create_new()
        else:
            try:
                self._migrate_if_needed()
            except PermissionError:
                log.warning("%s is locked — schema migration will retry on the "
                            "next save.", self.path.name)

    # ---------------------------------------------------------------- setup

    def _create_new(self):
        wb = Workbook()
        ws = wb.active
        ws.title = SHEET_NAME
        for col, (header, width) in enumerate(zip(HEADERS, _WIDTHS), start=1):
            c = ws.cell(1, col, header)
            c.font = _HEADER_FONT
            c.fill = _HEADER_FILL
            c.alignment = Alignment(horizontal="center")
            ws.column_dimensions[c.column_letter].width = width
        ws.freeze_panes = "A2"
        wb.save(self.path)

    @staticmethod
    def _layout_matches(ws) -> bool:
        headers = [ws.cell(1, c).value for c in range(1, ws.max_column + 1)]
        return headers[:len(HEADERS)] == HEADERS and len(headers) == len(HEADERS)

    def _migrate_if_needed(self):
        """Rewrite an older sheet (e.g. the previous 11-column layout) so its rows
        keep only the four output fields."""
        try:
            wb = load_workbook(self.path)
        except Exception:  # noqa: BLE001 - unreadable file, start clean
            self._create_new()
            return
        ws = wb[SHEET_NAME] if SHEET_NAME in wb.sheetnames else wb.active
        if self._layout_matches(ws):
            wb.close()
            return
        old_cols = {
            str(ws.cell(1, c).value): c
            for c in range(1, ws.max_column + 1) if ws.cell(1, c).value
        }

        def _cell(row, *names):
            for name in names:
                if name in old_cols:
                    return ws.cell(row, old_cols[name]).value
            return None

        data = []
        for row in range(2, ws.max_row + 1):
            date = _cell(row, "Date")
            place = _cell(row, "Place", "Merchant")
            if date == _TOTAL_LABEL or place == _TOTAL_LABEL:
                continue
            amount = _cell(row, "Amount")
            if date or place or amount is not None:
                data.append((date, _cell(row, "Currency"), amount, place))
        wb.close()

        self._create_new()
        if not data:
            return
        wb = load_workbook(self.path)
        ws = wb[SHEET_NAME]
        for row, (date, currency, amount, place) in enumerate(data, start=2):
            self._write_values(ws, row, date, currency, amount, place)
        wb.save(self.path)
        wb.close()
        log.info("Migrated %s to the %d-column output layout.",
                 self.path.name, len(HEADERS))

    def _open(self):
        self._check_writable()
        wb = load_workbook(self.path)
        ws = wb[SHEET_NAME]
        if not self._layout_matches(ws):
            wb.close()
            self._migrate_if_needed()
            wb = load_workbook(self.path)
        return wb

    def _check_writable(self):
        if self.path.exists():
            try:
                with open(self.path, "r+b"):
                    pass
            except PermissionError:
                raise FileLockedError(
                    f"'{self.path.name}' is locked — close it in Excel/WPS and "
                    "it will be retried automatically."
                )

    # ------------------------------------------------------------- totals

    @staticmethod
    def _strip_total_row(ws):
        for r in range(ws.max_row, 1, -1):
            if ws.cell(r, _C_DATE).value == _TOTAL_LABEL:
                ws.delete_rows(r)
                return

    @staticmethod
    def _add_total_row(ws):
        last_data_row = ws.max_row
        if last_data_row < 2:
            return  # no data yet
        r = last_data_row + 1
        label = ws.cell(r, _C_DATE, _TOTAL_LABEL)
        total = ws.cell(r, _C_AMOUNT, f"=SUM(C2:C{last_data_row})")
        total.number_format = "0.00"
        for c in (label, total):
            c.font = _TOTAL_FONT
            c.fill = _TOTAL_FILL
        ws.row_dimensions[r].height = None

    # ------------------------------------------------------------- writing

    @staticmethod
    def _write_values(ws, row: int, date, currency, amount, place):
        ws.cell(row, _C_DATE, date or "")
        ws.cell(row, _C_CUR, currency or "")
        amount_cell = ws.cell(row, _C_AMOUNT, amount if amount is not None else "")
        amount_cell.number_format = "0.00"
        ws.cell(row, _C_PLACE, place or "")

    def _write_row(self, ws, row: int, r: ReceiptData):
        self._write_values(ws, row, r.date, r.currency, r.amount, r.merchant)
        if r.needs_review:
            for col in range(1, len(HEADERS) + 1):
                ws.cell(row, col).fill = _REVIEW_FILL

    def append(self, r: ReceiptData):
        with self._lock:
            wb = self._open()
            ws = wb[SHEET_NAME]
            self._strip_total_row(ws)
            row = ws.max_row + 1
            self._write_row(ws, row, r)
            self._add_total_row(ws)
            wb.save(self.path)
            wb.close()
        log.info("Appended receipt #%d (%s, %s %s) to %s",
                 row - 1, r.merchant, r.amount, r.currency, self.path.name)

    def update_row(self, row: int, r: ReceiptData):
        with self._lock:
            wb = self._open()
            ws = wb[SHEET_NAME]
            # clear any previous review highlight on this row
            for col in range(1, len(HEADERS) + 1):
                ws.cell(row, col).fill = PatternFill()
                ws.cell(row, col).font = Font()
            self._write_row(ws, row, r)
            self._strip_total_row(ws)
            self._add_total_row(ws)
            wb.save(self.path)
            wb.close()

    # ------------------------------------------------------------- reading

    def list_rows(self):
        """Return [(excel_row_number, ReceiptData), ...] excluding the TOTAL row."""
        if not self.path.exists():
            return []
        with self._lock:
            wb = load_workbook(self.path, read_only=False)
            ws = wb[SHEET_NAME]
            rows = []
            for row in range(2, ws.max_row + 1):
                date = ws.cell(row, _C_DATE).value
                if date == _TOTAL_LABEL:
                    continue

                def _num(v):
                    return float(v) if isinstance(v, (int, float)) else None

                place = ws.cell(row, _C_PLACE).value
                currency = ws.cell(row, _C_CUR).value
                amount = _num(ws.cell(row, _C_AMOUNT).value)
                rows.append((row, ReceiptData(
                    date=str(date) if date else None,
                    merchant=str(place) if place else None,
                    amount=amount,
                    currency=str(currency or ""),
                    needs_review=not date or not place or amount is None
                    or not currency,
                )))
            wb.close()
            return rows
