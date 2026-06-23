#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdfium2", "rapidocr-onnxruntime", "onnxruntime"]
# ///
"""docling do_ocr=False vs do_ocr=True across the whole corpus, per document — the
cost of docling's (text-layer-aware) OCR. Warm: converters loaded once. Writes
docling_ocr_corpus.json. do_ocr=True OCRs only no-text regions, so the overhead shows
where the bitmap content actually is.
"""
import glob
import json
import os
import time

import pypdfium2 as pdfium
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions, RapidOcrOptions
from docling.document_converter import DocumentConverter, PdfFormatOption


def conv(do_ocr):
    o = PdfPipelineOptions()
    o.do_ocr = do_ocr
    o.do_table_structure = True
    if do_ocr:
        o.ocr_options = RapidOcrOptions()
    for a in ("generate_picture_images", "do_picture_classification", "do_picture_description"):
        if hasattr(o, a):
            setattr(o, a, False)
    return DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o)})


def main():
    c_no, c_ocr = conv(False), conv(True)
    pdfs = sorted(glob.glob("input/**/*.pdf", recursive=True))
    pdfs.sort(key=lambda p: len(pdfium.PdfDocument(p)))

    # warm both engines on the smallest doc
    c_no.convert(pdfs[0])
    c_ocr.convert(pdfs[0])

    rows = []
    print(f"{'document':40}{'pg':>4}{'no-ocr s':>10}{'+ocr s':>9}{'OCR adds':>10}{'overhead%':>10}", flush=True)
    for pdf in pdfs:
        n = len(pdfium.PdfDocument(pdf))
        t = time.perf_counter()
        c_no.convert(pdf)
        no_s = time.perf_counter() - t
        t = time.perf_counter()
        c_ocr.convert(pdf)
        ocr_s = time.perf_counter() - t
        add = ocr_s - no_s
        pct = 100 * add / no_s if no_s else 0
        rows.append({"document": os.path.basename(pdf), "pages": n,
                     "no_ocr_s": round(no_s, 1), "with_ocr_s": round(ocr_s, 1), "ocr_overhead_s": round(add, 1)})
        print(f"{os.path.basename(pdf)[:39]:40}{n:>4}{no_s:>10.1f}{ocr_s:>9.1f}{add:>10.1f}{pct:>9.0f}%", flush=True)

    json.dump(rows, open("docling_ocr_corpus.json", "w"), indent=2)
    no = sum(r["no_ocr_s"] for r in rows)
    wo = sum(r["with_ocr_s"] for r in rows)
    pg = sum(r["pages"] for r in rows)
    print(
        f"\nCORPUS ({len(rows)} docs, {pg} pages): docling no-OCR {no:.0f}s  →  with-OCR {wo:.0f}s  "
        f"(OCR adds {wo-no:.0f}s, {100*(wo-no)/no:.0f}%). docling OCRs only no-text regions, "
        f"so the add is concentrated on scanned/image docs."
    )
    print("wrote docling_ocr_corpus.json")


if __name__ == "__main__":
    main()
