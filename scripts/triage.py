#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "Pillow"]
# ///
"""Stage-0 page triage (doc-architecture.md / doc-build-order.md Phase 1).

Classify each page cheaply, no ML: text | image_content | blank.
  - text-presence  = pypdfium2 native char count (~2ms/page — vs ~30-65ms for
                     pdfplumber chars/words, which made the gate a net loss).
  - blank vs content = spatial complexity (stddev) of a ~40dpi thumbnail, rendered
                     ONLY on low-text pages (the minority), via pypdfium2.

  text          : has a usable text layer            -> parse (docling)
  image_content : ~no text but the page has content  -> ImageRef{OcrDeferred}
  blank         : flat thumbnail (no content)         -> skip

Emits JSON [{page, width, height, chars, stddev, klass}] for the Rust router.
Usage: triage.py <pdf> [--pages 1,2,..] [--out f.json]
"""
import argparse
import json

import pypdfium2 as pdfium
from PIL import ImageStat

CHARS_TEXT = 150   # >= this many chars (~30 words) => text page (skip the render)
CHARS_LOW = 25     # < this many chars => effectively no text layer
EPS_STDDEV = 5.0   # thumbnail stddev below this => blank / uniform decorative
THUMB_SCALE = 40 / 72.0  # ~40 dpi


def classify(page):
    n = page.get_textpage().count_chars()
    w, h = page.get_size()
    dims = {"width": round(w, 1), "height": round(h, 1), "chars": n}
    if n >= CHARS_TEXT:
        return {**dims, "stddev": None, "klass": "text"}

    # low/no text layer: render a small thumbnail and measure spatial complexity
    img = page.render(scale=THUMB_SCALE).to_pil().convert("L")
    sd = round(ImageStat.Stat(img).stddev[0], 1)
    if sd < EPS_STDDEV:
        klass = "blank"
    elif n < CHARS_LOW:
        klass = "image_content"  # no usable text + visual content -> OCR target
    else:
        klass = "text"  # sparse but has a real (small) text layer -> parse it
    return {**dims, "stddev": sd, "klass": klass}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pdf")
    ap.add_argument("--pages", help="comma list of 1-based pages (default: all)")
    ap.add_argument("--out")
    a = ap.parse_args()
    want = {int(x) for x in a.pages.split(",")} if a.pages else None

    doc = pdfium.PdfDocument(a.pdf)
    out = []
    for i in range(len(doc)):
        if want and (i + 1) not in want:
            continue
        out.append({"page": i + 1, **classify(doc[i])})

    s = json.dumps(out)
    if a.out:
        open(a.out, "w").write(s)
    else:
        print(s)


if __name__ == "__main__":
    main()
