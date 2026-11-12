# -*- mode: python ; coding: utf-8 -*-
"""PyInstaller spec: bundles the Python backend as a windowless onedir app."""
from PyInstaller.utils.hooks import collect_data_files

# rapidocr ships its ONNX models + config yaml as package data
rapidocr_data = collect_data_files("rapidocr")

a = Analysis(
    ["backend_main.py"],
    pathex=[],
    binaries=[],
    datas=rapidocr_data,
    hiddenimports=[
        "onnxruntime",
        "cv2",
        "shapely",
        "pyclipper",
        "yaml",
        "watchdog.observers.read_directory_changes",
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
)

pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name="backend",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    console=False,
    icon="src-tauri/icons/icon.ico",
)

coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=False,
    name="backend",
)
