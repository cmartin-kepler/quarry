#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdf"]
# ///
"""LiteParse sidecar for the Rust `text-table` extractor.

Crop a PDF page to the region bbox, run LiteParse (`lit`) on the crop, and emit
`{text, words:[{text,x0,y0,x1,y1}]}` to stdout — word boxes translated back into
ORIGINAL PAGE coordinates (top-left, points) so the crate's cell anchors resolve
to the source.

Usage: litparse_region.py <pdf> <page> <x0> <y0> <x1> <y1>   (bbox top-left points)
"""
import json
import os
import subprocess
import sys
import tempfile

from pypdf import PdfReader, PdfWriter
from pypdf.generic import RectangleObject


def main() -> None:
    pdf, page = sys.argv[1], int(sys.argv[2])
    x0, y0, x1, y1 = map(float, sys.argv[3:7])

    reader = PdfReader(pdf)
    pg = reader.pages[page - 1]
    height = float(pg.mediabox.height)
    # top-left bbox -> PDF bottom-left crop box
    rect = RectangleObject([max(0.0, x0), max(0.0, height - y1), x1, height - y0])
    pg.cropbox = rect
    pg.mediabox = rect
    writer = PdfWriter()
    writer.add_page(pg)
    tmp = os.path.join(tempfile.mkdtemp(), "crop.pdf")
    with open(tmp, "wb") as f:
        writer.write(f)

    out = tmp + ".json"
    subprocess.run(["lit", "parse", tmp, "--format", "json", "-o", out, "-q"],
                   check=True, capture_output=True)
    p = json.load(open(out))["pages"][0]
    # lit reports crop-relative top-left coords; shift by the crop origin (x0, y0)
    words = [
        {
            "text": it["text"],
            "x0": x0 + it["x"],
            "y0": y0 + it["y"],
            "x1": x0 + it["x"] + it.get("width", 0.0),
            "y1": y0 + it["y"] + it.get("height", 0.0),
        }
        for it in p.get("textItems", [])
    ]
    print(json.dumps({"text": p.get("text", ""), "words": words}))


if __name__ == "__main__":
    main()
