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
