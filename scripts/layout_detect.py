#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "pillow", "ultralytics", "huggingface_hub", "doclayout-yolo"]
# ///
"""Layout sidecar for the Rust `LayoutSidecar` extractor.

Render a PDF page and run a layout model, emitting
`{regions:[{label,confidence,bbox:[x0,y0,x1,y1]}]}` in PAGE points (top-left) — the
shape the crate's `sidecar::regions_from_json` adapter consumes.

Usage: layout_detect.py <model> <pdf> <page>   (model: yolo26 | doclayout)
"""
import json
import os
import sys

import pypdfium2 as pdfium

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import yolo_layout  # noqa: E402  (the prototype's pluggable YOLO layout detectors)


def main() -> None:
    model, pdf, page = sys.argv[1], sys.argv[2], int(sys.argv[3])
    scale = 2.0
    pg = pdfium.PdfDocument(pdf)[page - 1]
    pil = pg.render(scale=scale).to_pil()
    elements = yolo_layout.detect(pil, res=72.0 * scale, key=model)
    regions = [
        {"label": e["label"], "confidence": e.get("conf", 1.0), "bbox": e["bbox"]}
        for e in elements
    ]
    print(json.dumps({"regions": regions}))


if __name__ == "__main__":
    main()
