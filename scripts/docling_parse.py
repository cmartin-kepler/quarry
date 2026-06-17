#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling"]
# ///
"""Docling sidecar for the Rust `docling` extractor.

Run Docling on a PDF and print the `DoclingDocument` JSON to stdout — the schema
the crate's `docling::artifacts_from_docling` adapter consumes (pages with sizes,
tables with prov + data.table_cells, bottom-left bboxes).

Usage: docling_parse.py <pdf>
"""
import sys

from docling.document_converter import DocumentConverter


def main() -> None:
    result = DocumentConverter().convert(sys.argv[1])
    print(result.document.model_dump_json())


if __name__ == "__main__":
    main()
