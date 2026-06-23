#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf"]
# ///
"""Per-page docling stage costs (all do_ocr=False), to see where time goes and
whether image-dominant pages are expensive. Columns:
  litparse        : the `lit` text-grid parser
  dl-text         : docling, do_table_structure=False (layout + text, no tables)
  dl-tables       : docling, do_table_structure=True  (text + table-structure model)
  tbl-model       : dl-tables - dl-text  (cost of docling's table-structure model)
"""
import glob
import os
import statistics
import subprocess
import tempfile
import time

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption


def make(tables):
    o = PdfPipelineOptions()
    o.do_ocr = False
    o.do_table_structure = tables
    for a in ("generate_picture_images", "do_picture_classification", "do_picture_description"):
        if hasattr(o, a):
            setattr(o, a, False)
    return DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o)})


conv_text = make(False)
conv_full = make(True)


def find(substr):
    hits = glob.glob(f"input/**/*{substr}*", recursive=True)
    return hits[0] if hits else None


def write_page(path, page):
    from pypdf import PdfReader, PdfWriter
    pg = PdfReader(path).pages[page - 1]
    w = PdfWriter()
    w.add_page(pg)
    p = tempfile.mktemp(suffix=".pdf")
    with open(p, "wb") as f:
        w.write(f)
    return p


def lit_ms(p):
    t = time.perf_counter()
    subprocess.run(["lit", "parse", p, "--format", "json", "-o", p + ".json", "-q"],
                   check=True, capture_output=True)
    return (time.perf_counter() - t) * 1000


def dl_ms(conv, p):
    t = time.perf_counter()
    conv.convert(p)
    return (time.perf_counter() - t) * 1000


CASES = [
    ("Q4-FY24-Earnings", 20, "Q4 slide p20 (FULL IMAGE, 0 words)"),
    ("Q4-FY24-Earnings", 22, "Q4 slide p22 (FULL IMAGE, 0 words)"),
    ("gpmr2026", 2, "gpmr p2 (image-dominant, ~28 words)"),
    ("2026-Annual-Investor", 23, "2026pres p23 (22 imgs + text layer)"),
    ("2026-Annual-Investor", 5, "2026pres p5 (6 img + 2 tables)"),
    ("brk-2023-ar", 50, "brk p50 (plain text)"),
    ("brk-2023-ar", 55, "brk p55 (dense financial table)"),
]

resolved = [(find(s), pg, lbl) for s, pg, lbl in CASES]
warm = write_page(*[x for x in resolved if x[0]][0][:2])
conv_text.convert(warm)
conv_full.convert(warm)
lit_ms(warm)

print(f"\n{'page':38}{'litparse':>9}{'dl-text':>9}{'dl-tables':>10}{'tbl-model':>10}  (ms)")
for path, page, label in resolved:
    if not path:
        print(f"{label[:37]:38}  (not found)")
        continue
    fp = write_page(path, page)
    lit = statistics.median([lit_ms(fp) for _ in range(2)])
    dt = statistics.median([dl_ms(conv_text, fp) for _ in range(2)])
    df = statistics.median([dl_ms(conv_full, fp) for _ in range(2)])
    print(f"{label[:37]:38}{lit:>9.0f}{dt:>9.0f}{df:>10.0f}{df - dt:>10.0f}")
