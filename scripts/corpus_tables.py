#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf"]
# ///
"""Corpus-wide whole-document cost comparison of 4 table-extraction pipelines.

Reads corpus_yolo.py's per-page YOLO timings + table regions, then with the
docling converter loaded ONCE (do_ocr=False, pictures bounded but not extracted)
times per sampled page: docling-whole, docling-on-crop, litparse-on-region. Builds
the 4 options and extrapolates each to whole-document seconds (mean per page x doc
page count).

  o1 region+cheap          = YOLO(every pg) + litparse(table regions)
  o2 region+expensive      = YOLO(every pg) + docling-on-crop(table regions)
  o3 YOLO-gated docling     = YOLO(every pg) + [table pg: docling-whole + litparse]
  o4 docling-whole every pg = docling-whole(every pg)  (the "docling on the whole thing")
"""
import argparse
import json
import os
import subprocess
import tempfile
import time
from collections import defaultdict

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from pypdf import PdfReader, PdfWriter
from pypdf.generic import RectangleObject

ap = argparse.ArgumentParser()
ap.add_argument("--regions", required=True)
a = ap.parse_args()
docs = json.load(open(a.regions))

opts = PdfPipelineOptions()
opts.do_ocr = False                       # born-digital: keep the text layer
for attr in ("generate_picture_images", "do_picture_classification", "do_picture_description"):
    if hasattr(opts, attr):
        setattr(opts, attr, False)        # pictures: existence only, don't process
conv = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})


def write_page(path, page, box=None):
    pg = PdfReader(path).pages[page - 1]
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


def dms(path):
    t = time.perf_counter()
    conv.convert(path)
    return (time.perf_counter() - t) * 1000


def lms(path, page, box):
    crop = write_page(path, page, box)
    t = time.perf_counter()
    subprocess.run(["lit", "parse", crop, "--format", "json", "-o", crop + ".json", "-q"],
                   check=True, capture_output=True)
    return (time.perf_counter() - t) * 1000


first = next(iter(docs))
conv.convert(write_page(first, docs[first]["sample"][0]["page"]))  # warm-up (load models)

keys = ["o1", "o2", "o3", "o4"]
tot = {k: 0.0 for k in keys}
total_pages = 0
print(f"\n{'document':40}{'pg':>5}{'tbl%':>6}{'o1 cheap':>10}{'o2 exp':>9}{'o3 gated':>10}{'o4 dcAll':>10}  o4/o3", flush=True)
print(f"{'':40}{'':>5}{'':>6}{'(Y+lit)':>10}{'(Y+crop)':>9}{'(gated)':>10}{'(all pg)':>10}", flush=True)

# Stream per-document so partial results survive and the gating saving (o3 vs o4)
# is visible as each doc completes.
for path, d in sorted(docs.items(), key=lambda kv: kv[1]["n_pages"]):  # smallest first
    n = d["n_pages"]
    sample = d["sample"]
    k = len(sample)
    acc = {key: 0.0 for key in keys}
    n_tbl = 0
    for s in sample:
        p, tables, yolo = s["page"], s["tables"], s["yolo_ms"]
        dwhole = dms(write_page(path, p))
        dcrop = sum(dms(write_page(path, p, b)) for b in tables)
        lit = sum(lms(path, p, b) for b in tables)
        has = len(tables) > 0
        n_tbl += has
        acc["o1"] += yolo + lit
        acc["o2"] += yolo + dcrop
        acc["o3"] += yolo + (dwhole + lit if has else 0.0)
        acc["o4"] += dwhole
    scale = n / k                      # 1.0 when every page sampled (exact whole-doc)
    line = {key: acc[key] * scale / 1000.0 for key in keys}
    for key in keys:
        tot[key] += line[key]
    total_pages += n
    tag = "" if k == n else f"~{k}/{n}"
    ratio = line["o4"] / line["o3"] if line["o3"] else 0.0
    name = (os.path.basename(path)[:37] + tag)[:39]
    print(f"{name:40}{n:>5}{100*n_tbl/k:>5.0f}%{line['o1']:>10.1f}{line['o2']:>9.1f}{line['o3']:>10.1f}{line['o4']:>10.1f}{ratio:>6.1f}x", flush=True)

ratio = tot["o4"] / tot["o3"] if tot["o3"] else 0.0
print(f"\n{'CORPUS TOTAL (' + str(total_pages) + ' pages)':40}{'':>5}{'':>6}{tot['o1']:>10.1f}{tot['o2']:>9.1f}{tot['o3']:>10.1f}{tot['o4']:>10.1f}{ratio:>6.1f}x  seconds")
print("\no1 YOLO+litparse | o2 YOLO+docling-crop | o3 YOLO-gated docling-whole(+lit on table pages) | o4 docling-whole every page")
print("o4/o3 = how much docling-on-every-page costs vs gating it to table pages (the saving from skipping no-table pages).")
