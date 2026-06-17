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


_NUMCELL = re.compile(r"^[\(\-]?\$?[\d,]+\.?\d*%?\)?$")
_SYM = set("$€£¥()-—–")


def _merge_symbol_cols(grid, header_rows):
    """Merge a column whose DATA cells are only currency/paren glyphs into its
    numeric neighbour ('$'/'(' → right, ')' → left). A split-off symbol column is a
    structuring artifact, not a real column."""
    if not grid:
        return grid
    n = max(len(r) for r in grid)
    grid = [r + [""] * (n - len(r)) for r in grid]
    for c in range(n):
        seen = [grid[r][c] for r in range(min(header_rows, len(grid)), len(grid)) if grid[r][c]]
        if not seen or not all(v in _SYM for v in seen):
            continue
        tgt = c + 1 if seen[0] in "$€£¥(" else c - 1
        if not 0 <= tgt < n:
            continue
        for r in range(len(grid)):
            if grid[r][c]:
                grid[r][tgt] = (grid[r][c] + grid[r][tgt]) if tgt > c else (grid[r][tgt] + grid[r][c])
                grid[r][c] = ""
    return grid


def structure_words(words, row_tol: float = 3, col_gap: float = 6):
    """Cluster a region's words (pdfplumber dicts with text/x0/x1/top) into a table
    grid by GEOMETRY: rows by vertical position, columns by gaps in the horizontal
    word projection. Geometry-based so it never splits a word (words are atomic) and
    column boundaries come from real whitespace gaps.

    Multi-row headers are handled: the leading rows (until the first row with ≥2
    numeric cells) are the header. A header word that SPANS several data columns is
    spread across each of them (a `colspan` group); a header that just wraps over
    lines stacks down its one column. So each data column ends up with a tuple of
    header levels — the multi-index — which to_html_headed renders with colspan and
    materialize flattens into one column name. Returns (grid, header_rows)."""
    if not words:
        return [], 0
    ws = sorted(words, key=lambda w: (w["top"], w["x0"]))
    rows, cur, y0 = [], [], None
    for w in ws:
        if y0 is None or w["top"] - y0 <= row_tol:
            cur.append(w)
            y0 = w["top"] if y0 is None else y0
        else:
            rows.append(cur); cur = [w]; y0 = w["top"]
    if cur:
        rows.append(cur)

    # column intervals: merge word x-spans within col_gap; holes are separators.
    occ = []
    for w in sorted(words, key=lambda w: w["x0"]):
        if occ and w["x0"] <= occ[-1][1] + col_gap:
            occ[-1][1] = max(occ[-1][1], w["x1"])
        else:
            occ.append([w["x0"], w["x1"]])

    def colof(w):
        cx = (w["x0"] + w["x1"]) / 2
        best, bd = 0, 1e9
        for i, (a, b) in enumerate(occ):
            if a - col_gap <= cx <= b + col_gap:
                return i
            d = min(abs(cx - a), abs(cx - b))
            if d < bd:
                bd, best = d, i
        return best

    def spanned(w):  # columns whose CENTRE falls within the word's x-extent
        cs = [i for i, (a, b) in enumerate(occ) if w["x0"] - 2 <= (a + b) / 2 <= w["x1"] + 2]
        return cs or [colof(w)]

    # data-row assignment (centre) first, to find where the header ends
    data_grid = []
    for r in rows:
        cells = [""] * len(occ)
        for w in sorted(r, key=lambda w: w["x0"]):
            i = colof(w)
            cells[i] = (cells[i] + " " + w["text"]).strip()
        data_grid.append(cells)
    hdr = 0
    for cells in data_grid:
        if sum(1 for c in cells if _NUMCELL.match(c.replace(" ", ""))) >= 2:
            break
        hdr += 1
    if hdr >= len(rows):  # no data row found — treat just the top line as header
        hdr = 1 if len(rows) > 1 else 0

    grid = []
    for ri, r in enumerate(rows):
        if ri < hdr:  # header row: spread spanning words across the columns they cover
            cells = [""] * len(occ)
            for w in sorted(r, key=lambda w: w["x0"]):
                for i in spanned(w):
                    cells[i] = (cells[i] + " " + w["text"]).strip()
            grid.append(cells)
        else:
            grid.append(data_grid[ri])

    grid = _merge_symbol_cols(grid, hdr)
    n = len(grid[0]) if grid else 0
    keep = [c for c in range(n) if any(row[c] for row in grid)]
    return [[row[c] for c in keep] for row in grid], hdr


def structure_text(text: str, col_gap: int = 2):
    """Best-effort structuring of a BARE text grid (no coordinates): column
    boundaries come from the NUMERIC tokens (which align), every token is assigned to
    a column by character mid-point, then symbol columns merge. Less reliable than
    structure_words on real coordinates — ASCII rendering doesn't preserve where the
    labels and headers sit — so it's the fallback when a text-grid carries no words."""
    L = [l for l in (text or "").split("\n") if l.strip()]
    if not L:
        return [], 0
    numspans = []
    for l in L:
        for m in re.finditer(r"\S+", l):
            if _NUMCELL.match(m.group()):
                numspans.append((m.start(), m.end()))
    numspans.sort()
    occ = []
    for a, b in numspans:
        if occ and a <= occ[-1][1] + col_gap:
            occ[-1][1] = max(occ[-1][1], b)
        else:
            occ.append([a, b])
    cols = ([[0, occ[0][0]]] + [list(o) for o in occ]) if occ else [[0, max(len(l) for l in L)]]

    def colof(mid):
        for i, (a, b) in enumerate(cols):
            if a - col_gap <= mid <= b + col_gap:
                return i
        return min(range(len(cols)), key=lambda i: min(abs(mid - cols[i][0]), abs(mid - cols[i][1])))

    grid = []
    for l in L:
        cells = [""] * len(cols)
        for m in re.finditer(r"\S+", l):
            i = colof((m.start() + m.end()) / 2)
            cells[i] = (cells[i] + " " + m.group()).strip()
        grid.append(cells)
    hdr = 0
    for row in grid:
        if sum(1 for c in row if _NUMCELL.match(c.replace(" ", ""))) >= 2:
            break
        hdr += 1
    if hdr >= len(grid):
        hdr = 1 if len(grid) > 1 else 0
    grid = _merge_symbol_cols(grid, hdr)
    n = len(grid[0]) if grid else 0
    keep = [c for c in range(n) if any(row[c] for row in grid)]
    return [[row[c] for c in keep] for row in grid], hdr


def to_html_headed(grid: list[list[str]], header_rows: int = 1) -> str:
    """HTML for a grid with multi-row headers: the first `header_rows` rows are <th>;
    horizontally-adjacent identical header cells (a spread span) collapse into one
    colspan'd cell, so the multi-index renders as the hierarchy it is."""
    import html as H
    if not grid:
        return "<table></table>"
    n = max(len(r) for r in grid)
    out = ["<table>"]
    for ri, row in enumerate(grid):
        cells = [c for c in row] + [""] * (n - len(row))
        if ri < header_rows:
            tds, i = [], 0
            while i < n:
                j = i + 1
                while j < n and cells[j] == cells[i] and cells[i] != "":
                    j += 1
                span = f" colspan={j-i}" if j - i > 1 else ""
                tds.append(f"<th{span}>{H.escape(cells[i])}</th>")
                i = j
            out.append("  <tr>" + "".join(tds) + "</tr>")
        else:
            out.append("  <tr>" + "".join(f"<td>{H.escape(c)}</td>" for c in cells) + "</tr>")
    out.append("</table>")
    return "\n".join(out)


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
