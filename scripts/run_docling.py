#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf", "rapidocr-onnxruntime", "onnxruntime"]
# ///
"""Docling sidecar for the Rust `docling` extractor.

Run Docling on a PDF (do_ocr=True, text-layer-aware: docling reads the programmatic
text layer and OCRs ONLY bitmap regions that lack one — e.g. embedded figures — so
figure text is recovered surgically, ~free on pure-text pages; see evidence/10. We use
the RapidOCR engine so no system tesseract / easyocr+torch is needed. Pictures are
bounded-not-processed.) and print the `DoclingDocument` JSON — the schema the crate's
`docling::artifacts_from_docling` / `structured_doc_from_docling` adapters consume
(pages, tables with cells+prov, texts with labels, body reading order).

With `--pages 5,9,12` only those pages are converted (extracted to a temp PDF),
and the page numbers in the output are remapped back to the ORIGINAL pages — so the
Stage-0 triage can skip image/blank pages without docling ever seeing them.

Named run_docling.py (NOT docling_parse.py): `docling_parse` is a docling
dependency; a same-named script on sys.path shadows it (circular import).

Usage: run_docling.py <pdf> [--pages 1,2,...]
"""
import argparse
import json
import os
import tempfile

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions, RapidOcrOptions
from docling.document_converter import DocumentConverter, PdfFormatOption


def converter():
    o = PdfPipelineOptions()
    o.do_ocr = True  # text-layer-aware: OCRs only no-text regions (figures), not the text
    o.ocr_options = RapidOcrOptions()  # ONNX/CPU engine — no system tesseract / torch
    o.do_table_structure = True
    for a in ("generate_picture_images", "do_picture_classification", "do_picture_description"):
        if hasattr(o, a):
            setattr(o, a, False)
    return DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o)})


def remap_pages(d, sub_to_orig):
    """Rewrite page numbers in a model_dump dict from temp-PDF pages to originals."""
    if isinstance(d.get("pages"), dict):
        d["pages"] = {str(sub_to_orig.get(int(k), int(k))): v for k, v in d["pages"].items()}
    for coll in ("tables", "texts", "pictures"):
        for item in d.get(coll, []) or []:
            for pr in item.get("prov", []) or []:
                if "page_no" in pr:
                    pr["page_no"] = sub_to_orig.get(pr["page_no"], pr["page_no"])
    return d


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pdf")
    ap.add_argument("--pages", help="comma list of 1-based pages to convert (default: all)")
    a = ap.parse_args()
    conv = converter()

    if a.pages:
        from pypdf import PdfReader, PdfWriter
        pages = [int(x) for x in a.pages.split(",") if x.strip()]
        reader = PdfReader(a.pdf)
        writer = PdfWriter()
        for p in pages:
            writer.add_page(reader.pages[p - 1])
        tmp = os.path.join(tempfile.mkdtemp(), "sub.pdf")
        with open(tmp, "wb") as f:
            writer.write(f)
        doc = conv.convert(tmp).document
        d = remap_pages(doc.model_dump(mode="json"), {i + 1: pages[i] for i in range(len(pages))})
        print(json.dumps(d))
    else:
        print(conv.convert(a.pdf).document.model_dump_json())


if __name__ == "__main__":
    main()
