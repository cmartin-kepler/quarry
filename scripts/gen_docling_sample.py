#!/usr/bin/env python3
"""
gen_docling_sample.py - Emit a schema-faithful DoclingDocument JSON sample.

Builds a small DoclingDocument with docling-core's own models and dumps
`export_to_dict()`, so the JSON matches exactly what a real Docling conversion
produces (same field names, same BOTTOMLEFT bbox origin) — without the heavy
model pipeline. Used as a deterministic fixture for the Rust Docling adapter.

Usage:
  uv run --with docling-core scripts/gen_docling_sample.py -o tests/data/sample.docling.json
"""
from __future__ import annotations

import argparse
import json

from docling_core.types.doc import (
    BoundingBox,
    CoordOrigin,
    DoclingDocument,
    ProvenanceItem,
    Size,
    TableCell,
    TableData,
)

PAGE_W, PAGE_H = 612.0, 792.0


def cell(text, r, c, *, header=False, l=0.0, t=0.0, r_=0.0, b=0.0):
    return TableCell(
        text=text,
        start_row_offset_idx=r,
        end_row_offset_idx=r + 1,
        start_col_offset_idx=c,
        end_col_offset_idx=c + 1,
        column_header=header,
        # BOTTOMLEFT origin (Docling's PDF default): t/b measured from page bottom.
        bbox=BoundingBox(l=l, t=t, r=r_, b=b, coord_origin=CoordOrigin.BOTTOMLEFT),
    )


def table_from(rows, top_y):
    """rows: list of [label, v1, v2]; lays cells out top-to-bottom."""
    cells = []
    n_rows, n_cols = len(rows), len(rows[0])
    col_x = [(60, 240), (300, 360), (420, 480)]  # (l, r) per column
    rh = 18.0
    for ri, row in enumerate(rows):
        yt = top_y - ri * rh  # BOTTOMLEFT: higher on page = larger y
        yb = yt - rh
        for ci, txt in enumerate(row):
            l, r_ = col_x[ci]
            cells.append(cell(txt, ri, ci, header=(ri == 0), l=l, t=yt, r_=r_, b=yb))
    data = TableData(num_rows=n_rows, num_cols=n_cols, table_cells=cells)
    bbox = BoundingBox(l=55, t=top_y + 2, r=485, b=top_y - n_rows * rh - 2,
                       coord_origin=CoordOrigin.BOTTOMLEFT)
    return data, bbox


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", default="tests/data/sample.docling.json")
    args = ap.parse_args()

    doc = DoclingDocument(name="quarry-docling-sample")
    doc.add_page(page_no=1, size=Size(width=PAGE_W, height=PAGE_H))

    # Table 1: a clean income statement (reconciles).
    d1, b1 = table_from(
        [
            ["Line item", "FY2024", "FY2023"],
            ["Product revenue", "1,200", "1,000"],
            ["Service revenue", "800", "650"],
            ["Total revenue", "2,000", "1,650"],
        ],
        top_y=700,
    )
    doc.add_table(data=d1, prov=ProvenanceItem(page_no=1, bbox=b1, charspan=(0, 0)))

    # Table 2: a balance sheet with a BROAD arithmetic failure — BOTH column
    # totals are wrong (7,550 -> 9,999 and 6,050 -> 9,999), the signature of a
    # real mis-parse (a column shift breaks every column). A single wrong total
    # while another column reconciles would instead be a non-additive total, which
    # the detectors now (correctly) do not flag.
    d2, b2 = table_from(
        [
            ["Item", "2024", "2023"],
            ["Cash and equivalents", "5,400", "4,100"],
            ["Accounts receivable", "1,250", "1,100"],
            ["Inventory", "900", "850"],
            ["Total current assets", "9,999", "9,999"],
        ],
        top_y=560,
    )
    doc.add_table(data=d2, prov=ProvenanceItem(page_no=1, bbox=b2, charspan=(0, 0)))

    with open(args.out, "w") as fh:
        json.dump(doc.export_to_dict(), fh, indent=2)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
