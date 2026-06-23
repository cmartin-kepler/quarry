#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf"]
# ///
"""Region-scoped Docling sidecar (build-plan Step C-docling).

Crop a PDF page to a table region (set CropBox+MediaBox via pypdf, no OCR — the
born-digital text layer stays intact), run Docling on the 1-page crop, and print
the DoclingDocument JSON — the same schema run_docling.py emits, consumed by the
crate's `docling::artifacts_from_docling` adapter. The crate then translates the
crop-relative cell boxes back to page coordinates (coords::crop_to_page).

Usage: docling_crop.py <pdf> <page> <x0> <y0> <x1> <y1>   (bbox top-left points)
"""
import os
import sys
import tempfile

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from pypdf import PdfReader, PdfWriter
from pypdf.generic import RectangleObject


def main() -> None:
    pdf, page = sys.argv[1], int(sys.argv[2])
    x0, y0, x1, y1 = map(float, sys.argv[3:7])

    reader = PdfReader(pdf)
    pg = reader.pages[page - 1]
    height = float(pg.mediabox.height)
    # top-left bbox -> PDF bottom-left crop rectangle (same convention as litparse_region.py)
    rect = RectangleObject([max(0.0, x0), max(0.0, height - y1), x1, height - y0])
    pg.cropbox = rect
    pg.mediabox = rect
    writer = PdfWriter()
    writer.add_page(pg)
    tmp = os.path.join(tempfile.mkdtemp(), "crop.pdf")
    with open(tmp, "wb") as f:
        writer.write(f)

    # Born-digital: keep the text layer, DO NOT OCR (the plan's "text layer intact"
    # — avoids reintroducing OCR value-error risk, and is faster).
    opts = PdfPipelineOptions()
    opts.do_ocr = False
    conv = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})
    result = conv.convert(tmp)
    print(result.document.model_dump_json())


if __name__ == "__main__":
    main()
