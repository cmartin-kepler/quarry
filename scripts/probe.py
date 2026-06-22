#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber", "docling", "pypdf"]
# ///
"""Step-0 claim-level probe (doc-build-plan §0).

The plan's riskiest question: on BORN-DIGITAL tables, does the *easy-path cheap
parse* give wrong answers — or is its messiness cosmetic? Numbers are what consumer
questions (totals, cell lookups, deltas) are computed from, so we compare the
numeric content the cheap path captures against a strong reference (Docling).

CHEAP = the plan's real easy path: a YOLO table region (layout_detect.py) parsed
by litparse over the text layer (litparse_region.py). We take the numeric tokens
litparse reports in the region — NOT pdfplumber's column-exploding table-finder,
which was the old front-end the plan replaces. (litparse keeps `$ (30)` intact, so
it doesn't fragment values the way the table-finder did.)

  numbers agree  -> every numeric answer over that table is identical -> the cheap
    path is answer-faithful regardless of cosmetic structural noise.
  numbers differ -> a candidate WRONG ANSWER; render the source crop to adjudicate.

Pre-registered: >=95% answer-faithful on matched born-digital tables -> structural
noise is cosmetic -> shrink (drop cross-tier + docling for born-digital).

Usage:
  uv run scripts/probe.py --pdf input/finance/brk-2023-ar.pdf --pages auto --limit 8 \
      --out corpus/probe-brk
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import subprocess
import tempfile
import time

import pdfplumber
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from pypdf import PdfReader, PdfWriter

YOLO_MODEL = "yolo26n"


# ---- numeric claim ---------------------------------------------------------

def parse_num(s):
    t = (s or "").strip().replace("$", "").replace(",", "").replace("%", "").replace(" ", "")
    if not t:
        return None
    neg = t.startswith("(") and t.endswith(")")
    t = t.strip("()")
    try:
        v = float(t)
        return -v if neg else v
    except ValueError:
        return None


def nums_from_tokens(tokens) -> list[float]:
    out = [parse_num(t) for t in tokens]
    return sorted(round(v, 3) for v in out if v is not None)


def nums_from_grid(grid) -> list[float]:
    toks = []
    for row in grid or []:
        for c in row:
            toks.append(c if isinstance(c, str) else ("" if c is None else str(c)))
    return nums_from_tokens(toks)


def iou(a, b) -> float:
    ix0, iy0 = max(a[0], b[0]), max(a[1], b[1])
    ix1, iy1 = min(a[2], b[2]), min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1 - ix0) * (iy1 - iy0)
    return inter / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter)


# ---- cheap: YOLO region + litparse (the plan's easy path) ------------------

def run(cmd) -> str:
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout


def yolo_table_regions(pdf, page):
    regs = json.loads(run(["uv", "run", "scripts/layout_detect.py", YOLO_MODEL, pdf, str(page)]))["regions"]
    return [r["bbox"] for r in regs if r.get("label", "").strip().lower() == "table"]


def litparse_tokens(pdf, page, bbox):
    x0, y0, x1, y1 = bbox
    out = run(["uv", "run", "scripts/litparse_region.py", pdf, str(page),
               str(x0), str(y0), str(x1), str(y1)])
    return [w["text"] for w in json.loads(out).get("words", [])]


def cheap_tables(pdf, n_pages):
    out = []
    t_yolo = t_lit = 0.0
    for page in range(1, n_pages + 1):
        t = time.perf_counter()
        regs = yolo_table_regions(pdf, page)
        t_yolo += time.perf_counter() - t
        for bbox in regs:
            t = time.perf_counter()
            toks = litparse_tokens(pdf, page, bbox)
            t_lit += time.perf_counter() - t
            out.append({"page": page, "bbox": list(bbox), "tokens": toks,
                        "numbers": nums_from_tokens(toks)})
    return out, {"yolo": t_yolo, "litparse": t_lit}


# ---- reference: docling ----------------------------------------------------

def docling_tables(pdf):
    opts = PdfPipelineOptions()
    opts.do_ocr = False
    conv = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})
    doc = conv.convert(pdf).document
    out = []
    for t in doc.tables:
        if not t.prov:
            continue
        prov = t.prov[0]
        page = prov.page_no
        ph = doc.pages[page].size.height
        b = prov.bbox
        if str(getattr(b, "coord_origin", "")).upper().endswith("BOTTOMLEFT"):
            bbox = [b.l, ph - b.t, b.r, ph - b.b]
        else:
            bbox = [b.l, b.t, b.r, b.b]
        try:
            df = t.export_to_dataframe(doc=doc)
            grid = [list(map(str, df.columns))] + [list(map(str, r)) for r in df.values.tolist()]
        except Exception:
            grid = []
        out.append({"page": page, "bbox": bbox, "grid": grid, "numbers": nums_from_grid(grid)})
    return out


# ---- helpers ---------------------------------------------------------------

def subset_pdf(pdf_path, pages):
    reader = PdfReader(pdf_path)
    writer = PdfWriter()
    for p in pages:
        writer.add_page(reader.pages[p - 1])
    tmp = os.path.join(tempfile.mkdtemp(), "subset.pdf")
    with open(tmp, "wb") as f:
        writer.write(f)
    return tmp


def find_table_pages(pdf_path, limit):
    pages = []
    with pdfplumber.open(pdf_path) as pdf:
        for pno, page in enumerate(pdf.pages, 1):
            if page.find_tables():
                pages.append(pno)
            if len(pages) >= limit:
                break
    return pages


def crop_png(pdf_path, page, bbox):
    with pdfplumber.open(pdf_path) as pdf:
        pg = pdf.pages[page - 1]
        x0, y0, x1, y1 = bbox
        box = (max(0, x0), max(0, y0), min(pg.width, x1), min(pg.height, y1))
        bio = tempfile.SpooledTemporaryFile()
        pg.crop(box).to_image(resolution=150).save(bio, format="PNG")
        bio.seek(0)
        return base64.b64encode(bio.read()).decode()


# ---- main ------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pdf", required=True)
    ap.add_argument("--pages", default="auto")
    ap.add_argument("--limit", type=int, default=8)
    ap.add_argument("--match-iou", type=float, default=0.5)
    ap.add_argument("--out", default="corpus/probe")
    args = ap.parse_args()

    orig = ([int(x) for x in args.pages.split(",")] if args.pages != "auto"
            else find_table_pages(args.pdf, args.limit))
    if not orig:
        print("no table pages found")
        return
    print(f"probing {len(orig)} page(s): {orig}")

    sub = subset_pdf(args.pdf, orig)  # subset page k (1-based) == orig[k-1]
    cheap, ctime = cheap_tables(sub, len(orig))
    t = time.perf_counter()
    docl = docling_tables(sub)
    dtime = time.perf_counter() - t
    print(f"cheap (YOLO region + litparse): {len(cheap)} tables; docling: {len(docl)} tables")

    results, used = [], set()
    for c in cheap:
        best, bi = 0.0, -1
        for i, d in enumerate(docl):
            if i in used or d["page"] != c["page"]:
                continue
            ov = iou(c["bbox"], d["bbox"])
            if ov > best:
                best, bi = ov, i
        op = orig[c["page"] - 1]
        if bi >= 0 and best >= args.match_iou:
            used.add(bi)
            d = docl[bi]
            results.append({"page": op, "bbox": c["bbox"], "iou": round(best, 2),
                            "cheap_nums": len(c["numbers"]), "docling_nums": len(d["numbers"]),
                            "cheap_sum": round(sum(c["numbers"]), 2),
                            "docling_sum": round(sum(d["numbers"]), 2),
                            "numbers_agree": c["numbers"] == d["numbers"],
                            "cheap_tokens": c["tokens"], "docling_grid": d["grid"]})
        else:
            results.append({"page": op, "bbox": c["bbox"], "iou": round(best, 2), "match": False})

    matched = [r for r in results if "numbers_agree" in r]
    faithful = [r for r in matched if r["numbers_agree"]]
    divergent = [r for r in matched if not r["numbers_agree"]]
    unmatched = [r for r in results if r.get("match") is False]

    os.makedirs(args.out, exist_ok=True)
    json.dump(results, open(os.path.join(args.out, "probe.json"), "w"), indent=1)

    print("\n=== STEP-0 PROBE (born-digital, YOLO+litparse cheap vs docling) ===")
    print(f"matched tables:   {len(matched)}")
    if matched:
        rate = 100 * len(faithful) / len(matched)
        print(f"answer-faithful:  {len(faithful)}/{len(matched)} = {rate:.0f}%  (numbers identical)")
        print(f"answer-divergent: {len(divergent)}  (candidate wrong answers — adjudicate)")
        print(f"  bar >=95% -> {'PASS: shrink (structural noise cosmetic)' if rate >= 95 else 'FAIL: keep verification'}")
    print(f"cheap tables docling missed/mismatched: {len(unmatched)}")

    cheap_total = ctime["yolo"] + ctime["litparse"]
    print("\n=== SPEED (wall-clock; each `uv run` reloads its model, so startup-dominated) ===")
    print(f"cheap path: YOLO {ctime['yolo']:.1f}s + litparse {ctime['litparse']:.1f}s = {cheap_total:.1f}s")
    print(f"docling:    {dtime:.1f}s")
    if cheap and docl:
        print(f"per-table:  cheap {cheap_total / len(cheap):.2f}s  vs  docling {dtime / len(docl):.2f}s "
              f"({dtime / len(docl) / max(cheap_total / len(cheap), 1e-6):.1f}x)")
    print("  (litparse is the cheap path's real per-table cost; YOLO is one forward pass/page,")
    print("   amortizable across a page's tables; docling runs a full layout+structure pipeline.)")

    if divergent:
        rows = []
        for r in divergent:
            png = crop_png(args.pdf, r["page"], r["bbox"])
            dg = "<table border=1 cellspacing=0>" + "".join(
                "<tr>" + "".join(f"<td>{c or ''}</td>" for c in row) + "</tr>" for row in (r["docling_grid"] or [])
            ) + "</table>"
            rows.append(
                f"<h3>page {r['page']} — cheap_sum={r['cheap_sum']} ({r['cheap_nums']} nums) "
                f"vs docling_sum={r['docling_sum']} ({r['docling_nums']} nums)</h3>"
                f"<div style='display:flex;gap:12px;align-items:flex-start'>"
                f"<div><b>source</b><br><img src='data:image/png;base64,{png}' style='max-width:400px'></div>"
                f"<div style='max-width:280px'><b>cheap tokens (litparse)</b><br>{' · '.join(r['cheap_tokens'])}</div>"
                f"<div><b>docling</b>{dg}</div></div><hr>")
        open(os.path.join(args.out, "divergent.html"), "w").write(
            "<html><body><h1>Step-0 — divergent tables</h1>" + "".join(rows) + "</body></html>")
        print(f"\nadjudicate {len(divergent)}: open {args.out}/divergent.html")


if __name__ == "__main__":
    main()
