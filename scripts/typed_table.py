#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["polars>=1", "duckdb>=1", "pyarrow>=15"]
# ///
"""
typed_table.py - Materialize an HtmlTable into a TypedTable you can do math on.

This is the NON-REVERSIBLE transform at the end of the lineage:

    PDF --reconstruct-checked--> HtmlTable --materialize--> TypedTable --> DuckDB / Polars
         (glyphs)                (surface strings,          (-12.5, 1200000.0,
                                  anchored to glyphs)         typed columns)

`HtmlTable -> TypedTable` is not invertible: you cannot recover "1.2M" from
1200000.0 (it could have been "1,200,000" or "1200K"). That is fine. Reversibility
does NOT come from inverting this step; it comes from:

  * keeping the parent  - the HtmlTable generation still exists and is still
    anchored to the PDF glyphs. Rollback = drop the TypedTable, point back at the
    HtmlTable. There is no "down" function.
  * per-cell provenance - every typed value carries its surface string (and, if
    given, its source bbox) plus the list of transforms applied. You never
    reconstruct the dataframe back into text; you check FORWARD that each
    materialized number is consistent with the surface form it came from
    (sign via the same logic reconstruction uses, scale via the documented
    multiplier, totals via arithmetic reconciliation upstream).

So the materialization is auditable without being reversible. This module is the
artifact + the forward materialization check + the exporters.

  uv run scripts/typed_table.py demo
  uv run scripts/typed_table.py html parse.html --header 1 --table seg --duckdb
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from dataclasses import dataclass, field

# Accounting "no value" markers - legitimately empty in a numeric column.
PLACEHOLDERS = {"-", "—", "–", "--", "n/a", "na", "nm", "n/m", ""}
SCALE = {"k": 1e3, "m": 1e6, "b": 1e9, "t": 1e12}
CURRENCY = "$€£¥"


# ---------------------------------------------------------------------------
# 1. Materialize a single accounting-coded number from its surface string
# ---------------------------------------------------------------------------

@dataclass
class Num:
    value: float            # the materialized, signed, scaled value
    negative: bool
    percent: bool
    currency: bool
    scale: float            # 1.0, or 1e3 / 1e6 / 1e9 / 1e12
    integral: bool          # safe to store as an integer
    raw: str
    transforms: list[str]   # what we did to get here (the audit trail)


def parse_number(raw: str) -> Num | None:
    """Parse one accounting number, or None if the cell isn't a number.

    Handles, recording each step: parentheses negative, leading/trailing minus,
    CR/DR suffix, currency symbols, percent, thousands separators, and K/M/B/T
    scale suffixes. Letters that aren't a scale/CR/DR suffix mean it's a label,
    not a number (so "Revenue" and "Q1" stay text)."""
    s = (raw or "").strip()
    if s.lower() in PLACEHOLDERS:
        return None
    t = s
    tf: list[str] = []
    neg = False

    if t.startswith("(") and ")" in t:
        neg = True
        t = t[1:t.rfind(")")]
        tf.append("paren→neg")

    currency = False
    if any(c in t for c in CURRENCY):
        currency = True
        for c in CURRENCY:
            t = t.replace(c, "")
        tf.append("strip currency")

    percent = False
    if t.strip().endswith("%"):
        percent = True
        t = t.strip()[:-1]
        tf.append("percent")

    t = t.strip()
    low = t.lower()
    if low.endswith("cr"):
        neg = True
        t = t[:-2].strip()
        tf.append("CR→neg")
    elif low.endswith("dr"):
        t = t[:-2].strip()
    if t.endswith("-"):
        neg = True
        t = t[:-1].strip()
        tf.append("trailing−→neg")
    if t.startswith("-"):
        neg = True
        t = t[1:].strip()

    scale = 1.0
    if len(t) > 1 and t[-1].lower() in SCALE and any(c.isdigit() for c in t[:-1]):
        scale = SCALE[t[-1].lower()]
        tf.append(f"scale ×{scale:g}")
        t = t[:-1].strip()

    t = t.replace(",", "")
    if not t or not any(c.isdigit() for c in t):
        return None
    # Anything alphabetic left over => it was a label, not a number.
    if any(c.isalpha() for c in t):
        return None
    try:
        v = float(t)
    except ValueError:
        return None

    val = v * scale * (-1 if neg else 1)
    integral = ("." not in t) and scale == 1.0 and val == int(val)
    if neg:
        tf.append("signed")
    return Num(val, neg, percent, currency, scale, integral, raw, tf)


# ---------------------------------------------------------------------------
# 2. Cells, columns, the TypedTable artifact
# ---------------------------------------------------------------------------

@dataclass
class ProvCell:
    """A materialized value plus the provenance that makes it auditable."""
    value: object               # int | float | str | None
    surface: str                # the exact source string (the retained 'down')
    transforms: list[str]
    bbox: tuple | None = None    # source glyph box, if known
    ok: bool = True              # False = non-empty but failed to materialize


@dataclass
class Column:
    name: str
    dtype: str                  # "int" | "float" | "percent" | "currency" | "label"
    values: list                # coerced to dtype (None = null)
    cells: list[ProvCell]


@dataclass
class TypedTable:
    columns: list[Column]
    source: dict | None = None   # parent HtmlTable id / anchor (lineage pointer)
    violations: list[str] = field(default_factory=list)  # numeric col, non-parsing cell

    # -- introspection ------------------------------------------------------
    @property
    def names(self) -> list[str]:
        return [c.name for c in self.columns]

    @property
    def n_rows(self) -> int:
        return max((len(c.values) for c in self.columns), default=0)

    def report(self) -> str:
        out = [f"TypedTable: {len(self.columns)} cols × {self.n_rows} rows"]
        for c in self.columns:
            nulls = sum(1 for v in c.values if v is None)
            tf = Counter(t for cell in c.cells for t in cell.transforms)
            bad = sum(1 for cell in c.cells if not cell.ok)
            tfs = " ".join(f"{k}×{v}" for k, v in tf.items()) or "—"
            line = f"  {c.name:<22} {c.dtype:<8} nulls={nulls}"
            if bad:
                line += f" !{bad} non-numeric"
            out.append(f"{line}   [{tfs}]")
        if self.violations:
            out.append("  materialization violations:")
            out += [f"    - {v}" for v in self.violations]
        return "\n".join(out)

    # -- reshape ------------------------------------------------------------
    def melt(self) -> "TypedTable":
        """Tidy/long form: keep the label (non-numeric) columns as identifiers and
        unpivot every numeric column into ('variable', 'value') pairs — one row per
        original cell. The shape analysts actually want (one value per row, groupable
        by variable). Numeric dtypes collapse to a single 'value' float column."""
        ids = [c for c in self.columns if c.dtype == "label"]
        vals = [c for c in self.columns if c.dtype != "label"]
        if not vals:
            return self
        id_cols = [Column(c.name, "label", [], []) for c in ids]
        var_col = Column("variable", "label", [], [])
        val_col = Column("value", "float", [], [])
        for r in range(self.n_rows):
            for vc in vals:
                v = vc.values[r] if r < len(vc.values) else None
                if v is None:
                    continue
                for ic, src in zip(id_cols, ids):
                    sv = src.values[r] if r < len(src.values) else None
                    ic.values.append(sv); ic.cells.append(ProvCell(sv, sv or "", []))
                var_col.values.append(vc.name); var_col.cells.append(ProvCell(vc.name, vc.name, []))
                fv = float(v)
                val_col.values.append(fv)
                val_col.cells.append(ProvCell(fv, str(v), ["melt"]))
        return TypedTable(id_cols + [var_col, val_col], source=self.source)

    # -- exporters ----------------------------------------------------------
    def _schema(self):
        import polars as pl
        m = {"int": pl.Int64, "float": pl.Float64, "percent": pl.Float64,
             "currency": pl.Float64, "label": pl.Utf8}
        return {c.name: m[c.dtype] for c in self.columns}

    def to_polars(self):
        """A Polars DataFrame with materialized dtypes (Int64/Float64/Utf8)."""
        import polars as pl
        data = {c.name: c.values for c in self.columns}
        return pl.DataFrame(data, schema=self._schema())

    def to_arrow(self):
        return self.to_polars().to_arrow()

    def to_pandas(self):
        return self.to_polars().to_pandas()

    def to_duckdb(self, con=None, table: str = "parsed"):
        """Create (or replace) `table` in a DuckDB connection. Returns the con."""
        import duckdb
        con = con or duckdb.connect()
        con.register("__typed_src", self.to_arrow())
        con.execute(f'CREATE OR REPLACE TABLE "{table}" AS SELECT * FROM __typed_src')
        con.unregister("__typed_src")
        return con


# ---------------------------------------------------------------------------
# 3. The materialize transform: HtmlTable grid -> TypedTable
# ---------------------------------------------------------------------------

def _column_name(grid, header_rows, c, used):
    parts = []
    for r in range(header_rows):
        if c < len(grid[r]):
            t = grid[r][c].strip()
            if t and t not in parts:
                parts.append(t)
    name = " ".join(parts).strip() or f"col_{c}"
    base, i = name, 2
    while name in used:
        name = f"{base}_{i}"
        i += 1
    used.add(name)
    return name


def materialize(grid: list[list[str]], header_rows: int = 1,
                anchors: list[list[tuple]] | None = None,
                source: dict | None = None) -> TypedTable:
    """Materialize a string grid into typed columns. `anchors`, if given, is a
    parallel grid of source bboxes used for per-cell provenance."""
    n_cols = max((len(r) for r in grid), default=0)
    data_rows = list(range(header_rows, len(grid)))
    used: set[str] = set()
    columns, violations = [], []

    for c in range(n_cols):
        surfaces = [(grid[r][c].strip() if c < len(grid[r]) else "") for r in data_rows]
        parsed = [parse_number(s) for s in surfaces]
        meaningful = [i for i, s in enumerate(surfaces) if s.lower() not in PLACEHOLDERS]
        nums = [i for i in meaningful if parsed[i] is not None]

        is_numeric = bool(nums) and len(nums) * 2 >= len(meaningful)  # majority parse
        name = _column_name(grid, header_rows, c, used)

        if is_numeric:
            is_percent = sum(1 for i in nums if parsed[i].percent) * 2 >= len(nums)
            has_cur = any(parsed[i].currency for i in nums)
            all_int = all(parsed[i].integral and not parsed[i].percent for i in nums)
            dtype = "percent" if is_percent else ("int" if all_int else
                                                  ("currency" if has_cur else "float"))
        else:
            dtype = "label"

        values, cells = [], []
        for k, r in enumerate(data_rows):
            s = surfaces[k]
            box = anchors[r][c] if anchors and r < len(anchors) and c < len(anchors[r]) else None
            p = parsed[k]
            if dtype == "label":
                v = s if s else None
                cells.append(ProvCell(v, s, [], box, ok=True))
                values.append(v)
            elif s.lower() in PLACEHOLDERS:
                cells.append(ProvCell(None, s, [], box, ok=True))
                values.append(None)
            elif p is None:
                # non-empty cell that did not materialize in a numeric column:
                # this is the "stray text in a numeric column" mis-parse signal.
                violations.append(f"{name!r} row {k}: {s!r} is not a number")
                cells.append(ProvCell(None, s, [], box, ok=False))
                values.append(None)
            else:
                v = int(p.value) if dtype == "int" else float(p.value)
                cells.append(ProvCell(v, s, p.transforms, box, ok=True))
                values.append(v)

        columns.append(Column(name, dtype, values, cells))

    return TypedTable(columns, source=source, violations=violations)


# ---------------------------------------------------------------------------
# 4. Reshaping: section-to-column (un-nest group headers) and tidy/melt
# ---------------------------------------------------------------------------

def _is_section_row(row, ncols) -> bool:
    """A group-header row: a non-empty first cell, every other cell blank, and the
    label itself isn't a number (so a one-number row isn't mistaken for a header)."""
    if not row or not row[0].strip():
        return False
    if parse_number(row[0]) is not None:
        return False
    return all(not (row[c].strip() if c < len(row) else "") for c in range(1, ncols))


def section_to_column(grid, header_rows=1):
    """Promote nested section headers to a real column. A row that is just a label
    with no values (e.g. 'Revenues:') heads the rows beneath it until the next such
    row; we drop those header rows and prepend a 'Section' column carrying the group
    onto every data row. Returns (grid, header_rows, found). No-op (found=False) when
    the table isn't sectioned, so it's always safe to apply."""
    ncols = max((len(r) for r in grid), default=0)
    body = grid[header_rows:]
    secrows = [i for i, r in enumerate(body) if _is_section_row(r, ncols)]
    # need ≥1 section header that actually groups rows beneath it
    if not secrows or secrows == list(range(len(secrows))):
        return grid, header_rows, False

    out = [["Section"] + [(grid[r][c].strip() if c < len(grid[r]) else "")
                          for c in range(ncols)] for r in range(header_rows)]
    cur = ""
    for r in body:
        cells = [(r[c].strip() if c < len(r) else "") for c in range(ncols)]
        if _is_section_row(r, ncols):
            cur = cells[0]
            continue
        out.append([cur] + cells)
    return out, header_rows, True


def from_html(html: str, header_rows: int | None = None, source: dict | None = None,
              sections: bool = False) -> TypedTable:
    """Materialize directly from an HtmlTable's HTML, reusing the reconstruction
    validator's dense-grid parser (so rowspan/colspan are expanded consistently)."""
    sys.path.insert(0, __file__.rsplit("/", 1)[0])
    import recon_validate as rv
    grid, hdr = rv.parse_html_grid(html)
    hr = header_rows if header_rows is not None else (max(hdr) + 1 if hdr else 1)
    if sections:
        grid, hr, _ = section_to_column(grid, hr)
    return materialize(grid, hr, source=source)


# ---------------------------------------------------------------------------
# Demo / CLI
# ---------------------------------------------------------------------------

_DEMO = [
    ["Segment",       "Revenue", "Op income", "Margin", "YoY",   "Mkt cap"],
    ["Entertainment", "$10,341", "1,200",     "18.9%",  "(902)", "1.2B"],
    ["Sports",        "4,540",   "920",       "20.3%",  "152",   "880M"],
    ["Experiences",   "8,430",   "2,210",     "26.2%",  "(30)",  "3.4B"],
]


def run_demo():
    tt = materialize(_DEMO, header_rows=1)
    print("=== materialize: surface strings -> typed columns ===")
    print(tt.report())

    print("\n=== provenance (the retained 'down' for one negative + one scaled cell) ===")
    for cname in ("YoY", "Mkt cap"):
        col = next(c for c in tt.columns if c.name == cname)
        cell = next(c for c in col.cells if c.value is not None)
        print(f"  {cname:<9} {cell.surface!r:>9} -> {cell.value!r:<12} via {cell.transforms}")

    print("\n=== as a Polars DataFrame (typed, math-ready) ===")
    df = tt.to_polars()
    print(df)
    print("  dtypes:", {n: str(t) for n, t in zip(df.columns, df.dtypes)})
    print("  sum(YoY) =", df["YoY"].sum(), " (negatives preserved: -902 + 152 - 30)")

    print("\n=== as a DuckDB table (SQL aggregation) ===")
    con = tt.to_duckdb(table="segments")
    q = ('SELECT round(avg("Margin"),1) AS avg_margin, '
         'sum("Revenue") AS total_revenue, sum("Mkt cap") AS total_mktcap '
         'FROM segments')
    print("  " + q)
    print(con.sql(q))


def main():
    ap = argparse.ArgumentParser(description="Materialize an HtmlTable into a TypedTable.")
    sub = ap.add_subparsers(dest="mode", required=True)
    sub.add_parser("demo", help="run the built-in financial example")
    h = sub.add_parser("html", help="materialize an HTML table file")
    h.add_argument("path")
    h.add_argument("--header", type=int, default=None, help="number of header rows")
    h.add_argument("--table", default="parsed", help="DuckDB table name")
    h.add_argument("--duckdb", action="store_true", help="also create a DuckDB table and describe it")
    h.add_argument("--json", action="store_true", help="emit the materialization report as JSON")

    args = ap.parse_args()
    if args.mode == "demo":
        run_demo()
    elif args.mode == "html":
        tt = from_html(open(args.path).read(), header_rows=args.header)
        if args.json:
            print(json.dumps({"columns": [{"name": c.name, "dtype": c.dtype,
                              "values": c.values} for c in tt.columns],
                              "violations": tt.violations}, default=str, indent=2))
        else:
            print(tt.report())
            print()
            print(tt.to_polars())
        if args.duckdb:
            con = tt.to_duckdb(table=args.table)
            print(f"\nDuckDB table {args.table!r}:")
            print(con.sql(f'DESCRIBE "{args.table}"'))


if __name__ == "__main__":
    main()
