#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "pillow", "doclayout-yolo", "ultralytics", "huggingface_hub"]
# ///
"""Corpus-wide warm YOLO pass: for every PDF under --dir, sample pages evenly,
time render+detect (model loaded once across the whole corpus), record table
regions. Feeds corpus_tables.py for the 4-option whole-document cost comparison.
"""
import argparse
import glob
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pypdfium2 as pdfium  # noqa: E402
import yolo_layout  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--dir", default="input")
ap.add_argument("--sample", type=int, default=12)
ap.add_argument("--model", default="yolo26n")
ap.add_argument("--out", required=True)
a = ap.parse_args()
SCALE = 2.0


def sample_pages(n, k):
    if k <= 0 or n <= k:  # k<=0 => ALL pages (whole-document cost, no extrapolation)
        return list(range(1, n + 1))
    return sorted({round(i * (n - 1) / (k - 1)) + 1 for i in range(k)})


pdfs = sorted(glob.glob(os.path.join(a.dir, "**", "*.pdf"), recursive=True))
docs = {}
warmed = False
for path in pdfs:
    try:
        doc = pdfium.PdfDocument(path)
        n = len(doc)
    except Exception as e:
        print(f"skip {path}: {e}")
        continue
    pages = sample_pages(n, a.sample)
    if not warmed:
        yolo_layout.detect(doc[pages[0] - 1].render(scale=SCALE).to_pil(), res=72 * SCALE, key=a.model)
        warmed = True
    sample = []
    for p in pages:
        t = time.perf_counter()
        pil = doc[p - 1].render(scale=SCALE).to_pil()
        els = yolo_layout.detect(pil, res=72 * SCALE, key=a.model)
        ms = (time.perf_counter() - t) * 1000
        tables = [e["bbox"] for e in els if e["label"].strip().lower() == "table"]
        sample.append({"page": p, "yolo_ms": round(ms, 1), "tables": tables})
    docs[path] = {"n_pages": n, "sample": sample}
    print(f"{os.path.basename(path):52} {n:>4}pg, sampled {len(pages)}")

json.dump(docs, open(a.out, "w"))
print(f"-> {a.out}")
