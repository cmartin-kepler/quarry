#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pdfplumber", "docling", "pypdf"]
# ///
"""Step-0 claim-level probe (doc-build-plan §0 / doc-easy-path-plan §0).

The plan's riskiest question: on BORN-DIGITAL tables, does the cheap parse give
*wrong answers* — or is its messiness cosmetic to a consumer? We make that
objective: a table's numbers are what consumer questions (totals, cell lookups,
deltas) are computed from, so we compare the **numeric content** the cheap parser
(pdfplumber) extracts against a strong reference (Docling) for each table.

  - numbers agree  -> every numeric answer over that table is identical -> the
    cheap parse is answer-faithful regardless of cosmetic structural noise.
  - numbers differ -> a candidate WRONG ANSWER. We render the source crop so a
    human can adjudicate which parser is right (and whether the region was scoped
    correctly -> localizes the failure to layout vs parse).

Pre-registered decision (commit before looking): if answer-faithful >= 95% of
matched born-digital tables, structural noise is cosmetic -> drop cross-tier +
docling for born-digital (shrink to YOLO + litparse). Divergences are the real
failures to invest against.

Usage:
  uv run scripts/probe.py --pdf input/finance/brk-2023-ar.pdf --pages auto --limit 6 \
      --out corpus/probe-brk
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import re
import tempfile

import pdfplumber
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption
from pypdf import PdfReader, PdfWriter


# ---- numeric claim ---------------------------------------------------------

_NUM = re.compile(r"^\(?-?[\d,]*\.?\d+\)?%?$")


def parse_num(s: str):
    """A cell -> float if it reads as a number (handles $, commas, %, (neg))."""
    t = (s or "").strip().replace("$", "").replace(",", "").replace("%", "")
    if not t:
        return None
    neg = t.startswith("(") and t.endswith(")")
    t = t.strip("()")
    try:
        v = float(t)
        return -v if neg else v
    except ValueError:
        return None


def numbers(grid) -> list[float]:
    out = []
    for row in grid or []:
        for cell in row:
            n = parse_num(cell if isinstance(cell, str) else ("" if cell is None else str(cell)))
            if n is not None:
                out.append(round(n, 3))
    return sorted(out)


# ---- geometry --------------------------------------------------------------

def iou(a, b) -> float:
    ix0, iy0 = max(a[0], b[0]), max(a[1], b[1])
    ix1, iy1 = min(a[2], b[2]), min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1 - ix0) * (iy1 - iy0)
    ua = (a[2] - a[0]) * (a[3] - a[1])
    ub = (b[2] - b[0]) * (b[3] - b[1])
    return inter / (ua + ub - inter)


# ---- parsers ---------------------------------------------------------------

def cheap_tables(pdf_path):
    """pdfplumber tables: {page, bbox(top-left pts), grid}."""
    out = []
    with pdfplumber.open(pdf_path) as pdf:
        for pno, page in enumerate(pdf.pages, 1):
            for t in page.find_tables():
                out.append({"page": pno, "bbox": list(t.bbox), "grid": t.extract()})
    return out


def docling_tables(pdf_path):
    """Docling tables (born-digital, no OCR): {page, bbox(top-left pts), grid}."""
    opts = PdfPipelineOptions()
    opts.do_ocr = False
    conv = DocumentConverter(format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=opts)})
    doc = conv.convert(pdf_path).document
    out = []
    for t in doc.tables:
        if not t.prov:
            continue
        prov = t.prov[0]
        page = prov.page_no
        ph = doc.pages[page].size.height
        b = prov.bbox
        # docling bbox may be BOTTOMLEFT; normalize to top-left points
        if str(getattr(b, "coord_origin", "")).upper().endswith("BOTTOMLEFT"):
            bbox = [b.l, ph - b.t, b.r, ph - b.b]
        else:
            bbox = [b.l, b.t, b.r, b.b]
        try:
            df = t.export_to_dataframe()
            grid = [list(map(str, df.columns))] + [list(map(str, r)) for r in df.values.tolist()]
        except Exception:
            grid = []
        out.append({"page": page, "bbox": bbox, "grid": grid})
    return out


# ---- main ------------------------------------------------------------------

def subset_pdf(pdf_path, pages):
    """Extract 1-based `pages` into a temp PDF; returns (path, [orig_page...])."""
    reader = PdfReader(pdf_path)
    writer = PdfWriter()
    for p in pages:
        writer.add_page(reader.pages[p - 1])
    tmp = os.path.join(tempfile.mkdtemp(), "subset.pdf")
    with open(tmp, "wb") as f:
        writer.write(f)
    return tmp, pages


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
        x0 = max(0, x0); y0 = max(0, y0); x1 = min(pg.width, x1); y1 = min(pg.height, y1)
        im = pg.crop((x0, y0, x1, y1)).to_image(resolution=150)
        bio = tempfile.SpooledTemporaryFile()
        im.save(bio, format="PNG")
        bio.seek(0)
        return base64.b64encode(bio.read()).decode()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pdf", required=True)
    ap.add_argument("--pages", default="auto", help='"auto" or comma list of 1-based pages')
    ap.add_argument("--limit", type=int, default=6, help="max table-pages when --pages auto")
    ap.add_argument("--match-iou", type=float, default=0.5)
    ap.add_argument("--out", default="corpus/probe")
    args = ap.parse_args()

    if args.pages == "auto":
        orig_pages = find_table_pages(args.pdf, args.limit)
    else:
        orig_pages = [int(x) for x in args.pages.split(",")]
    if not orig_pages:
        print("no table pages found")
        return
    print(f"probing {len(orig_pages)} page(s): {orig_pages}")

    sub, _ = subset_pdf(args.pdf, orig_pages)  # 1..K in subset = orig_pages[k-1]
    cheap = cheap_tables(sub)
    docl = docling_tables(sub)
    print(f"cheap parser: {len(cheap)} tables; docling: {len(docl)} tables")

    # match cheap<->docling by page + bbox IoU
    results, used = [], set()
    for c in cheap:
        best, bi = 0.0, -1
        for i, d in enumerate(docl):
            if i in used or d["page"] != c["page"]:
                continue
            ov = iou(c["bbox"], d["bbox"])
            if ov > best:
                best, bi = ov, i
        if bi >= 0 and best >= args.match_iou:
            used.add(bi)
            d = docl[bi]
            cn, dn = numbers(c["grid"]), numbers(d["grid"])
            results.append({
                "page": orig_pages[c["page"] - 1],
                "bbox": c["bbox"],
                "iou": round(best, 2),
                "cheap_nums": len(cn), "docling_nums": len(dn),
                "cheap_sum": round(sum(cn), 2), "docling_sum": round(sum(dn), 2),
                "numbers_agree": cn == dn,
                "cheap_grid": c["grid"], "docling_grid": d["grid"],
            })
        else:
            results.append({"page": orig_pages[c["page"] - 1], "bbox": c["bbox"],
                            "iou": round(best, 2), "match": False, "cheap_grid": c["grid"]})

    matched = [r for r in results if r.get("numbers_agree") is not None]
    faithful = [r for r in matched if r["numbers_agree"]]
    divergent = [r for r in matched if not r["numbers_agree"]]
    unmatched = [r for r in results if not r.get("match", True) and "numbers_agree" not in r]

    os.makedirs(args.out, exist_ok=True)
    with open(os.path.join(args.out, "probe.json"), "w") as f:
        json.dump(results, f, indent=1)

    print("\n=== STEP-0 PROBE (born-digital, cheap vs docling numeric content) ===")
    print(f"matched tables:       {len(matched)}")
    if matched:
        rate = 100 * len(faithful) / len(matched)
        print(f"answer-faithful:      {len(faithful)}/{len(matched)} = {rate:.0f}%  (numbers identical)")
        print(f"answer-divergent:     {len(divergent)}  (candidate WRONG ANSWERS — adjudicate)")
        print(f"  pre-registered bar:  >=95% faithful -> structural noise is cosmetic")
        print(f"  -> {'PASS (shrink: drop cross-tier+docling for born-digital)' if rate >= 95 else 'FAIL (divergences are real; keep verification)'}")
    print(f"cheap tables docling missed/mismatched: {len(unmatched)}")

    # adjudication HTML for the divergent cases (crop | cheap | docling)
    if divergent:
        rows = []
        for r in divergent:
            png = crop_png(args.pdf, r["page"], r["bbox"])
            def tbl(g):
                return "<table border=1 cellspacing=0>" + "".join(
                    "<tr>" + "".join(f"<td>{(c or '')}</td>" for c in row) + "</tr>" for row in (g or [])
                ) + "</table>"
            rows.append(
                f"<h3>page {r['page']} — cheap_sum={r['cheap_sum']} vs docling_sum={r['docling_sum']} "
                f"({r['cheap_nums']} vs {r['docling_nums']} numbers)</h3>"
                f"<div style='display:flex;gap:12px;align-items:flex-start'>"
                f"<div><b>source</b><br><img src='data:image/png;base64,{png}' style='max-width:380px'></div>"
                f"<div><b>cheap</b>{tbl(r['cheap_grid'])}</div>"
                f"<div><b>docling</b>{tbl(r['docling_grid'])}</div></div><hr>"
            )
        html = "<html><body><h1>Step-0 probe — divergent tables to adjudicate</h1>" + "".join(rows) + "</body></html>"
        with open(os.path.join(args.out, "divergent.html"), "w") as f:
            f.write(html)
        print(f"\nadjudicate {len(divergent)} divergence(s): open {args.out}/divergent.html")


if __name__ == "__main__":
    main()
