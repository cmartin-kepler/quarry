#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber"]
# ///
"""Stage-0 page triage (doc-architecture.md / doc-build-order.md Phase 1).

Classify each page cheaply, no ML: text | image_content | blank, from word_count,
image_area_fraction, and the spatial complexity (stddev) of a small grayscale
thumbnail. The render (~10ms) runs ONLY on low-text pages, so text-rich pages are
near-free.

  text          : has a usable text layer        -> parse (docling + litparse)
  image_content : ~no text, image-dominant, has structure -> ImageRef{OcrDeferred}
  blank         : flat thumbnail (no content)     -> skip

Emits JSON [{page, words, image_frac, stddev, klass}] for the Rust router.
Usage: triage.py <pdf> [--pages 1,2,..] [--out f.json]
"""
import argparse
import json

import pdfplumber
from PIL import ImageStat

W_TEXT = 30        # >= this many words => definitely a text page (skip the render)
W_LOW = 5          # < this many words => effectively no text layer
EPS_STDDEV = 5.0   # thumbnail stddev below this => blank / uniform decorative
IMG_FRAC = 0.5     # image-area fraction above this => image-dominant


def classify(page):
    words = len(page.extract_words() or [])
    pa = (page.width or 0) * (page.height or 0)
    iarea = sum(
        max(0.0, im["x1"] - im["x0"]) * max(0.0, im["bottom"] - im["top"])
        for im in (page.images or [])
    )
    frac = round(iarea / pa, 3) if pa else 0.0

    if words >= W_TEXT:
        return {"words": words, "image_frac": frac, "stddev": None, "klass": "text"}

    # low/no text layer: render a small thumbnail and measure spatial complexity
    img = page.to_image(resolution=40).original.convert("L")
    sd = round(ImageStat.Stat(img).stddev[0], 1)
    if sd < EPS_STDDEV:
        klass = "blank"
    elif words < W_LOW and frac >= IMG_FRAC:
        klass = "image_content"
    else:
        klass = "text"  # sparse but has a real (small) text layer -> parse it
    return {"words": words, "image_frac": frac, "stddev": sd, "klass": klass}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pdf")
    ap.add_argument("--pages", help="comma list of 1-based pages (default: all)")
    ap.add_argument("--out")
    a = ap.parse_args()
    want = {int(x) for x in a.pages.split(",")} if a.pages else None

    out = []
    with pdfplumber.open(a.pdf) as pdf:
        for i, page in enumerate(pdf.pages, 1):
            if want and i not in want:
                continue
            row = {"page": i, **classify(page)}
            out.append(row)

    s = json.dumps(out)
    if a.out:
        open(a.out, "w").write(s)
    else:
        print(s)


if __name__ == "__main__":
    main()
