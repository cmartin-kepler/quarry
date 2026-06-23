#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "Pillow", "rapidocr-onnxruntime", "numpy"]
# ///
"""Is OCR worth it, and what does sparing use save?

1. Measure OCR cost per page, for text pages vs image pages (render + recognize).
2. Across the corpus, count no-text pages (the only pages that NEED OCR) and compute:
     OCR-everything   = all pages   × cost
     OCR-sparingly    = no-text only × cost   (what the pipeline does)
     saved            = text pages  × cost     (skipped — they already have a text layer)
"""
import glob
import os
import statistics
import time

import numpy as np
import pypdfium2 as pdfium
from rapidocr_onnxruntime import RapidOCR

SCALE = 3.0
CHARS_TEXT = 150  # >= this many chars => has a real text layer (no OCR needed)


def ocr_page_ms(ocr, page):
    pil = page.render(scale=SCALE).to_pil().convert("RGB")
    t = time.perf_counter()
    ocr(np.array(pil))
    return (time.perf_counter() - t) * 1000


def main():
    ocr = RapidOCR()
    text_pdf = "input/finance/brk-2023-letter.pdf"  # dense text pages
    img_pdf = "input/finance/disney/Q4-FY24-Earnings-Presentation.pdf"  # full-image slides

    print("=== 1. OCR cost per page (render @216dpi + recognize) ===")
    td = pdfium.PdfDocument(text_pdf)
    tms = [ocr_page_ms(ocr, td[i]) for i in range(min(4, len(td)))]
    idd = pdfium.PdfDocument(img_pdf)
    ims = [ocr_page_ms(ocr, idd[i]) for i in range(min(4, len(idd)))]
    text_ms, img_ms = statistics.median(tms), statistics.median(ims)
    avg = statistics.median(tms + ims)
    print(f"  text page : {text_ms:6.0f} ms/page")
    print(f"  image page: {img_ms:6.0f} ms/page")
    print(f"  blended   : {avg:6.0f} ms/page (used below)\n")

    print("=== 2. corpus economics: OCR everything vs only no-text pages ===")
    print(f"{'document':40}{'pg':>4}{'no-text':>8}{'all s':>8}{'sparse s':>9}{'saved s':>8}{'saved%':>7}")
    tot = {"pg": 0, "notext": 0}
    for pdf in sorted(glob.glob("input/**/*.pdf", recursive=True)):
        doc = pdfium.PdfDocument(pdf)
        n = len(doc)
        notext = sum(1 for i in range(n) if doc[i].get_textpage().count_chars() < CHARS_TEXT)
        tot["pg"] += n
        tot["notext"] += notext
        all_s = n * avg / 1000
        sparse_s = notext * avg / 1000
        saved_s = (n - notext) * avg / 1000
        pct = 100 * (n - notext) / n if n else 0
        print(f"{os.path.basename(pdf)[:39]:40}{n:>4}{notext:>8}{all_s:>8.0f}{sparse_s:>9.0f}{saved_s:>8.0f}{pct:>6.0f}%")

    P, NT = tot["pg"], tot["notext"]
    print(
        f"\nCORPUS {P} pages: OCR-everything {P*avg/1000:.0f}s  →  OCR-sparingly {NT*avg/1000:.0f}s "
        f"(only {NT} no-text pages); saved {(P-NT)*avg/1000:.0f}s ({100*(P-NT)/P:.0f}%) by skipping the {P-NT} text pages."
    )


if __name__ == "__main__":
    main()
