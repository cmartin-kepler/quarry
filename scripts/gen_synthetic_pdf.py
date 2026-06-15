#!/usr/bin/env python3
"""
gen_synthetic_pdf.py - Generate a born-digital test PDF (tables + a figure) with
known ground-truth cell values, using reportlab.

This is the no-TeX fallback for the LaTeX pipeline in scripts/latex/: it produces
the same kind of artifact (a real born-digital PDF with a true text layer and a
bar-chart figure) so the bridge + Quarry eval can be exercised anywhere, without
a TeX install. The table VALUES are emitted to <name>.cells.json in document
order; bbox anchors are filled in later from the compiled PDF by build_truth.py.

The tables use right-aligned numeric columns of varying width on purpose: that is
what defeats a cheap parser's global column model and yields realistic
clean-looking-but-wrong reconstructions -- the silent failures Quarry hunts.

Usage (Python deps via uv: run `uv sync` once at the repo root):
  uv run scripts/gen_synthetic_pdf.py --out corpus/synthetic
  # produces corpus/synthetic.pdf and corpus/synthetic.cells.json
"""
from __future__ import annotations

import argparse
import json
import os

from reportlab.lib import colors
from reportlab.lib.pagesizes import letter
from reportlab.lib.styles import getSampleStyleSheet
from reportlab.lib.units import inch
from reportlab.graphics.shapes import Drawing
from reportlab.graphics.charts.barcharts import VerticalBarChart
from reportlab.platypus import SimpleDocTemplate, Paragraph, Spacer, Table, TableStyle

# ---- The document content (single source of truth) ------------------------
# Each table: header row + data rows + a reconciling total row.

INCOME = {
    "name": "income_statement",
    "difficulty": "clean-digital",
    "column_types": ["text", "currency", "currency"],
    "cells": [
        ["Line item", "FY2024", "FY2023"],
        ["Product revenue", "1,200", "1,000"],
        ["Service revenue", "800", "650"],
        ["Total revenue", "2,000", "1,650"],
        ["Cost of revenue", "1,100", "950"],
        ["Gross profit", "900", "700"],
    ],
}

BALANCE = {
    "name": "balance_sheet",
    "difficulty": "right-aligned-varying-width",
    "column_types": ["text", "currency", "currency"],
    "cells": [
        ["Item", "2024", "2023"],
        ["Cash and equivalents", "5,400", "4,100"],
        ["Accounts receivable", "1,250", "1,100"],
        ["Inventory", "900", "850"],
        ["Total current assets", "7,550", "6,050"],
    ],
}

TABLES = [INCOME, BALANCE]


def make_table_flowable(spec: dict) -> Table:
    data = spec["cells"]
    # Generous, well-separated columns: keeps inter-column gaps far wider than
    # the parser's cell-merge threshold, so the cheap parser fails (when it does)
    # via the realistic right-aligned-number column split, not row-collapse.
    n_cols = len(data[0])
    col_widths = [2.6 * inch] + [1.2 * inch] * (n_cols - 1)
    t = Table(data, colWidths=col_widths, hAlign="LEFT")
    t.setStyle(
        TableStyle(
            [
                # Ruled grid so pdfplumber's line-based detector finds the region.
                ("GRID", (0, 0), (-1, -1), 0.5, colors.black),
                ("FONTNAME", (0, 0), (-1, 0), "Helvetica-Bold"),
                ("FONTSIZE", (0, 0), (-1, -1), 10),
                # Right-align numeric columns -> varying x0 -> realistic failures.
                ("ALIGN", (1, 0), (-1, -1), "RIGHT"),
                ("ALIGN", (0, 0), (0, -1), "LEFT"),
                ("LEFTPADDING", (0, 0), (-1, -1), 6),
                ("RIGHTPADDING", (0, 0), (-1, -1), 6),
            ]
        )
    )
    return t


def make_bar_chart() -> Drawing:
    d = Drawing(360, 180)
    chart = VerticalBarChart()
    chart.x, chart.y = 30, 20
    chart.width, chart.height = 300, 140
    # Total revenue FY2023 vs FY2024 (matches the income statement).
    chart.data = [[1650, 2000]]
    chart.categoryAxis.categoryNames = ["FY2023", "FY2024"]
    chart.valueAxis.valueMin = 0
    chart.valueAxis.valueMax = 2400
    chart.valueAxis.valueStep = 600
    chart.bars[0].fillColor = colors.HexColor("#4472C4")
    d.add(chart)
    return d


def build(out_base: str):
    pdf_path = out_base + ".pdf"
    cells_path = out_base + ".cells.json"
    os.makedirs(os.path.dirname(out_base) or ".", exist_ok=True)

    styles = getSampleStyleSheet()
    doc = SimpleDocTemplate(pdf_path, pagesize=letter,
                            topMargin=0.8 * inch, bottomMargin=0.8 * inch)
    story = [
        Paragraph("Synthetic Quarterly Report (Quarry test fixture)", styles["Title"]),
        Spacer(1, 0.3 * inch),
        Paragraph("Condensed Statement of Operations", styles["Heading2"]),
        make_table_flowable(INCOME),
        Spacer(1, 0.4 * inch),
        Paragraph("Condensed Balance Sheet", styles["Heading2"]),
        make_table_flowable(BALANCE),
        Spacer(1, 0.4 * inch),
        Paragraph("Figure 1. Total revenue by fiscal year ($M)", styles["Heading2"]),
        make_bar_chart(),
    ]
    doc.build(story)

    with open(cells_path, "w") as fh:
        json.dump({"tables": TABLES}, fh, indent=2)

    print(f"wrote {pdf_path} and {cells_path}")


def main():
    ap = argparse.ArgumentParser(description="Generate a synthetic born-digital test PDF.")
    ap.add_argument("--out", default="corpus/synthetic", help="output path base (no extension)")
    args = ap.parse_args()
    build(args.out)


if __name__ == "__main__":
    main()
