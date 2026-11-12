//! Shared data types. Port of python-edition app/models.py.

use serde::{Deserialize, Serialize};

/// One receipt row (a spreadsheet row minus the TOTAL row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptData {
    pub date: Option<String>,
    /// Place / merchant name.
    pub merchant: Option<String>,
    pub amount: Option<f64>,
    /// Internal only (not stored in the 4-column spreadsheet).
    #[serde(skip)]
    pub tax: Option<f64>,
    #[serde(rename = "currency")]
    pub currency: String,
    #[serde(skip)]
    pub raw_text: String,
    #[serde(skip)]
    pub confidence: Option<f32>,
    pub needs_review: bool,
    #[serde(skip)]
    pub engine: String,
}

impl ReceiptData {
    pub fn empty() -> Self {
        Self {
            date: None,
            merchant: None,
            amount: None,
            tax: None,
            currency: "CNY".into(),
            raw_text: String::new(),
            confidence: None,
            needs_review: true,
            engine: String::new(),
        }
    }
}
