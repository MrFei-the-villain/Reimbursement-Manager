"""Regex heuristics that turn raw OCR text into structured receipt fields."""
import re

from .models import ReceiptData

# Number like -1,234.56 or 2458.40 — guards prevent matching part of a longer
# digit run (e.g. "202" inside the year "2026") or of dates/IDs.
PLAIN_NUM_RE = re.compile(r"(?<![\d.,])-?\d+(?:,\d{3})*\.\d{1,2}(?![\d.])")
# Value prefixed by a currency symbol, e.g. ¥130.00 or $19.08
CURRENCY_NUM_RE = re.compile(r"[¥￥$€£]\s*(\d+(?:,\d{3})*(?:\.\d{1,2})?)(?![\d.])")

_TOTAL_KEYWORDS = re.compile(
    r"合计|总计|总额|实付|实收|应付|应收|金额|TOTAL|Total|Amount Due|amount due|Balance Due"
)
# 'Sub-total 2458.40' also contains 'total' — never let it win over the real total
_SUBTOTAL_EXCLUDE = re.compile(r"sub\s*-?\s*total|小计", re.IGNORECASE)
_TAX_KEYWORDS = re.compile(r"税额|税率|税|VAT|Tax|tax")
_TAX_EXCLUDE = re.compile(r"价税合计|不含税")
_TAX_RATE_RE = re.compile(r"(\d{1,2}(?:\.\d+)?)\s*%")

# yyyy[-/.]m[-/.]d (optionally with 日)
_DATE_YMD_RE = re.compile(r"(\d{4})\s*[年./\-]\s*(\d{1,2})\s*[月./\-]\s*(\d{1,2})\s*日?")
# d/m/yyyy or m/d/yyyy
_DATE_DMY_RE = re.compile(r"(\d{1,2})\s*[/.]\s*(\d{1,2})\s*[/.]\s*(\d{4})")

_CURRENCY_PATTERNS = [
    (re.compile(r"¥|￥|元|RMB"), "CNY"),
    (re.compile(r"\$|USD"), "USD"),
    (re.compile(r"€|EUR"), "EUR"),
    (re.compile(r"£|GBP"), "GBP"),
]

_MERCHANT_BLACKLIST = re.compile(
    r"发票|收银|收据|小票|订单|欢迎|会员|电话|地址|Ticket|Order|Invoice|"
    r"Receipt|Welcome|Tel|No\.?\s*$|^\s*No",
    re.IGNORECASE,
)


def _numbers_in(text: str):
    """All monetary values in a line, currency-tagged ones first."""
    values = []
    for m in CURRENCY_NUM_RE.finditer(text.replace("，", ",")):
        values.append(float(m.group(1).replace(",", "")))
    for m in PLAIN_NUM_RE.finditer(text.replace("，", ",")):
        values.append(float(m.group(0).replace(",", "")))
    return values


def _adjacent_values(lines, i):
    """Values on line i and its neighbours — OCR often splits the label
    ('合计：' or 'TOTAL') and the value ('¥130.00') onto separate lines.
    Same-line values come first in the list."""
    values = []
    for j in (i, i + 1, i - 1):
        if 0 <= j < len(lines):
            values.extend(_numbers_in(lines[j]))
    return values


def _find_amount(lines):
    """Prefer lines containing total keywords; fallback to the largest value."""
    for i, line in enumerate(lines):
        if _SUBTOTAL_EXCLUDE.search(line):
            continue
        if _TOTAL_KEYWORDS.search(line):
            values = _adjacent_values(lines, i)
            if values:
                return max(values)
    # Fallback: largest currency-tagged value, then largest plain value
    currency_values, plain_values = [], []
    for line in lines:
        line = line.replace("，", ",")
        currency_values += [float(m.group(1).replace(",", ""))
                            for m in CURRENCY_NUM_RE.finditer(line)]
        plain_values += [float(m.group(0).replace(",", ""))
                         for m in PLAIN_NUM_RE.finditer(line)]
    if currency_values:
        return max(currency_values)
    return max(plain_values) if plain_values else None


def _find_tax(lines, amount):
    for i, line in enumerate(lines):
        if not _TAX_KEYWORDS.search(line) or _TAX_EXCLUDE.search(line):
            continue
        for j in (i, i + 1, i - 1):
            if 0 <= j < len(lines) and not _TAX_EXCLUDE.search(lines[j]):
                values = _numbers_in(lines[j])
                if values:
                    return values[0]
        # A tax rate like 13% instead of an absolute value
        rate = _TAX_RATE_RE.search(line)
        if rate and amount:
            pct = float(rate.group(1))
            # Chinese VAT invoices: the total is tax-inclusive
            return round(amount - amount / (1 + pct / 100), 2)
    return None


def _find_date(lines):
    for line in lines:
        m = _DATE_YMD_RE.search(line)
        if m:
            y, mo, d = int(m.group(1)), int(m.group(2)), int(m.group(3))
            if 1 <= mo <= 12 and 1 <= d <= 31:
                return f"{y:04d}-{mo:02d}-{d:02d}", False
        m = _DATE_DMY_RE.search(line)
        if m:
            a, b, y = int(m.group(1)), int(m.group(2)), int(m.group(3))
            if a > 12 and b <= 12:      # clearly day-first
                return f"{y:04d}-{b:02d}-{a:02d}", False
            if b > 12 and a <= 12:      # clearly month-first
                return f"{y:04d}-{a:02d}-{b:02d}", False
            if a <= 12 and b <= 12:     # ambiguous -> guess month-first, flag
                return f"{y:04d}-{a:02d}-{b:02d}", True
    return None, False


def _find_currency(raw_text, default):
    for pattern, code in _CURRENCY_PATTERNS:
        if pattern.search(raw_text):
            return code
    return default


def _find_merchant(lines):
    for line in lines:
        if _MERCHANT_BLACKLIST.search(line):
            continue
        # needs at least 2 CJK chars or letters, not mostly digits
        word_chars = re.findall(r"[\u4e00-\u9fff a-zA-Z]", line)
        if len("".join(word_chars).replace(" ", "")) < 2:
            continue
        digits = sum(ch.isdigit() for ch in line)
        if digits > len(line) / 2:
            continue
        return line.strip()[:40]
    return None


def parse_receipt(raw_text: str, avg_score, cfg: dict) -> ReceiptData:
    """Fill a ReceiptData from raw OCR text (local engine output)."""
    lines = [l.strip() for l in raw_text.splitlines() if l.strip()]
    amount = _find_amount(lines)
    tax = _find_tax(lines, amount)
    date, date_ambiguous = _find_date(lines)
    merchant = _find_merchant(lines)
    currency = _find_currency(raw_text, cfg.get("default_currency", "CNY"))

    min_conf = cfg.get("local_ocr", {}).get("min_avg_confidence", 0.75)
    low_confidence = avg_score is not None and avg_score < min_conf

    needs_review = (
        amount is None
        or date is None
        or merchant is None
        or date_ambiguous
        or low_confidence
    )

    return ReceiptData(
        date=date,
        merchant=merchant,
        amount=amount,
        tax=tax,
        currency=currency,
        raw_text=raw_text,
        confidence=avg_score,
        needs_review=needs_review,
        engine="local",
    )
