#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber"]
# ///
"""Count text-layer words inside given page regions — the cheap signal for "does
this image region want OCR?". A docling Figure region whose bbox has ~no words under
it is a rasterized/scanned sub-image (OCR target); one with words is a diagram whose
labels are already extractable (skip OCR).

Input  : argv[1] = pdf path; argv[2] = JSON [{page, bbox:[x0,y0,x1,y1]}] (1-based
         page, top-left points — Quarry's convention).
Output : JSON [word_count] aligned to the input order.
"""
import json
import sys

import pdfplumber


def main():
    pdf_path, regions = sys.argv[1], json.loads(sys.argv[2])
    counts = []
    with pdfplumber.open(pdf_path) as pdf:
        for r in regions:
            page = pdf.pages[r["page"] - 1]
            x0, y0, x1, y1 = r["bbox"]
            lo_x, hi_x = sorted((x0, x1))
            lo_y, hi_y = sorted((y0, y1))
            box = (
                max(0.0, lo_x),
                max(0.0, lo_y),
                min(float(page.width), hi_x),
                min(float(page.height), hi_y),
            )
            try:
                counts.append(len(page.crop(box, strict=False).extract_words() or []))
            except Exception:
                counts.append(-1)  # un-croppable → don't flag (treated as "has text")
    print(json.dumps(counts))


if __name__ == "__main__":
    main()
