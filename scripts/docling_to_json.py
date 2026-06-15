#!/usr/bin/env python3
"""
docling_to_json.py - Convert a PDF with Docling and dump DoclingDocument JSON.

Docling is a real, table-producing parser: it does layout + table-structure
recognition and emits tables with cells and bounding boxes. That output feeds
Quarry's Docling adapter (src/docling.rs) which maps it onto the Artifact model —
bypassing .qdoc and the cheap reconstructor entirely. The detector/eval core then
runs unchanged. This is the integration pattern for any such parser (Reducto,
LlamaParse, …): a thin adapter to Artifact, not a rewrite to .qdoc.

Docling is HEAVY (pulls torch + models, downloads weights on first run), so it is
NOT a project dependency. Run it on demand with uv's --with:

  uv run --with docling scripts/docling_to_json.py input/foo.pdf -o corpus/foo.docling.json

Then import + check:
  cargo run -- import-docling corpus/foo.docling.json --pdf input/foo.pdf --out corpus/foo.docling.artifacts
  cargo run -- check corpus/foo.docling.artifacts
"""
from __future__ import annotations

import argparse
import json
import sys


def main():
    ap = argparse.ArgumentParser(description="Convert a PDF to DoclingDocument JSON.")
    ap.add_argument("pdf")
    ap.add_argument("-o", "--out", required=True)
    args = ap.parse_args()

    try:
        from docling.document_converter import DocumentConverter
    except ImportError:
        sys.exit("Docling not installed. Run via:  uv run --with docling scripts/docling_to_json.py ...")

    result = DocumentConverter().convert(args.pdf)
    doc = result.document.export_to_dict()
    with open(args.out, "w") as fh:
        json.dump(doc, fh)
    print(f"{args.pdf} -> {args.out}: {len(doc.get('tables', []))} table(s)", file=sys.stderr)


if __name__ == "__main__":
    main()
