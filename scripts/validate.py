#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling", "pypdf"]
# ///
"""Validate the quarry pipeline against full-docling and lit baselines, per doc.

For every PDF under --dir, compare three approaches (all no-OCR):
  - quarry   : `quarry pipeline` (triage-gated docling on TEXT pages only)
  - docling  : docling whole-doc, every page, do_table_structure=True
  - lit      : litparse on the whole doc (text-layer tokens)

The point is CORRECTNESS, not timing (quarry reloads models per uv call, so its
wall-time is not comparable here — see corpus_*.py for clean timing): does the gate
DROP any real tables/text, and does full-docling OVER-detect tables on image pages?

Reports, per doc: pages, quarry {tables, text-elems, OCR markers}, docling-full
{tables, texts}, lit {tokens}, and the table delta (docling_full − quarry).

Usage: validate.py [--dir input] [--quarry target/debug/quarry]
"""
import argparse
import glob
import json
import os
import re
import subprocess
import tempfile
import time

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption


def docling_converter():
    o = PdfPipelineOptions()
    o.do_ocr = False
    o.do_table_structure = True
    for a in ("generate_picture_images", "do_picture_classification", "do_picture_description"):
        if hasattr(o, a):
            setattr(o, a, False)
    return DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=o)})


def run_quarry(quarry, pdf):
    """Run `quarry pipeline` and parse its printed counts."""
    store = tempfile.mkdtemp()
    out = subprocess.run([quarry, "pipeline", pdf, "--out", store],
                         capture_output=True, text=True)
    s = out.stdout
    def g(pat):
        m = re.search(pat, s)
        return int(m.group(1)) if m else -1
    return {
        "tables": g(r"parsed: (\d+) tables"),
        "texts": g(r"(\d+) text elements"),
        "ocr": g(r"(\d+) OCR markers"),
        "text_pages": g(r"(\d+) text,"),
        "img_pages": g(r"(\d+) image_content"),
        "blank_pages": g(r"(\d+) blank"),
        "ok": out.returncode == 0,
        "err": out.stderr[-200:] if out.returncode else "",
    }


def run_docling_full(conv, pdf):
    t = time.perf_counter()
    doc = conv.convert(pdf).document
    return {"tables": len(doc.tables), "texts": len(doc.texts), "s": time.perf_counter() - t}


def run_lit(pdf):
    out_json = tempfile.mktemp(suffix=".json")
    t = time.perf_counter()
    r = subprocess.run(["lit", "parse", pdf, "--format", "json", "-o", out_json, "-q"],
                       capture_output=True, text=True)
    s = time.perf_counter() - t
    toks = 0
    try:
        d = json.load(open(out_json))
        toks = sum(len(p.get("textItems", [])) for p in d.get("pages", []))
    except Exception:
        pass
    return {"tokens": toks, "s": s, "ok": r.returncode == 0}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default="input")
    ap.add_argument("--quarry", default="target/debug/quarry")
    a = ap.parse_args()
    conv = docling_converter()

    pdfs = sorted(glob.glob(os.path.join(a.dir, "**", "*.pdf"), recursive=True))
    # smallest first so partial runs cover the most docs
    from pypdf import PdfReader
    def npages(p):
        try:
            return len(PdfReader(p).pages)
        except Exception:
            return 10**9
    pdfs.sort(key=npages)

    print(f"{'document':40}{'pg':>4}  {'quarry(t/txt/ocr)':>18} {'docling(t/txt)':>15} {'lit tok':>8}  {'Δtbl':>5}")
    for pdf in pdfs:
        name = os.path.basename(pdf)[:38]
        q = run_quarry(a.quarry, pdf)
        if not q["ok"]:
            print(f"{name:40}  quarry FAILED: {q['err']}", flush=True)
            continue
        d = run_docling_full(conv, pdf)
        lt = run_lit(pdf)
        n = q["text_pages"] + q["img_pages"] + q["blank_pages"]
        dtbl = d["tables"] - q["tables"]  # tables docling-full found that quarry didn't (on skipped pages)
        flag = "  <-- DROPPED" if dtbl > 0 else ""
        print(f"{name:40}{n:>4}  {q['tables']:>4}/{q['texts']:>4}/{q['ocr']:<4}     "
              f"{d['tables']:>4}/{d['texts']:<4}      {lt['tokens']:>7}  {dtbl:>5}{flag}", flush=True)


if __name__ == "__main__":
    main()
