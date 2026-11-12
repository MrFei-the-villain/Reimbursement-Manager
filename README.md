# Reimbursement Manager

Snap receipt photos into a folder — OCR pulls out **Date, Currency, Amount, Place** and appends
them to `receipts.xlsx` with a live TOTAL row. The bundled GUI lets you review, edit, and chart
spending, and launch your company's reimbursement site with one click.

Two editions live in this repository:

| Edition | Path | Backend | Notes |
| --- | --- | --- | --- |
| **Rust edition** (this folder) | `src-tauri/` + `ui/` | Pure Rust in-process (ocrs OCR, rust_xlsxwriter store, notify watcher) | No Python, no sidecar — one self-contained installer |
| **Python edition** (original) | [`python-edition/`](python-edition/) | FastAPI-style HTTP backend + RapidOCR, spawned by the Tauri shell | Kept for reference; see its own README |

## Download

Grab `Reimbursement.Manager_<version>_x64-setup.exe` from the
[Releases](../../releases) page and run it (per-user install, Windows 10/11).

## How it works

1. Drop receipt photos (jpg/png/webp/bmp/tif) onto the app window or into the `inbox` folder.
2. The watcher picks each image up, runs on-device OCR (no cloud calls by default) and the
   parser extracts the four fields — totals win over line items, subtotals are ignored.
3. Rows are appended to `receipts.xlsx` (red highlight = needs review); the file is then moved
   to `processed/` (failures land in `errors/` with a log).
4. Edit any cell in the GUI and press **Save changes** to write it back to the workbook.
5. **Open reimbursement site** launches the URL configured in Settings.

Optional: enable cloud OCR fallback (any OpenAI-compatible vision endpoint) in Settings for
low-confidence scans.

## Data folder

`%APPDATA%\ReimbursementManager` for installed builds (project directory when run from source):
`config.json`, `receipts.xlsx`, `inbox/`, `processed/`, `errors/`. Set `REIMBURSE_DATA_DIR` to
override.

## Build from source

```powershell
npm install
npm run tauri build
```

The OCR models (`resources/ocr/*.onnx`) are bundled automatically; the installer is emitted to
`src-tauri/target/release/bundle/nsis/`.

## Tests

```powershell
cd src-tauri
cargo test
# Full OCR roundtrip against a real receipt photo:
$env:OCR_TEST_IMAGE = "path\to\receipt.jpg"; cargo test -- --ignored --nocapture
```
