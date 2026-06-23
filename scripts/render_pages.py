#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "Pillow"]
# ///
"""Rasterize PDF pages to base64 PNGs for the side-by-side viewer.

Input  : argv[1] = pdf; --pages 1,2,.. (default all); --scale (px per point, ~1.5≈108dpi)
Output : JSON {page: {w, h, png}} — w/h are page size in POINTS (so the viewer can
         scale the extraction overlay to the same coordinate space), png is base64.
"""
import argparse
import base64
import io
import json

import pypdfium2 as pdfium


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pdf")
    ap.add_argument("--pages", help="comma list of 1-based pages")
    ap.add_argument("--scale", type=float, default=1.5)
    a = ap.parse_args()

    doc = pdfium.PdfDocument(a.pdf)
    want = [int(x) for x in a.pages.split(",") if x.strip()] if a.pages else list(range(1, len(doc) + 1))

    out = {}
    for p in want:
        page = doc[p - 1]
        w, h = page.get_size()
        pil = page.render(scale=a.scale).to_pil().convert("RGB")
        buf = io.BytesIO()
        pil.save(buf, format="PNG")
        out[str(p)] = {"w": round(w, 2), "h": round(h, 2), "png": base64.b64encode(buf.getvalue()).decode()}
    print(json.dumps(out))


if __name__ == "__main__":
    main()
