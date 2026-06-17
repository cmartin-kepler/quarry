#!/usr/bin/env python3
"""
text_tables.py - Detect tables from TEXT / markdown (no coordinates).

A different parsing PATH from the geometric reconstructor: instead of clustering
glyph boxes, this reads a parser's *text* output and recovers tables from it —
either markdown pipe tables, or (what LiteParse's `--format text` actually emits)
space-aligned fixed-width columns. It works purely on characters, so it has no
bboxes; it operates at page granularity.

Method for space-aligned text: within a block of consecutive "tabular-looking"
lines, a column boundary is a character column that is whitespace in EVERY line
(a vertical whitespace channel). Split each line at those channels into cells.

Usage:
  from text_tables import detect_tables
  grids = detect_tables(page_text)   # -> list of 2D string grids
"""
from __future__ import annotations

import re


def _markdown_tables(text: str) -> list[list[list[str]]]:
    grids, cur = [], []
    for line in text.splitlines():
        if line.strip().startswith("|") and line.count("|") >= 2:
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if not re.fullmatch(r"[\s:|-]+", line):  # skip the |---|---| rule
                cur.append(cells)
        else:
            if len(cur) >= 2:
                grids.append(cur)
            cur = []
    if len(cur) >= 2:
        grids.append(cur)
    return grids


def _is_tabular(line: str) -> bool:
    """A row with >=1 internal multi-space gap (>=2 segments) — so two-column
    tables (Date | Value) are detected, not just 3+ column ones."""
    return len(re.findall(r"\S(?:  +)\S", line)) >= 1 and bool(line.strip())


def _columns_from_text_block(lines: list[str]) -> list[list[str]]:
    width = max(len(ln) for ln in lines)
    padded = [ln.ljust(width) for ln in lines]
    # A boundary column is whitespace in every line.
    is_sep = [all(p[c] == " " for p in padded) for c in range(width)]
    # Cut points = the center of each whitespace channel that is >=2 wide.
    cuts, c = [], 0
    while c < width:
        if is_sep[c]:
            start = c
            while c < width and is_sep[c]:
                c += 1
            if c - start >= 2:
                cuts.append((start, c))
        else:
            c += 1
    bounds = [0] + [(a + b) // 2 for (a, b) in cuts] + [width]
    grid = []
    for p in padded:
        row = [p[bounds[i]:bounds[i + 1]].strip() for i in range(len(bounds) - 1)]
        grid.append(row)
    # Drop all-empty columns (leading/trailing channel artifacts).
    keep = [j for j in range(len(grid[0])) if any(r[j] for r in grid)]
    grid = [[r[j] for j in keep] for r in grid]
    return grid


def _spacealigned_tables(text: str) -> list[list[list[str]]]:
    lines = text.splitlines()
    grids, block, gap = [], [], 0
    for ln in lines:
        if _is_tabular(ln):
            block.append(ln)
            gap = 0
        elif ln.strip() and block:
            # a one-line non-tabular interruption (e.g. a wrapped label) is ok
            gap += 1
            if gap <= 1:
                block.append(ln)
            else:
                if len(block) >= 2:
                    grids.append(block)
                block, gap = [], 0
        else:
            if len(block) >= 2:
                grids.append(block)
            block, gap = [], 0
    if len(block) >= 2:
        grids.append(block)

    out = []
    for blk in grids:
        grid = _columns_from_text_block(blk)
        if grid and len(grid[0]) >= 2 and len(grid) >= 2:
            out.append(grid)
    return out


def detect_tables(text: str) -> list[list[list[str]]]:
    """Return tables found in the text (markdown first; else space-aligned)."""
    md = _markdown_tables(text)
    if md:
        return md
    return _spacealigned_tables(text)


_SYMS = set("$€£¥()-—–")  # currency / sign glyphs that get split into their own column


def _rect(grid):
    w = max((len(r) for r in grid), default=0)
    return [[(c or "").strip() for c in r] + [""] * (w - len(r)) for r in grid]


def canonicalize(grid: list[list[str]]):
    """Clean structural extraction defects into a canonical grid, returning
    (grid, changes). The repairs (in order):

      1. trim every cell; pad ragged rows to a rectangle
      2. re-merge symbol-only columns — a column of just '$' (or '(') belongs to
         the number on its RIGHT; a column of just ')' belongs to the number on its
         LEFT. Column-based extractors routinely split these off, which breaks
         currency/negative materialization. Reuniting them is the highest-value fix.
      3. drop columns that are empty in every data row
      4. drop rows that are empty in every column

    Structural only — never edits a real value, so it's safe to try before a costly
    re-parse. Whether it helped is judged downstream by arithmetic reconciliation."""
    if not grid:
        return grid, []
    g = _rect(grid)
    changes = []
    body = g[1:] if len(g) > 1 else g
    ncols = len(g[0])

    def symcol(c):  # data cells are blank or a lone split-off glyph, with ≥1 glyph
        vals = [row[c] for row in body]
        seen = [v for v in vals if v]
        return bool(seen) and all(v in _SYMS for v in seen)

    # 2. fold symbol-only columns into their numeric neighbour
    merged = set()
    for c in range(ncols):
        if c in merged or not symcol(c):
            continue
        glyph = next(row[c] for row in g[1:] if row[c]) if len(g) > 1 else ""
        right = glyph in "$€£¥("              # a prefix → attach to the column right
        tgt = c + 1 if right else c - 1
        if not (0 <= tgt < ncols):
            continue
        for row in g:
            if row[c]:
                row[tgt] = (row[c] + row[tgt]) if right else (row[tgt] + row[c])
            row[c] = ""
        merged.add(c)
        changes.append(f"merged symbol column {c} ('{glyph}…') into column {tgt}")

    # 3. drop all-empty columns (the now-emptied symbol columns, and any others)
    keep = [c for c in range(ncols) if any(row[c] for row in g)]
    if len(keep) < ncols:
        g = [[row[c] for c in keep] for row in g]
        changes.append(f"dropped {ncols - len(keep)} empty column(s)")

    # 4. drop all-empty rows
    before = len(g)
    g = [row for row in g if any(c for c in row)]
    if len(g) < before:
        changes.append(f"dropped {before - len(g)} empty row(s)")

    return g, changes


def to_markdown(grid: list[list[str]]) -> str:
    """Render a grid as a GitHub-flavored markdown pipe table (the '=> md' step).
    Re-parsing this with _markdown_tables recovers the grid ('md to table')."""
    n_cols = max(len(r) for r in grid)
    def row(cells):
        cells = [c.replace("|", "\\|") for c in (cells + [""] * (n_cols - len(cells)))]
        return "| " + " | ".join(cells) + " |"
    out = [row(grid[0]), "| " + " | ".join(["---"] * n_cols) + " |"]
    out += [row(r) for r in grid[1:]]
    return "\n".join(out)


def to_html(grid: list[list[str]]) -> str:
    import html as H
    n_cols = max(len(r) for r in grid)
    rows = []
    for i, r in enumerate(grid):
        tag = "th" if i == 0 else "td"
        cells = "".join(f"<{tag}>{H.escape(c)}</{tag}>" for c in (r + [""] * (n_cols - len(r))))
        rows.append(f"  <tr>{cells}</tr>")
    return "<table>\n" + "\n".join(rows) + "\n</table>"


if __name__ == "__main__":
    import sys
    txt = open(sys.argv[1]).read()
    for i, g in enumerate(detect_tables(txt)):
        print(f"--- table {i}: {len(g)}x{max(len(r) for r in g)} ---")
        for r in g[:8]:
            print("  | " + " | ".join(c[:16] for c in r))
