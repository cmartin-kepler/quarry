#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "pillow", "doclayout-yolo", "ultralytics", "huggingface_hub"]
# ///
"""Warm per-page YOLO layout timing (model loaded once). For each page: render +
detect, record the time and the table-region bboxes (points). Pairs with
speed_tables.py for the three-pipeline speed comparison.

Usage: speed_yolo.py --pdf X.pdf --pages 50,51,... --out yolo.json
"""
import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pypdfium2 as pdfium  # noqa: E402
import yolo_layout  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--pdf", required=True)
ap.add_argument("--pages", required=True)
ap.add_argument("--model", default="yolo26n")
ap.add_argument("--out", required=True)
a = ap.parse_args()

pages = [int(x) for x in a.pages.split(",") if x.strip()]
doc = pdfium.PdfDocument(a.pdf)
SCALE = 2.0

# warm-up (loads the model) on the first page — excluded from timings
warm = doc[pages[0] - 1].render(scale=SCALE).to_pil()
yolo_layout.detect(warm, res=72 * SCALE, key=a.model)

out = []
for p in pages:
    t = time.perf_counter()
    pil = doc[p - 1].render(scale=SCALE).to_pil()
    els = yolo_layout.detect(pil, res=72 * SCALE, key=a.model)
    ms = (time.perf_counter() - t) * 1000  # render + detect = the region step
    tables = [e["bbox"] for e in els if e["label"].strip().lower() == "table"]
    out.append({"page": p, "yolo_ms": round(ms, 1), "tables": tables})

json.dump({"pages": out}, open(a.out, "w"))
print(f"yolo: timed {len(pages)} pages -> {a.out}")
