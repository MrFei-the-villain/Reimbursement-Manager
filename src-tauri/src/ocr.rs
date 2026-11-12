//! Local OCR engine based on ocrs (pure Rust) with bundled ONNX models.
//! Replaces the Python edition's RapidOCR engine; output contract is the same:
//! newline-joined text plus the average per-character recognition confidence.

use std::path::Path;

use ocrs::{ImageSource, OcrEngine, OcrEngineParams};
use rten::Model;

pub struct LocalOcrEngine {
    engine: OcrEngine,
}

pub struct OcrOutput {
    pub text: String,
    pub avg_confidence: Option<f32>,
}

impl LocalOcrEngine {
    /// `det_path`/`rec_path` point at text-detection.onnx / text-recognition.onnx.
    pub fn new(det_path: &Path, rec_path: &Path) -> Result<Self, String> {
        let detection = Model::load_file(det_path)
            .map_err(|e| format!("loading detection model: {e}"))?;
        let recognition = Model::load_file(rec_path)
            .map_err(|e| format!("loading recognition model: {e}"))?;
        let engine = OcrEngine::new(OcrEngineParams {
            detection_model: Some(detection),
            recognition_model: Some(recognition),
            ..Default::default()
        })
        .map_err(|e| format!("initialising OCR engine: {e}"))?;
        Ok(Self { engine })
    }

    pub fn extract(&self, image_path: &Path) -> Result<OcrOutput, String> {
        let img = image::open(image_path).map_err(|e| format!("opening image: {e}"))?;
        let rgb = img.into_rgb8();
        let source = ImageSource::from_bytes(rgb.as_raw(), (rgb.width(), rgb.height()))
            .map_err(|e| format!("preparing image: {e}"))?;
        let input = self
            .engine
            .prepare_input(source)
            .map_err(|e| format!("preparing image: {e}"))?;
        let words = self
            .engine
            .detect_words(&input)
            .map_err(|e| format!("text detection: {e}"))?;
        let lines = self.engine.find_text_lines(&input, &words);
        let recognized = self
            .engine
            .recognize_text(&input, &lines)
            .map_err(|e| format!("text recognition: {e}"))?;

        let mut texts = Vec::new();
        for line in recognized.into_iter().flatten() {
            texts.push(line.to_string());
        }
        // ocrs 0.14 does not expose per-character confidences, so avg is None
        // (the parser only treats confidence as "low" when it is available).
        Ok(OcrOutput {
            text: texts.join("\n"),
            avg_confidence: None,
        })
    }
}

/// Full OCR -> parse -> Excel roundtrip against a real receipt photo.
/// Run with: OCR_TEST_IMAGE=<path-to-receipt> cargo test -- --ignored
#[test]
#[ignore]
fn ocr_full_roundtrip() {
    let img = std::env::var("OCR_TEST_IMAGE").expect("set OCR_TEST_IMAGE");
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let engine = LocalOcrEngine::new(
        &manifest.join("resources/ocr/text-detection.onnx"),
        &manifest.join("resources/ocr/text-recognition.onnx"),
    )
    .unwrap();
    let out = engine.extract(std::path::Path::new(&img)).unwrap();
    println!("---- OCR text ----\n{}", out.text);
    let data = crate::parser::parse_receipt(&out.text, out.avg_confidence, "CNY", 0.75);
    println!("---- parsed ----\n{data:?}");
    assert!(data.amount.is_some(), "amount should be found");

    let dir = std::env::temp_dir().join(format!("rm_ocr_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = crate::store::ExcelStore::new(dir.join("receipts.xlsx")).unwrap();
    store.append(data).unwrap();
    let rows = store.list_rows();
    println!("---- store rows ----\n{rows:?}");
    assert_eq!(rows.len(), 1);
}
