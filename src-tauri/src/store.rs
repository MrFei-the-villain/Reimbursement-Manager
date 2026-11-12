//! Excel-backed receipt store with totals row and lock detection.
//! Port of python-edition app/excel.py. Unlike the openpyxl original, every
//! save rewrites the whole workbook (rust_xlsxwriter) after reading the
//! current contents (calamine) — same observable behaviour.

use std::fmt;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;

use calamine::{Data, Reader, Xlsx};
use rust_xlsxwriter::{Format, FormatAlign, Formula, Workbook};

use crate::models::ReceiptData;

pub const SHEET_NAME: &str = "Receipts";
pub const HEADERS: [&str; 4] = ["Date", "Currency", "Amount", "Place"];
const WIDTHS: [f64; 4] = [14.0, 10.0, 12.0, 36.0];
const TOTAL_LABEL: &str = "TOTAL";

#[derive(Debug)]
pub enum StoreError {
    FileLocked(String),
    Io(std::io::Error),
    XlsxWrite(rust_xlsxwriter::XlsxError),
    Calamine(calamine::XlsxError),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::FileLocked(name) => write!(
                f,
                "'{name}' is locked — close it in Excel/WPS and it will be retried automatically."
            ),
            StoreError::Io(e) => write!(f, "{e}"),
            StoreError::XlsxWrite(e) => write!(f, "{e}"),
            StoreError::Calamine(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<rust_xlsxwriter::XlsxError> for StoreError {
    fn from(e: rust_xlsxwriter::XlsxError) -> Self {
        StoreError::XlsxWrite(e)
    }
}

impl From<calamine::XlsxError> for StoreError {
    fn from(e: calamine::XlsxError) -> Self {
        StoreError::Calamine(e)
    }
}

pub struct ExcelStore {
    pub path: PathBuf,
    lock: Mutex<()>,
}

/// (excel_row_number, data)
pub type RowList = Vec<(u32, ReceiptData)>;

impl ExcelStore {
    pub fn new(path: PathBuf) -> Result<Self, StoreError> {
        let store = Self { path, lock: Mutex::new(()) };
        if store.path.exists() {
            // An older layout (e.g. the Python edition's pre-migration file):
            // back it up and start a fresh 4-column sheet.
            if !store.layout_matches() {
                let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let backup = store.path.with_file_name(format!(
                    "receipts_legacy_{stamp}.xlsx"
                ));
                std::fs::copy(&store.path, &backup).map_err(StoreError::Io)?;
                store.rewrite(&[])?;
            }
        } else {
            store.rewrite(&[])?;
        }
        Ok(store)
    }

    fn layout_matches(&self) -> bool {
        let mut wb: Xlsx<_> = match calamine::open_workbook(&self.path) {
            Ok(wb) => wb,
            Err(_) => return true, // unreadable -> rewrite() will surface errors
        };
        match wb.worksheet_range(SHEET_NAME) {
            Ok(range) => {
                let headers: Vec<String> = (0..HEADERS.len() as u32)
                    .map(|c| Self::cell_str(&range, 0, c).unwrap_or_default())
                    .collect();
                headers == HEADERS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
            }
            Err(_) => false, // sheet missing -> different layout
        }
    }

    pub fn check_writable(&self) -> Result<(), StoreError> {
        if self.path.exists() {
            OpenOptions::new()
                .write(true)
                .open(&self.path)
                .map_err(|_| StoreError::FileLocked(self.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()))?;
        }
        Ok(())
    }

    // ------------------------------------------------------------- reading

    fn read_range(&self) -> calamine::Range<Data> {
        let mut wb: Xlsx<_> = match calamine::open_workbook(&self.path) {
            Ok(wb) => wb,
            Err(_) => return calamine::Range::empty(),
        };
        wb.worksheet_range(SHEET_NAME).unwrap_or_default()
    }

    fn cell_str(range: &calamine::Range<Data>, row: u32, col: u32) -> Option<String> {
        match range.get_value((row, col)) {
            Some(Data::String(s)) => Some(s.clone()),
            Some(Data::Float(f)) => Some(fmt_amount(*f)),
            Some(Data::Int(i)) => Some(i.to_string()),
            Some(Data::DateTime(dt)) => Some(dt.to_string()),
            _ => None,
        }
    }

    fn cell_f64(range: &calamine::Range<Data>, row: u32, col: u32) -> Option<f64> {
        match range.get_value((row, col)) {
            Some(Data::Float(f)) => Some(*f),
            Some(Data::Int(i)) => Some(*i as f64),
            Some(Data::String(s)) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Return [(excel_row_number, ReceiptData), ...] excluding the TOTAL row.
    pub fn list_rows(&self) -> RowList {
        if !self.path.exists() {
            return Vec::new();
        }
        let _guard = self.lock.lock().unwrap();
        self.list_rows_unlocked()
    }

    /// Caller must hold `lock`.
    fn list_rows_unlocked(&self) -> RowList {
        let range = self.read_range();
        let mut rows = Vec::new();
        for row in 1..range.height() as u32 {
            let date = Self::cell_str(&range, row, 0);
            if date.as_deref() == Some(TOTAL_LABEL) {
                continue;
            }
            let place = Self::cell_str(&range, row, 3);
            let currency = Self::cell_str(&range, row, 1);
            let amount = Self::cell_f64(&range, row, 2);
            let data = ReceiptData {
                needs_review: date.as_deref().map(str::is_empty).unwrap_or(true)
                    || place.as_deref().map(str::is_empty).unwrap_or(true)
                    || amount.is_none()
                    || currency.as_deref().map(str::is_empty).unwrap_or(true),
                date: date.filter(|s| !s.is_empty()),
                merchant: place.filter(|s| !s.is_empty()),
                currency: currency.unwrap_or_default(),
                amount,
                ..ReceiptData::empty()
            };
            rows.push((row + 1, data)); // calamine row index is 0-based; sheet row is 1-based
        }
        rows
    }

    // ------------------------------------------------------------- writing

    /// Rewrite the workbook from a full row list.
    fn rewrite(&self, rows: &[ReceiptData]) -> Result<(), StoreError> {
        self.check_writable()?;
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name(SHEET_NAME)?;

        let header_fmt = Format::new()
            .set_bold()
            .set_font_color("#FFFFFF")
            .set_background_color("#4472C4")
            .set_align(FormatAlign::Center);
        let review_fmt = Format::new().set_background_color("#FFC7CE");
        let review_amount_fmt = Format::new().set_background_color("#FFC7CE").set_num_format("0.00");
        let total_fmt = Format::new()
            .set_bold()
            .set_background_color("#D9E1F2")
            .set_num_format("0.00");
        let amount_fmt = Format::new().set_num_format("0.00");

        for (col, (header, width)) in HEADERS.iter().zip(WIDTHS).enumerate() {
            ws.write_with_format(0, col as u16, *header, &header_fmt)?;
            ws.set_column_width(col as u16, width)?;
        }
        ws.set_freeze_panes(1, 0)?;

        for (i, r) in rows.iter().enumerate() {
            let row = (i + 1) as u32;
            let cells: [Option<&str>; 4] = [
                r.date.as_deref(),
                Some(r.currency.as_str()),
                None, // amount handled separately (number format)
                r.merchant.as_deref(),
            ];
            for (col, v) in cells.iter().enumerate() {
                if let Some(text) = v {
                    if r.needs_review {
                        ws.write_with_format(row, col as u16, *text, &review_fmt)?;
                    } else {
                        ws.write(row, col as u16, *text)?;
                    }
                } else if r.needs_review && col != 2 {
                    ws.write_with_format(row, col as u16, "", &review_fmt)?;
                }
            }
            if let Some(amount) = r.amount {
                if r.needs_review {
                    ws.write_with_format(row, 2, amount, &review_amount_fmt)?;
                } else {
                    ws.write_with_format(row, 2, amount, &amount_fmt)?;
                }
            }
        }

        if !rows.is_empty() {
            // 0-based sheet row for the TOTAL row (header=0, data=1..n)
            let total_row = (rows.len() + 1) as u32;
            // 1-based Excel row of the last data row (header is row 1)
            let last_data_row = rows.len() + 1;
            ws.write_with_format(total_row, 0, TOTAL_LABEL, &total_fmt)?;
            ws.write_with_format(
                total_row,
                2,
                Formula::new(format!("=SUM(C2:C{last_data_row})")),
                &total_fmt,
            )?;
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        wb.save(&self.path)?;
        Ok(())
    }

    // ------------------------------------------------------- data mutation

    fn merge_and_write(&self, f: impl FnOnce(&mut Vec<ReceiptData>)) -> Result<(), StoreError> {
        let _guard = self.lock.lock().unwrap();
        let mut rows: Vec<ReceiptData> = self.list_rows_unlocked().into_iter().map(|(_, d)| d).collect();
        f(&mut rows);
        self.rewrite(&rows)
    }

    pub fn append(&self, r: ReceiptData) -> Result<(), StoreError> {
        self.merge_and_write(|rows| rows.push(r))
    }

    /// `row` is the 1-based spreadsheet row (header = row 1).
    pub fn update_row(&self, row: u32, r: ReceiptData) -> Result<(), StoreError> {
        self.merge_and_write(|rows| {
            let idx = (row as usize).checked_sub(2);
            if let Some(i) = idx {
                if i < rows.len() {
                    rows[i] = r;
                }
            }
        })
    }
}

fn fmt_amount(f: f64) -> String {
    if (f - f.round()).abs() < f64::EPSILON {
        format!("{}", f.round() as i64)
    } else {
        format!("{f}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> ExcelStore {
        let dir = std::env::temp_dir().join(format!("rm_store_test_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ExcelStore::new(dir.join("receipts.xlsx")).unwrap()
    }

    #[test]
    fn append_update_roundtrip() {
        let store = tmp_store("roundtrip");
        let mut r = ReceiptData::empty();
        r.date = Some("2026-06-02".into());
        r.merchant = Some("Yangtze".into());
        r.amount = Some(2458.40);
        r.currency = "CNY".into();
        store.append(r.clone()).unwrap();

        let rows = store.list_rows();
        assert_eq!(rows.len(), 1);
        let (row, d) = &rows[0];
        assert_eq!(*row, 2);
        assert_eq!(d.date.as_deref(), Some("2026-06-02"));
        assert_eq!(d.amount, Some(2458.40));
        assert!(!d.needs_review);

        let mut r2 = r.clone();
        r2.amount = Some(99.5);
        r2.needs_review = false;
        store.update_row(2, r2).unwrap();
        let rows = store.list_rows();
        assert_eq!(rows[0].1.amount, Some(99.5));
        assert_eq!(rows.len(), 1); // TOTAL row not listed
    }

    #[test]
    fn missing_fields_need_review() {
        let store = tmp_store("review");
        let mut r = ReceiptData::empty();
        r.currency = "CNY".into();
        store.append(r).unwrap();
        let rows = store.list_rows();
        assert!(rows[0].1.needs_review);
    }
}
