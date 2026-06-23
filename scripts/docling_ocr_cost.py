#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf", "rapidocr-onnxruntime", "onnxruntime"]
# ///
"""Cost of running docling WITH OCR vs without, per page.

Our pipeline runs docling do_ocr=False (skip OCR) and OCRs only no-text regions
separately. The alternative is docling do_ocr=True (OCR inside the parse). This times
both on a text page and an image page so we can compare. Uses the RapidOCR engine
(already installed) so we don't pull easyocr/torch.
"""
import tempfile
import time

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions, RapidOcrOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from pypdf import PdfReader, PdfWriter


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


def page_pdf(reader, p):
    w = PdfWriter()
    w.add_page(reader.pages[p - 1])
    path = tempfile.mktemp(suffix=".pdf")
    with open(path, "wb") as f:
        w.write(f)
    return path


def main():
    c_no, c_ocr = conv(False), conv(True)
    cases = [
        ("text page ", "input/finance/brk-2023-letter.pdf", 2),
        ("image page", "input/finance/disney/Q4-FY24-Earnings-Presentation.pdf", 1),
    ]
    # warm both engines once
    warm = page_pdf(PdfReader(cases[0][1]), 1)
    c_no.convert(warm)
    c_ocr.convert(warm)

    print(f"{'page':12}{'docling no-ocr':>16}{'docling +ocr':>14}{'OCR adds':>10}")
    for label, pdf, pg in cases:
        reader = PdfReader(pdf)
        p = page_pdf(reader, pg)
        t = time.perf_counter()
        c_no.convert(p)
        no_ms = (time.perf_counter() - t) * 1000
        t = time.perf_counter()
        c_ocr.convert(p)
        ocr_ms = (time.perf_counter() - t) * 1000
        print(f"{label:12}{no_ms:>13.0f}ms{ocr_ms:>11.0f}ms{ocr_ms-no_ms:>7.0f}ms")


if __name__ == "__main__":
    main()
