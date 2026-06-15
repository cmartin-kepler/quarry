#!/usr/bin/env python3
"""
pdf_to_qdoc.py - Bridge a real PDF into Quarry's `.qdoc` text-layer format.

Quarry's extractors consume `.qdoc` (positioned text spans + marked table
regions), a stand-in for "the original bytes". This bridge produces that from an
actual born-digital PDF using pdfplumber, so the cheap extractor and the eval
harness can run against real or synthetic (LaTeX/reportlab) PDFs.

Coordinates: pdfplumber reports points with a top-left origin, which is exactly
the `.qdoc` convention -- a word's (x0, top, x1, bottom) maps straight to
bbox = [x0, y0, x1, y1]. No flipping or unit conversion needed.

Table regions:
  - default: pdfplumber's ruled-line table detection (page.find_tables()).
    The region only SCOPES which spans the cheap extractor reconstructs; the
    naive reconstruction (and its failure modes) stays in Quarry, so the eval
    still measures something real.
  - --regions FILE: explicit [[page,x0,y0,x1,y1], ...] to override detection
    (used by generators that already know where their tables are).
  - --no-detect: emit spans only, no regions.

Usage (Python deps via uv: run `uv sync` once at the repo root):
  uv run scripts/pdf_to_qdoc.py input.pdf -o out.qdoc
  uv run scripts/pdf_to_qdoc.py input.pdf -o out.qdoc --regions regions.json
"""
from __future__ import annotations

import argparse
import json
import sys

import pdfplumber


def words_to_spans(page) -> list[dict]:
    spans = []
    for w in page.extract_words(use_text_flow=False, keep_blank_chars=False):
        spans.append(
            {
                "text": w["text"],
                "bbox": [
                    round(float(w["x0"]), 2),
                    round(float(w["top"]), 2),
                    round(float(w["x1"]), 2),
                    round(float(w["bottom"]), 2),
                ],
                # pdfplumber has no per-word confidence on born-digital text;
                # 1.0 == digital. An OCR front-end would populate this.
                "confidence": 1.0,
            }
        )
    return spans


def detect_regions(page) -> list[dict]:
    regions = []
    for t in page.find_tables():
        x0, top, x1, bottom = t.bbox
        regions.append(
            {
                "bbox": [round(x0, 2), round(top, 2), round(x1, 2), round(bottom, 2)],
                "note": "auto-detected (pdfplumber ruled lines)",
            }
        )
    return regions


def load_explicit_regions(path: str) -> dict[int, list[dict]]:
    by_page: dict[int, list[dict]] = {}
    for row in json.load(open(path)):
        page, x0, y0, x1, y1 = row
        by_page.setdefault(int(page), []).append(
            {"bbox": [x0, y0, x1, y1], "note": "explicit"}
        )
    return by_page


def convert(pdf_path: str, regions_path: str | None, detect: bool) -> dict:
    explicit = load_explicit_regions(regions_path) if regions_path else {}
    pages_out = []
    with pdfplumber.open(pdf_path) as pdf:
        for page in pdf.pages:
            pno = page.page_number  # 1-based
            if explicit:
                regions = explicit.get(pno, [])
            elif detect:
                regions = detect_regions(page)
            else:
                regions = []
            pages_out.append(
                {
                    "page": pno,
                    "width": round(float(page.width), 2),
                    "height": round(float(page.height), 2),
                    "spans": words_to_spans(page),
                    "table_regions": regions,
                }
            )
    return {"format": "pdf", "pages": pages_out}


def main():
    ap = argparse.ArgumentParser(description="Convert a PDF to Quarry .qdoc format.")
    ap.add_argument("pdf")
    ap.add_argument("-o", "--out", required=True, help="output .qdoc path")
    ap.add_argument("--regions", help="explicit regions JSON: [[page,x0,y0,x1,y1],...]")
    ap.add_argument("--no-detect", action="store_true", help="do not auto-detect tables")
    args = ap.parse_args()

    doc = convert(args.pdf, args.regions, detect=not args.no_detect)
    with open(args.out, "w") as fh:
        json.dump(doc, fh, indent=2)

    n_spans = sum(len(p["spans"]) for p in doc["pages"])
    n_regions = sum(len(p["table_regions"]) for p in doc["pages"])
    print(
        f"{args.pdf} -> {args.out}: "
        f"{len(doc['pages'])} page(s), {n_spans} span(s), {n_regions} table region(s)",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
