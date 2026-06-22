#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf"]
# ///
"""Warm per-page table-parse timing + the three-pipeline speed comparison.

Reads speed_yolo.py's output (per-page YOLO time + table regions) and, with the
docling converter loaded ONCE, times for each page:
  - docling whole-page (docling on the isolated page)
  - docling-on-crop for each table region (sum)
  - litparse on each table region via the `lit` binary (sum)

Then reports, per page and in aggregate, three pipelines across EVERY page (not
just table pages):
  region + cheap     = YOLO + litparse(table regions)
  region + expensive = YOLO + docling-on-crop(table regions)
  docling            = docling whole-page

Usage: speed_tables.py --pdf X.pdf --regions yolo.json
"""
import argparse
import json
import subprocess
import tempfile
import time

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from pypdf import PdfReader, PdfWriter
from pypdf.generic import RectangleObject

ap = argparse.ArgumentParser()
ap.add_argument("--pdf", required=True)
ap.add_argument("--regions", required=True)
a = ap.parse_args()

reg = {r["page"]: r for r in json.load(open(a.regions))["pages"]}
pages = sorted(reg)


def write_page(page, box=None):
    """Isolate `page` (1-based) to a temp PDF, optionally cropped to `box`
    (top-left points). Re-reads each call so cropbox mutation doesn't leak."""
    pg = PdfReader(a.pdf).pages[page - 1]
    if box:
        h = float(pg.mediabox.height)
        x0, y0, x1, y1 = box
        rect = RectangleObject([max(0.0, x0), max(0.0, h - y1), x1, h - y0])
        pg.cropbox = rect
        pg.mediabox = rect
    w = PdfWriter()
    w.add_page(pg)
    p = tempfile.mktemp(suffix=".pdf")
    with open(p, "wb") as f:
        w.write(f)
    return p


opts = PdfPipelineOptions()
opts.do_ocr = False
conv = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})
conv.convert(write_page(pages[0]))  # warm-up: load models (excluded)


def docling_ms(path):
    t = time.perf_counter()
    conv.convert(path)
    return (time.perf_counter() - t) * 1000


def lit_ms(page, box):
    crop = write_page(page, box)
    t = time.perf_counter()
    subprocess.run(["lit", "parse", crop, "--format", "json", "-o", crop + ".json", "-q"],
                   check=True, capture_output=True)
    return (time.perf_counter() - t) * 1000


rows = []
for p in pages:
    tables = reg[p]["tables"]
    yolo = reg[p]["yolo_ms"]
    whole = docling_ms(write_page(p))
    crop = sum(docling_ms(write_page(p, b)) for b in tables)
    lit = sum(lit_ms(p, b) for b in tables)
    rows.append({"page": p, "n_tables": len(tables), "yolo": yolo, "lit": lit,
                 "dcrop": crop, "dwhole": whole,
                 "cheap": yolo + lit, "expensive": yolo + crop})

print(f"\n{'page':>5} {'tbls':>4} {'region+cheap':>13} {'region+exp':>11} {'docling':>9}   (ms)")
for r in rows:
    print(f"{r['page']:>5} {r['n_tables']:>4} {r['cheap']:>13.0f} {r['expensive']:>11.0f} {r['dwhole']:>9.0f}")


def tot(key, subset):
    return sum(r[key] for r in subset) / 1000.0


def summarize(name, subset):
    if not subset:
        return
    print(f"\n{name} ({len(subset)} pages):")
    print(f"  region+cheap     {tot('cheap', subset):6.1f}s   (YOLO {tot('yolo', subset):.1f} + litparse {tot('lit', subset):.1f})")
    print(f"  region+expensive {tot('expensive', subset):6.1f}s   (YOLO {tot('yolo', subset):.1f} + docling-crop {tot('dcrop', subset):.1f})")
    print(f"  docling whole    {tot('dwhole', subset):6.1f}s")


summarize("ALL", rows)
summarize("table pages", [r for r in rows if r["n_tables"] > 0])
summarize("NO-table pages", [r for r in rows if r["n_tables"] == 0])
print("\n(warm: models loaded once. region+cheap on a no-table page is just the YOLO")
print(" forward pass — docling whole-page pays the full pipeline on every page.)")
