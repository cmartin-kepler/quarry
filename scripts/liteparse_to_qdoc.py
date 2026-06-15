#!/usr/bin/env python3
"""
liteparse_to_qdoc.py - Bridge LiteParse JSON into Quarry's .qdoc text layer.

LiteParse (`lit parse <file> --format json`) emits a positioned text layer
(textItems: text + x/y/width/height + font + confidence) but NO table structure
— unlike Docling, which emits structured tables. So to compare table quality we
run LiteParse's tokens through Quarry's reconstructor: this bridge maps its
textItems onto .qdoc spans (they line up almost 1:1) and pulls table regions from
pdfplumber's ruled-line detection (LiteParse doesn't mark regions).

LiteParse's tokens carry OCR confidence, so this is also the path that would feed
scanned pages once an OCR tier is wanted.

Usage:
  lit parse doc.pdf --format json -o doc.liteparse.json
  uv run scripts/liteparse_to_qdoc.py doc.liteparse.json --pdf doc.pdf -o doc.qdoc
"""
from __future__ import annotations

import argparse
import json
import sys

import pdfplumber


def detect_regions(pdf_path: str) -> dict[int, list[dict]]:
    by_page: dict[int, list[dict]] = {}
    with pdfplumber.open(pdf_path) as pdf:
        for page in pdf.pages:
            regs = []
            for t in page.find_tables():
                x0, top, x1, bottom = t.bbox
                regs.append({"bbox": [round(x0, 2), round(top, 2), round(x1, 2), round(bottom, 2)],
                             "note": "auto-detected (pdfplumber)"})
            if regs:
                by_page[page.page_number] = regs
    return by_page


def convert(lite_path: str, pdf_path: str) -> dict:
    lite = json.load(open(lite_path))
    regions = detect_regions(pdf_path)
    pages_out = []
    for pg in lite["pages"]:
        spans = []
        for it in pg.get("textItems", []):
            if not it.get("text", "").strip():
                continue
            x, y, w, h = it["x"], it["y"], it["width"], it["height"]
            span = {
                "text": it["text"],
                "bbox": [round(x, 2), round(y, 2), round(x + w, 2), round(y + h, 2)],
                "confidence": float(it.get("confidence", 1.0)),
            }
            spans.append(span)
        pages_out.append({
            "page": pg["page"],
            "width": float(pg["width"]),
            "height": float(pg["height"]),
            "spans": spans,
            "table_regions": regions.get(pg["page"], []),
        })
    return {"format": "pdf", "pages": pages_out}


def main():
    ap = argparse.ArgumentParser(description="Convert LiteParse JSON to Quarry .qdoc.")
    ap.add_argument("liteparse_json")
    ap.add_argument("--pdf", required=True, help="source PDF (for table-region detection)")
    ap.add_argument("-o", "--out", required=True)
    args = ap.parse_args()

    doc = convert(args.liteparse_json, args.pdf)
    json.dump(doc, open(args.out, "w"), indent=2)
    n_spans = sum(len(p["spans"]) for p in doc["pages"])
    n_reg = sum(len(p["table_regions"]) for p in doc["pages"])
    print(f"{args.liteparse_json} -> {args.out}: {len(doc['pages'])} page(s), "
          f"{n_spans} span(s), {n_reg} table region(s)", file=sys.stderr)


if __name__ == "__main__":
    main()
