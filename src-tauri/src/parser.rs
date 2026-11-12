//! Regex heuristics that turn raw OCR text into structured receipt fields.
//! Port of python-edition app/parser.py.

use fancy_regex::Regex;
use std::sync::LazyLock;

use crate::models::ReceiptData;

pub static PLAIN_NUM_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Number like -1,234.56 or 2458.40 — guards prevent matching part of a
    // longer digit run (e.g. "202" inside the year "2026") or of dates/IDs.
    Regex::new(r"(?<![\d.,])-?\d+(?:,\d{3})*\.\d{1,2}(?![\d.])").unwrap()
});

pub static CURRENCY_NUM_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Value prefixed by a currency symbol, e.g. ¥130.00 or $19.08
    Regex::new(r#"[¥￥$€£]\s*(\d+(?:,\d{3})*(?:\.\d{1,2})?)(?![\d.])"#).unwrap()
});

static TOTAL_KEYWORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"合计|总计|总额|实付|实收|应付|应收|金额|TOTAL|Total|Amount Due|amount due|Balance Due").unwrap()
});
// 'Sub-total 2458.40' also contains 'total' — never let it win over the real total
static SUBTOTAL_EXCLUDE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)sub\s*-?\s*total|小计").unwrap());
static TAX_KEYWORDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"税额|税率|税|VAT|Tax|tax").unwrap());
static TAX_EXCLUDE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"价税合计|不含税").unwrap());
static TAX_RATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d{1,2}(?:\.\d+)?)\s*%").unwrap());

// yyyy[-/.]m[-/.]d (optionally with 日)
static DATE_YMD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{4})\s*[年./\-]\s*(\d{1,2})\s*[月./\-]\s*(\d{1,2})\s*日?").unwrap());
// d/m/yyyy or m/d/yyyy
static DATE_DMY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d{1,2})\s*[/.]\s*(\d{1,2})\s*[/.]\s*(\d{4})").unwrap());

static CURRENCY_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (Regex::new(r"¥|￥|元|RMB").unwrap(), "CNY"),
        (Regex::new(r"\$|USD").unwrap(), "USD"),
        (Regex::new(r"€|EUR").unwrap(), "EUR"),
        (Regex::new(r"£|GBP").unwrap(), "GBP"),
    ]
});

static MERCHANT_BLACKLIST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)发票|收银|收据|小票|订单|欢迎|会员|电话|地址|Ticket|Order|Invoice|Receipt|Welcome|Tel|No\.?\s*$|^\s*No",
    )
    .unwrap()
});

static WORD_CHARS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\u{4e00}-\u{9fff} a-zA-Z]").unwrap());

fn normalize(text: &str) -> String {
    text.replace('，', ",")
}

fn parse_plain(m: &str) -> f64 {
    m.replace(',', "").parse::<f64>().unwrap_or(0.0)
}

/// All monetary values in a line, currency-tagged ones first.
fn numbers_in(text: &str) -> Vec<f64> {
    let t = normalize(text);
    let mut values = Vec::new();
    for c in CURRENCY_NUM_RE.captures_iter(&t) {
        if let Some(m) = c.ok().and_then(|c| c.get(1).map(|m| m.as_str().to_string())) {
            values.push(parse_plain(&m));
        }
    }
    for m in PLAIN_NUM_RE.find_iter(&t).flatten() {
        values.push(parse_plain(m.as_str()));
    }
    values
}

/// Values on line i and its neighbours — OCR often splits the label
/// ('合计：' or 'TOTAL') and the value ('¥130.00') onto separate lines.
/// Same-line values come first in the list.
fn adjacent_values(lines: &[&str], i: usize) -> Vec<f64> {
    let mut values = Vec::new();
    for j in [Some(i), i.checked_add(1), i.checked_sub(1)].into_iter().flatten() {
        if j < lines.len() {
            values.extend(numbers_in(lines[j]));
        }
    }
    values
}

/// Prefer lines containing total keywords; fallback to the largest value.
fn find_amount(lines: &[&str]) -> Option<f64> {
    for (i, line) in lines.iter().enumerate() {
        if SUBTOTAL_EXCLUDE.is_match(line).unwrap_or(false) {
            continue;
        }
        if TOTAL_KEYWORDS.is_match(line).unwrap_or(false) {
            let values = adjacent_values(lines, i);
            if !values.is_empty() {
                return values.into_iter().fold(f64::MIN, f64::max).into();
            }
        }
    }
    // Fallback: largest currency-tagged value, then largest plain value
    let mut currency_values = Vec::new();
    let mut plain_values = Vec::new();
    for line in lines {
        let t = normalize(line);
        for c in CURRENCY_NUM_RE.captures_iter(&t).flatten() {
            if let Some(m) = c.get(1) {
                currency_values.push(parse_plain(m.as_str()));
            }
        }
        for m in PLAIN_NUM_RE.find_iter(&t).flatten() {
            plain_values.push(parse_plain(m.as_str()));
        }
    }
    currency_values
        .into_iter()
        .max_by(|a, b| a.partial_cmp(b).unwrap())
        .or_else(|| plain_values.into_iter().max_by(|a, b| a.partial_cmp(b).unwrap()))
}

fn find_tax(lines: &[&str], amount: Option<f64>) -> Option<f64> {
    for (i, line) in lines.iter().enumerate() {
        if !TAX_KEYWORDS.is_match(line).unwrap_or(false) || TAX_EXCLUDE.is_match(line).unwrap_or(false) {
            continue;
        }
        for j in [Some(i), i.checked_add(1), i.checked_sub(1)].into_iter().flatten() {
            if let Some(l) = lines.get(j) {
                if !TAX_EXCLUDE.is_match(l).unwrap_or(false) {
                    let values = numbers_in(l);
                    if !values.is_empty() {
                        return Some(values[0]);
                    }
                }
            }
        }
        // A tax rate like 13% instead of an absolute value
        if let Some(rate) = TAX_RATE_RE.captures(line).ok().flatten() {
            if let (Some(a), Some(pct_str)) = (amount, rate.get(1)) {
                if let Ok(pct) = pct_str.as_str().parse::<f64>() {
                    // Chinese VAT invoices: the total is tax-inclusive
                    let tax = a - a / (1.0 + pct / 100.0);
                    return Some((tax * 100.0).round() / 100.0);
                }
            }
        }
    }
    None
}

/// Returns (date, ambiguous).
fn find_date(lines: &[&str]) -> (Option<String>, bool) {
    for line in lines {
        if let Some(caps) = DATE_YMD_RE.captures(line).ok().flatten() {
            let y: i32 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            let mo: i32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            let d: i32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);
            if (1..=12).contains(&mo) && (1..=31).contains(&d) && y > 1900 {
                return (Some(format!("{y:04}-{mo:02}-{d:02}")), false);
            }
        }
        if let Some(caps) = DATE_DMY_RE.captures(line).ok().flatten() {
            let a: i32 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
            let b: i32 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            let y: i32 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);
            if a > 12 && (1..=12).contains(&b) {
                // clearly day-first
                return (Some(format!("{y:04}-{b:02}-{a:02}")), false);
            }
            if b > 12 && (1..=12).contains(&a) {
                // clearly month-first
                return (Some(format!("{y:04}-{a:02}-{b:02}")), false);
            }
            if (1..=12).contains(&a) && (1..=12).contains(&b) {
                // ambiguous -> guess month-first, flag
                return (Some(format!("{y:04}-{a:02}-{b:02}")), true);
            }
        }
    }
    (None, false)
}

fn find_currency(raw_text: &str, default: &str) -> String {
    for (pattern, code) in CURRENCY_PATTERNS.iter() {
        if pattern.is_match(raw_text).unwrap_or(false) {
            return code.to_string();
        }
    }
    default.to_string()
}

fn find_merchant(lines: &[&str]) -> Option<String> {
    for line in lines {
        if MERCHANT_BLACKLIST.is_match(line).unwrap_or(false) {
            continue;
        }
        // needs at least 2 CJK chars or letters, not mostly digits
        let word_chars: String = WORD_CHARS_RE
            .find_iter(line)
            .flatten()
            .map(|m| m.as_str())
            .collect();
        if word_chars.replace(' ', "").chars().count() < 2 {
            continue;
        }
        let digits = line.chars().filter(|c| c.is_ascii_digit()).count();
        if digits as f64 > line.chars().count() as f64 / 2.0 {
            continue;
        }
        let stripped = line.trim();
        // Truncate to 40 chars on a char boundary
        let out: String = stripped.chars().take(40).collect();
        return Some(out);
    }
    None
}

pub fn parse_receipt(
    raw_text: &str,
    avg_score: Option<f32>,
    default_currency: &str,
    min_conf: f32,
) -> ReceiptData {
    let lines: Vec<&str> = raw_text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let amount = find_amount(&lines);
    let tax = find_tax(&lines, amount);
    let (date, date_ambiguous) = find_date(&lines);
    let merchant = find_merchant(&lines);
    let currency = find_currency(raw_text, default_currency);

    let low_confidence = avg_score.is_some_and(|s| s < min_conf);
    let needs_review =
        amount.is_none() || date.is_none() || merchant.is_none() || date_ambiguous || low_confidence;

    ReceiptData {
        date,
        merchant,
        amount,
        tax,
        currency,
        raw_text: raw_text.to_string(),
        confidence: avg_score,
        needs_review,
        engine: "local".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, score: Option<f32>) -> ReceiptData {
        parse_receipt(text, score, "CNY", 0.75)
    }

    #[test]
    fn finds_simple_total_and_date() {
        let r = parse("Yangtze River Restaurant\n2026-06-02\n合计：¥130.00\n", Some(0.95));
        assert_eq!(r.amount, Some(130.0));
        assert_eq!(r.date.as_deref(), Some("2026-06-02"));
        assert_eq!(r.currency, "CNY");
        assert!(!r.needs_review);
    }

    #[test]
    fn multi_digit_total_beats_dish_price() {
        // Regression: scrambled RapidOCR output of a real receipt — the totals
        // block is split into label/value lines and only 3-digit numbers used
        // to be visible. The real total 2458.40 must win over 634.40.
        let text = "清蒸鲈鱼 634.40\nSub-total\n2458.40\n总计\nTotal RMB\n2458.40\n";
        let r = parse(text, Some(0.9));
        assert_eq!(r.amount, Some(2458.40));
    }

    #[test]
    fn subtotal_never_wins() {
        let r = parse("SUB-TOTAL 30.00\nTOTAL 50.00\n", None);
        assert_eq!(r.amount, Some(50.0));
    }

    #[test]
    fn usd_currency_symbol() {
        let r = parse("Coffee Shop\nTOTAL $19.08\n2026/05/01\n", Some(0.9));
        assert_eq!(r.currency, "USD");
        assert_eq!(r.amount, Some(19.08));
    }

    #[test]
    fn ambiguous_date_flagged() {
        let r = parse("Cafe\n05/06/2026\nTOTAL 10.00\n", Some(0.9));
        assert_eq!(r.date.as_deref(), Some("2026-05-06"));
        assert!(r.needs_review);
    }

    #[test]
    fn blacklisted_merchant_lines_skipped() {
        let r = parse("Invoice\nYangtze Restaurant\nTOTAL 20.00\n", Some(0.9));
        assert_eq!(r.merchant.as_deref(), Some("Yangtze Restaurant"));
    }

    #[test]
    fn low_confidence_needs_review() {
        let r = parse("Shop\n2026-01-01\nTOTAL 9.99\n", Some(0.5));
        assert!(r.needs_review);
    }
}
