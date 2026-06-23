#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf", "pdfplumber"]
# ///
"""Does the cheap triage gate SAVE docling time? Warm, per page, across a corpus.

For each page: classify it (triage: words + image-area + thumbnail stddev) AND time
docling-whole on that single page (model loaded once). Then:
  full   = Σ docling over ALL pages          (docling on everything)
  gated  = Σ docling over TEXT pages only     (the quarry pipeline)
  saved  = full − gated                        (docling skipped on image/blank pages)
  triage = Σ classify() over all pages         (the gate's own cost)
  net    = saved − triage

Reports per doc and corpus total. Timing is WARM and per-page (consistent with the
o4 figure in 03) — the fair comparison the validation (08) deliberately didn't do.
"""
import argparse
import glob
import os
import tempfile
import time

import pdfplumber
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from PIL import ImageStat
from pypdf import PdfReader, PdfWriter

W_TEXT, W_LOW, EPS, IMG_FRAC = 30, 5, 5.0, 0.5


def make_conv():
    o = PdfPipelineOptions()
    o.do_ocr = False
    o.do_table_structure = True
    for a in ("generate_picture_images", "do_picture_classification", "do_picture_description"):
        if hasattr(o, a):
            setattr(o, a, False)
    return DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o)})


def classify(page):
    """Return (klass, classify_ms). Renders only for low-text pages."""
    t = time.perf_counter()
    words = len(page.extract_words() or [])
    pa = (page.width or 0) * (page.height or 0)
    iarea = sum(max(0.0, im["x1"] - im["x0"]) * max(0.0, im["bottom"] - im["top"]) for im in (page.images or []))
    frac = iarea / pa if pa else 0.0
    if words >= W_TEXT:
        return "text", (time.perf_counter() - t) * 1000
    sd = ImageStat.Stat(page.to_image(resolution=40).original.convert("L")).stddev[0]
    ms = (time.perf_counter() - t) * 1000
    if sd < EPS:
        return "blank", ms
    if words < W_LOW and frac >= IMG_FRAC:
        return "image_content", ms
    return "text", ms


def page_pdf(reader, p):
    w = PdfWriter()
    w.add_page(reader.pages[p - 1])
    path = tempfile.mktemp(suffix=".pdf")
    with open(path, "wb") as f:
        w.write(f)
    return path


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="input")
    a = ap.parse_args()
    conv = make_conv()
    pdfs = sorted(glob.glob(os.path.join(a.dir, "**", "*.pdf"), recursive=True))
    pdfs.sort(key=lambda p: (lambda r: len(r.pages))(PdfReader(p)))

    # warm docling once on the first page of the first doc
    conv.convert(page_pdf(PdfReader(pdfs[0]), 1))

    print(f"{'document':38}{'pg':>4}{'t/i/b':>10}{'full s':>8}{'gated s':>8}{'saved':>7}{'triage':>7}{'net%':>6}")
    tot = {"full": 0.0, "gated": 0.0, "triage": 0.0, "pg": 0}
    for pdf in pdfs:
        reader = PdfReader(pdf)
        full = gated = triage = 0.0
        c = {"text": 0, "image_content": 0, "blank": 0}
        with pdfplumber.open(pdf) as plumb:
            for i, page in enumerate(plumb.pages, 1):
                klass, cms = classify(page)
                triage += cms
                c[klass] += 1
                t = time.perf_counter()
                conv.convert(page_pdf(reader, i))
                ms = (time.perf_counter() - t) * 1000
                full += ms
                if klass == "text":
                    gated += ms
        saved = full - gated
        net = saved - triage
        for k, v in (("full", full), ("gated", gated), ("triage", triage)):
            tot[k] += v
        tot["pg"] += len(reader.pages)
        netpct = 100 * net / full if full else 0
        cls = f"{c['text']}/{c['image_content']}/{c['blank']}"
        print(f"{os.path.basename(pdf)[:37]:38}{len(reader.pages):>4}{cls:>10}"
              f"{full/1000:>8.1f}{gated/1000:>8.1f}{saved/1000:>7.1f}{triage/1000:>7.1f}{netpct:>5.0f}%", flush=True)

    s, g, tr = tot["full"], tot["gated"], tot["triage"]
    print(f"\nCORPUS ({tot['pg']} pages): docling-all {s/1000:.0f}s  →  triage-gated {g/1000:.0f}s + triage {tr/1000:.1f}s")
    print(f"  docling time saved: {(s-g)/1000:.0f}s ({100*(s-g)/s:.0f}%);  NET saved (after triage cost): {(s-g-tr)/1000:.0f}s ({100*(s-g-tr)/s:.0f}%)")


if __name__ == "__main__":
    main()
