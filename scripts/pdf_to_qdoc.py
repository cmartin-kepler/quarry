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
    # extra_attrs=["upright"] splits words by orientation and tags each with
    # `upright` (False for rotated/vertical text — e.g. figure axis labels or
    # the attention-heatmap tokens in ML papers). pdfplumber emits rotated glyphs
    # in reversed order, so flagging them lets the detectors discount the region
    # instead of trusting garbled "cells".
    for w in page.extract_words(
        use_text_flow=False, keep_blank_chars=False, extra_attrs=["upright"]
    ):
        span = {
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
        if not w.get("upright", True):
            span["rotated"] = True  # omitted when upright (defaults to false)
        spans.append(span)
    return spans


def _lightness(c) -> float:
    """Approximate lightness in [0,1] (1=white). Handles gray, RGB, and CMYK —
    these financial PDFs use CMYK, so the 4-tuple branch is essential."""
    if c is None:
        return 1.0
    if isinstance(c, (int, float)):
        return float(c)
    if len(c) == 3:
        return 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
    if len(c) == 4:  # CMYK
        return 1.0 - min(1.0, c[3] + (c[0] + c[1] + c[2]) / 3)
    return 1.0


def figure_score(page, bbox) -> float:
    """Fraction of a region covered by DARK/saturated filled rectangles — the
    signature of a chart's bars or an infographic's colored boxes. Real tables
    have at most a small dark header band (~7%); bar charts / infographics run
    17–21%. Pale table row-shading is light, so it doesn't count. This is the
    vector signal that tells a chart (full of numbers) from a table."""
    x0, y0, x1, y1 = bbox
    area = (x1 - x0) * (y1 - y0)
    if area <= 0:
        return 0.0
    dark = 0.0
    for r in page.rects:
        cx, cy = (r["x0"] + r["x1"]) / 2, (r["top"] + r["bottom"]) / 2
        if not (x0 <= cx <= x1 and y0 <= cy <= y1):
            continue
        if r.get("fill") and _lightness(r.get("non_stroking_color")) < 0.55:
            dark += (r["x1"] - r["x0"]) * (r["bottom"] - r["top"])
    return round(min(1.0, dark / area), 3)


def detect_regions(page) -> list[dict]:
    regions = []
    for t in page.find_tables():
        x0, top, x1, bottom = t.bbox
        regions.append(
            {
                "bbox": [round(x0, 2), round(top, 2), round(x1, 2), round(bottom, 2)],
                "note": "auto-detected (pdfplumber ruled lines)",
                "figure_score": figure_score(page, (x0, top, x1, bottom)),
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


def convert(pdf_path: str, regions_path: str | None, detect: bool, max_pages: int | None) -> dict:
    explicit = load_explicit_regions(regions_path) if regions_path else {}
    pages_out = []
    with pdfplumber.open(pdf_path) as pdf:
        pages = pdf.pages if max_pages is None else pdf.pages[:max_pages]
        for page in pages:
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
    ap.add_argument("--max-pages", type=int, help="cap pages (for very large PDFs)")
    args = ap.parse_args()

    doc = convert(args.pdf, args.regions, detect=not args.no_detect, max_pages=args.max_pages)
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
