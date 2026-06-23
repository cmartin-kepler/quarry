#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "Pillow", "rapidocr-onnxruntime", "numpy"]
# ///
"""Actually OCR page regions — fulfils the OcrDeferred markers.

Renders each region at high DPI and runs RapidOCR (ONNX, CPU, pip-only — no system
tesseract). Input: argv[1]=pdf, argv[2]=JSON [{page, bbox:[x0,y0,x1,y1]}] (1-based
page, top-left points). Output: JSON [text] aligned to input order.
"""
import json
import sys

import numpy as np
import pypdfium2 as pdfium
from rapidocr_onnxruntime import RapidOCR

SCALE = 3.0  # ~216 dpi — OCR likes resolution


def main():
    pdf_path, regions = sys.argv[1], json.loads(sys.argv[2])
    ocr = RapidOCR()
    doc = pdfium.PdfDocument(pdf_path)
    out = []
    for r in regions:
        page = doc[r["page"] - 1]
        pil = page.render(scale=SCALE).to_pil().convert("RGB")
        x0, y0, x1, y1 = r["bbox"]
        crop = pil.crop((
            int(min(x0, x1) * SCALE),
            int(min(y0, y1) * SCALE),
            int(max(x0, x1) * SCALE),
            int(max(y0, y1) * SCALE),
        ))
        res, _ = ocr(np.array(crop))
        out.append(" ".join(line[1] for line in res) if res else "")
    print(json.dumps(out))


if __name__ == "__main__":
    main()
