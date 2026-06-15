#!/usr/bin/env python3
"""
build_truth.py - Assemble a Quarry ground-truth file from known cell values + the
table regions detected in the compiled PDF's `.qdoc`.

A generator (LaTeX or reportlab) emits the logical table VALUES in document order
(<name>.cells.json). The bridge (pdf_to_qdoc.py) emits the PDF's spans and
detected table regions (<name>.qdoc). This script pairs them positionally
(i-th known table <-> i-th detected region, ordered page-asc then top-asc) and
writes <name>.truth.json with each table's correct cells + its source anchor
(page, PDF-coordinate bbox) -- the anchor format the design brief asks for.

If the detected region count differs from the known table count, it pairs as many
as it can and WARNS (silent truncation would read as full coverage when it isn't).

Usage:
  python3 build_truth.py --cells name.cells.json --qdoc name.qdoc -o name.truth.json
"""
from __future__ import annotations

import argparse
import json
import sys


def ordered_regions(qdoc: dict) -> list[tuple[int, list[float]]]:
    regions = []
    for page in qdoc["pages"]:
        for r in page["table_regions"]:
            regions.append((page["page"], r["bbox"]))
    # page ascending, then top (y0) ascending.
    regions.sort(key=lambda pr: (pr[0], pr[1][1]))
    return regions


def main():
    ap = argparse.ArgumentParser(description="Build Quarry truth.json from cells + qdoc regions.")
    ap.add_argument("--cells", required=True)
    ap.add_argument("--qdoc", required=True)
    ap.add_argument("-o", "--out", required=True)
    args = ap.parse_args()

    cells = json.load(open(args.cells))["tables"]
    qdoc = json.load(open(args.qdoc))
    regions = ordered_regions(qdoc)

    if len(regions) != len(cells):
        print(
            f"WARNING: {len(cells)} known table(s) but {len(regions)} detected region(s); "
            f"pairing {min(len(cells), len(regions))}. Unpaired tables are dropped "
            f"from truth -- detection likely split/merged or missed a table.",
            file=sys.stderr,
        )

    out_tables = []
    for tbl, (page, bbox) in zip(cells, regions):
        out_tables.append(
            {
                "name": tbl["name"],
                "page": page,
                "bbox": bbox,
                "column_types": tbl.get("column_types", []),
                "cells": tbl["cells"],
            }
        )

    with open(args.out, "w") as fh:
        json.dump({"tables": out_tables}, fh, indent=2)
    print(f"wrote {args.out}: {len(out_tables)} table(s)", file=sys.stderr)


if __name__ == "__main__":
    main()
