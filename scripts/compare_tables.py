#!/usr/bin/env python3
"""
compare_tables.py - Build a visual HTML report: for each table region, show the
rendered PDF crop (the original) next to each parser's reconstructed table.

Self-contained: PDF crops are rendered with pdfplumber and embedded as base64, so
the output is a single .html you open in a browser — no server. Tables from the
different stores are matched by page + bounding-box overlap, so you see the same
region side by side (including "— no table here —" when a parser missed it).

Usage:
  uv run scripts/compare_tables.py --pdf input/arxiv/2604.08538v3.pdf \
      --store docling=corpus/2604.docling.artifacts \
      --store liteparse=corpus/2604.liteparse.artifacts \
      --store cheap=corpus/input/2604.08538v3.artifacts \
      -o corpus/compare-2604.html
  open corpus/compare-2604.html
"""
from __future__ import annotations

import argparse
import base64
import html as html_lib
import io
import json
import os
import sys

import pdfplumber


def load_tables(store_dir: str) -> list[dict]:
    manifest = json.load(open(os.path.join(store_dir, "manifest.json")))
    out = []
    for a in manifest["artifacts"]:
        if a.get("kind") != "HtmlTable":
            continue
        src = a["meta"]["provenance"].get("Source") or {}
        if src.get("format") != "pdf":
            continue
        b = src["bbox"]
        out.append({
            "id": a["meta"]["id"], "page": src["page"],
            "bbox": (b["x0"], b["y0"], b["x1"], b["y1"]),
            "rows": a["n_rows"], "cols": a["n_cols"], "html": a["html"],
        })
    return out


def iou(a, b) -> float:
    ix0, iy0 = max(a[0], b[0]), max(a[1], b[1])
    ix1, iy1 = min(a[2], b[2]), min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1 - ix0) * (iy1 - iy0)
    area = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / area if area > 0 else 0.0


def union(a, b):
    return (min(a[0], b[0]), min(a[1], b[1]), max(a[2], b[2]), max(a[3], b[3]))


def cluster_regions(by_parser: dict[str, list[dict]]):
    """Group tables across parsers into shared regions by page + bbox overlap."""
    regions = []  # each: {"page", "bbox", "tables": {parser: table}}
    for parser, tables in by_parser.items():
        for t in tables:
            placed = False
            for r in regions:
                if r["page"] == t["page"] and iou(r["bbox"], t["bbox"]) > 0.3:
                    r["bbox"] = union(r["bbox"], t["bbox"])
                    r["tables"].setdefault(parser, t)
                    placed = True
                    break
            if not placed:
                regions.append({"page": t["page"], "bbox": t["bbox"], "tables": {parser: t}})
    regions.sort(key=lambda r: (r["page"], r["bbox"][1]))
    return regions


def crop_png(pdf, page: int, bbox, pad=6, resolution=120) -> str:
    pg = pdf.pages[page - 1]
    x0, y0, x1, y1 = bbox
    box = (max(0, x0 - pad), max(0, y0 - pad), min(pg.width, x1 + pad), min(pg.height, y1 + pad))
    im = pg.crop(box).to_image(resolution=resolution)
    buf = io.BytesIO()
    im.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode()


CSS = """
body { font-family: -apple-system, system-ui, sans-serif; margin: 20px; background:#f6f6f7; }
h1 { font-size: 20px; } .meta { color:#666; font-size:13px; margin-bottom:16px; }
.region { background:#fff; border:1px solid #ddd; border-radius:8px; margin:18px 0; padding:14px; }
.region h2 { font-size:15px; margin:0 0 10px; }
.cols { display:flex; gap:16px; align-items:flex-start; overflow-x:auto; }
.cell { flex:0 0 auto; max-width:46%; }
.cell h3 { font-size:13px; margin:0 0 6px; color:#333; }
.cell.src h3 { color:#0a7; } .tag { font-size:11px; color:#888; font-weight:normal; }
img { max-width:100%; border:1px solid #ccc; background:#fff; }
table { border-collapse:collapse; font-size:12px; background:#fff; }
td, th { border:1px solid #bbb; padding:2px 6px; text-align:left; white-space:nowrap; }
th { background:#eef; } .miss { color:#b00; font-style:italic; padding:20px 0; }
"""


def render(pdf_path: str, by_parser: dict[str, list[dict]], parsers: list[str]) -> str:
    regions = cluster_regions(by_parser)
    pdf = pdfplumber.open(pdf_path)
    parts = [f"<html><head><meta charset='utf-8'><style>{CSS}</style></head><body>"]
    parts.append(f"<h1>Table reconstruction vs. source — {html_lib.escape(os.path.basename(pdf_path))}</h1>")
    counts = {p: sum(1 for r in regions if p in r["tables"]) for p in parsers}
    parts.append("<div class='meta'>" + " &nbsp;|&nbsp; ".join(
        f"<b>{p}</b>: {counts[p]} tables" for p in parsers) +
        f" &nbsp;|&nbsp; {len(regions)} distinct region(s)</div>")

    for i, r in enumerate(regions, 1):
        png = crop_png(pdf, r["page"], r["bbox"])
        parts.append("<div class='region'>")
        bb = ",".join(f"{v:.0f}" for v in r["bbox"])
        parts.append(f"<h2>Region {i} — page {r['page']} &nbsp;<span class='tag'>bbox {bb}</span></h2>")
        parts.append("<div class='cols'>")
        parts.append("<div class='cell src'><h3>SOURCE (rendered PDF crop)</h3>"
                     f"<img src='data:image/png;base64,{png}'></div>")
        for p in parsers:
            t = r["tables"].get(p)
            if t:
                parts.append(f"<div class='cell'><h3>{html_lib.escape(p)} "
                             f"<span class='tag'>{t['rows']}×{t['cols']}</span></h3>{t['html']}</div>")
            else:
                parts.append(f"<div class='cell'><h3>{html_lib.escape(p)}</h3>"
                             "<div class='miss'>— no table detected here —</div></div>")
        parts.append("</div></div>")
    parts.append("</body></html>")
    return "\n".join(parts)


def main():
    ap = argparse.ArgumentParser(description="Visual table-vs-source comparison report.")
    ap.add_argument("--pdf", required=True)
    ap.add_argument("--store", action="append", required=True,
                    help="name=dir (repeatable), e.g. docling=corpus/x.artifacts")
    ap.add_argument("-o", "--out", required=True)
    args = ap.parse_args()

    by_parser, parsers = {}, []
    for spec in args.store:
        name, _, d = spec.partition("=")
        parsers.append(name)
        by_parser[name] = load_tables(d)

    out = render(args.pdf, by_parser, parsers)
    with open(args.out, "w") as fh:
        fh.write(out)
    print(f"wrote {args.out} ({os.path.getsize(args.out)//1024} KB) — open it in a browser",
          file=sys.stderr)


if __name__ == "__main__":
    main()
