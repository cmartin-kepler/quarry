#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["docling"]
# ///
"""Docling sidecar for the Rust `docling` extractor.

Run Docling on a PDF and print the `DoclingDocument` JSON to stdout — the schema
the crate's `docling::artifacts_from_docling` adapter consumes (pages with sizes,
tables with prov + data.table_cells, bottom-left bboxes).

Named run_docling.py (NOT docling_parse.py) deliberately: `docling_parse` is one
of docling's own dependencies, so a same-named script on sys.path shadows the
package and triggers a circular import.

Usage: run_docling.py <pdf>
"""
import sys

from docling.document_converter import DocumentConverter


def main() -> None:
    result = DocumentConverter().convert(sys.argv[1])
    print(result.document.model_dump_json())


if __name__ == "__main__":
    main()
