# Reimbursement Manager

A Windows desktop app that turns receipt photos into a clean reimbursement spreadsheet — drop a receipt in, get **Date, Currency, Amount, Place** out, review it, and file your claim.

Built with **Tauri (Rust)** for the shell and a bundled **Python (RapidOCR)** backend for OCR.

## Features

- **Drop-and-scan** — drop receipt photos into the `inbox` folder (or the app window); each one is OCR-scanned and appended to `receipts.xlsx`
- **Smart total detection** — finds the real total at the bottom of the receipt, even when OCR scrambles the layout (subtotal lines are never mistaken for the total)
- **Editable table** — click any cell (Date / Currency / Amount / Place) to fix it, then hit *Save changes* to write back to the spreadsheet
- **Stats tab** — totals by currency, top places, and monthly spending grouped by currency
- **Reimbursement site button** — one click opens your company's expense portal (URL configurable in Settings)
- **Review flags** — low-confidence OCR rows are marked so you know what to double-check
- Fully local: receipts never leave your machine

## Getting started

### Install (recommended)

Download `Reimbursement Manager_1.0.0_x64-setup.exe` from the `dist` folder (or the v1.0.0 release) and run it.

### Using the app

1. Launch the app — it starts the OCR backend automatically
2. Drop receipt photos (`.jpg`, `.png`, …) into the `inbox` folder or onto the app
3. Review the parsed rows; edit any cell that needs fixing, then **Save changes**
4. Click **Reimbursement site ↗** and file your claim
5. Processed images land in the `processed` folder (unreadable ones in `errors`)

### Configuration

Settings live in `%APPDATA%\ReimbursementManager\config.json` (or edit in-app via the Settings tab):

| Setting | Purpose |
|---|---|
| `default_currency` | Used when a receipt doesn't state one |
| `reimbursement_url` | The site the "Reimbursement site" button opens |
| `cloud_ocr.*` | Optional cloud OCR API keys for higher accuracy |

Receipt data is written to `receipts.xlsx` in the same data folder.

## Building from source

Prerequisites: **Rust** (MSVC toolchain), **Node.js**, **Python 3.12+** with a venv.

```powershell
# 1. Python deps + backend binary
pip install -r requirements.txt
python -m PyInstaller backend.spec --noconfirm   # -> dist\backend\backend.exe

# 2. Frontend + app shell
npm install
npm run tauri build
```

Outputs: `src-tauri\target\release\reimbursement-manager.exe` and the NSIS installer under `src-tauri\target\release\bundle\nsis\`.

> **Note:** rustc needs some GB free on `C:` — a full system drive causes cryptic "metadata stub" build errors.

## Project layout

```
app/          Python backend: HTTP API, OCR pipeline, parser, xlsx store, folder watcher
ui/           Frontend (vanilla HTML/CSS/JS) served inside the Tauri window
src-tauri/    Rust shell: spawns backend.exe, exposes get_backend_port, window setup
dist/         Final standalone build (installer + portable)
```

## Tests

```powershell
python test_parser.py   # parser unit tests incl. scrambled-OCR regression cases
```
