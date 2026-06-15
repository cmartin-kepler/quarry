#!/usr/bin/env python3
"""
recon_validate.py - Reconstruction-error validator for table parses (label-free).

Idea (see reconstruction-validator-brief.md): a correct HTML table, together with
the observed glyph positions in the source PDF region, explains where every glyph
landed. A transposed column / dropped cell / shifted header produces HTML that
CANNOT account for the observed token layout — it fails reconstruction even when
the text "looks clean". High reconstruction error => likely silent parse failure.

We compare in RELATIONAL space (which tokens share a row band / column band), not
absolute pixels — so renderer/font drift can't swamp the signal, and no headless
browser is needed.

Modes:
  selftest                 inject known corruptions into correct HTML; show the
                           error spikes + localizes; emit a TP/FP curve over tau.
  single  <pdf> --page N --bbox x0,y0,x1,y1 --html parse.html
  store   --store <artifacts_dir> --pdf <pdf>   batch over a quarry store -> CSV

Born-digital only: a region with no text layer returns NotApplicable (it must NOT
silently pass — that's the OCR tier's problem).
"""
from __future__ import annotations

import argparse
import csv
import json
import os
import sys
from dataclasses import dataclass, field
from difflib import SequenceMatcher
from html.parser import HTMLParser

import pdfplumber


# ---------------------------------------------------------------------------
# Result types (mirror the brief's interface)
# ---------------------------------------------------------------------------

@dataclass
class Diagnostics:
    coverage: float
    spurious_rate: float
    row_violations: float
    col_violations: float
    column_permutation: list[int] | None
    merge_split_sites: list[tuple] = field(default_factory=list)
    missing_tokens: list[str] = field(default_factory=list)
    spurious_tokens: list[str] = field(default_factory=list)


@dataclass
class ReconResult:
    status: str                 # "ok" | "not_applicable"
    error: float | None         # [0,1]; higher = worse
    diagnostics: Diagnostics | None

    def top_diagnostic(self) -> str:
        if self.status != "ok" or self.diagnostics is None:
            return self.status
        d = self.diagnostics
        cands = [
            (1 - d.coverage, f"missing {len(d.missing_tokens)} token(s)"),
            (d.spurious_rate, f"spurious {len(d.spurious_tokens)} token(s)"),
            (d.col_violations, "column-band violations"),
            (d.row_violations, "row-band violations"),
        ]
        if d.column_permutation and d.column_permutation != sorted(d.column_permutation):
            cands.append((1.0, f"column transposition {d.column_permutation}"))
        worst = max(cands, key=lambda c: c[0])
        return worst[1]


# ---------------------------------------------------------------------------
# Text normalization + tokenization
# ---------------------------------------------------------------------------

def norm(s: str) -> str:
    return "".join(ch for ch in s.lower() if ch.isalnum())


def tok_split(text: str) -> list[str]:
    return [t for t in text.split() if norm(t)]


# ---------------------------------------------------------------------------
# 1-2. Observe + band the source region
# ---------------------------------------------------------------------------

@dataclass
class Obs:
    text: str
    x0: float
    x1: float
    cy: float
    h: float
    row: int = -1
    col: int = -1


def observe(pdf_path: str, page: int, bbox: tuple[float, float, float, float]) -> list[Obs]:
    with pdfplumber.open(pdf_path) as pdf:
        pg = pdf.pages[page - 1]
        x0, y0, x1, y1 = bbox
        # Clamp to page so a slightly-oversized region doesn't error.
        crop = pg.crop((max(0, x0), max(0, y0), min(pg.width, x1), min(pg.height, y1)))
        words = crop.extract_words(use_text_flow=False, keep_blank_chars=False)
    out = []
    for w in words:
        out.append(Obs(
            text=w["text"], x0=float(w["x0"]), x1=float(w["x1"]),
            cy=(float(w["top"]) + float(w["bottom"])) / 2,
            h=float(w["bottom"]) - float(w["top"]),
        ))
    return out


def band_rows(tokens: list[Obs]) -> int:
    """Cluster on y-centers; assign .row. Returns band count."""
    if not tokens:
        return 0
    heights = sorted(t.h for t in tokens)
    tol = max(heights[len(heights) // 2] * 0.9, 2.0)
    order = sorted(range(len(tokens)), key=lambda i: tokens[i].cy)
    row = 0
    last = tokens[order[0]].cy
    for i in order:
        if tokens[i].cy - last > tol:
            row += 1
        tokens[i].row = row
        last = tokens[i].cy
    return row + 1


def band_cols(tokens: list[Obs]) -> int:
    """Column bands by merging overlapping x-intervals (alignment-agnostic: works
    for left/right/decimal-aligned columns since cells in a column overlap in x).
    Returns band count."""
    if not tokens:
        return 0
    widths = sorted(t.x1 - t.x0 for t in tokens)
    gap = max(widths[len(widths) // 2] * 0.5, 4.0)  # intra-cell spaces < gap < column gaps
    order = sorted(range(len(tokens)), key=lambda i: tokens[i].x0)
    col = 0
    cur_max = tokens[order[0]].x1
    for i in order:
        if tokens[i].x0 - cur_max > gap:
            col += 1
            cur_max = tokens[i].x1
        else:
            cur_max = max(cur_max, tokens[i].x1)
        tokens[i].col = col
    return col + 1


# ---------------------------------------------------------------------------
# 3. Lay out the hypothesis HTML as a logical grid
# ---------------------------------------------------------------------------

class _TableParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.rows: list[list[tuple[str, int, int]]] = []  # (text, rowspan, colspan)
        self.header_flags: list[bool] = []  # per <tr>: all cells were <th>
        self._row = None
        self._th = None
        self._cell = None
        self._span = (1, 1)

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if tag == "tr":
            self._row, self._th = [], []
        elif tag in ("td", "th") and self._row is not None:
            self._cell = []
            self._span = (int(a.get("rowspan", 1)), int(a.get("colspan", 1)))
            self._is_th = tag == "th"

    def handle_data(self, data):
        if self._cell is not None:
            self._cell.append(data)

    def handle_endtag(self, tag):
        if tag in ("td", "th") and self._cell is not None:
            text = "".join(self._cell).strip()
            self._row.append((text, self._span[0], self._span[1]))
            self._th.append(self._is_th)
            self._cell = None
        elif tag == "tr" and self._row is not None:
            self.rows.append(self._row)
            self.header_flags.append(bool(self._th) and all(self._th))
            self._row = None


def parse_html_grid(html: str) -> tuple[list[list[str]], set[int]]:
    """Parse an HTML table into a dense matrix (expanding rowspan/colspan) plus
    the set of header row indices (rows that were all <th>)."""
    p = _TableParser()
    p.feed(html)
    grid: list[list[str | None]] = []

    def ensure(r, c):
        while len(grid) <= r:
            grid.append([])
        while len(grid[r]) <= c:
            grid[r].append(None)

    header_rows = set()
    for r, row in enumerate(p.rows):
        if p.header_flags[r]:
            header_rows.add(r)
        c = 0
        for text, rs, cs in row:
            while True:
                ensure(r, c)
                if grid[r][c] is None:
                    break
                c += 1
            for dr in range(rs):
                for dc in range(cs):
                    ensure(r + dr, c + dc)
                    # Anchor text in the top-left; spanned cells get "" (filled).
                    grid[r + dr][c + dc] = text if (dr == 0 and dc == 0) else ""
            c += cs
    width = max((len(r) for r in grid), default=0)
    dense = [[(cell if cell is not None else "") for cell in r] + [""] * (width - len(r)) for r in grid]
    return dense, header_rows


# ---------------------------------------------------------------------------
# 4. Align hypothesis tokens to observed tokens (reading-order, diff-style)
# ---------------------------------------------------------------------------

@dataclass
class HypTok:
    text: str
    row: int
    col: int
    is_header: bool = False


def hyp_tokens(grid: list[list[str]], header_rows: set[int]) -> list[HypTok]:
    out = []
    for r, row in enumerate(grid):
        for c, cell in enumerate(row):
            for t in tok_split(cell):
                out.append(HypTok(t, r, c, r in header_rows))
    return out


def obs_reading_order(tokens: list[Obs]) -> list[int]:
    return sorted(range(len(tokens)), key=lambda i: (tokens[i].row, tokens[i].x0))


def align(obs: list[Obs], hyp: list[HypTok]):
    """Match by normalized TEXT (so a token still matches even if the parser put
    it in the wrong cell — which is exactly what we want to detect as a structural
    violation). Duplicates of the same text are paired by reading order.
    Returns (matched [(obs_i, hyp_j)], missing obs idx, spurious hyp idx)."""
    from collections import defaultdict

    obs_by: dict[str, list[int]] = defaultdict(list)
    for i in obs_reading_order(obs):
        if norm(obs[i].text):
            obs_by[norm(obs[i].text)].append(i)

    hyp_order = sorted(
        (j for j in range(len(hyp)) if norm(hyp[j].text)),
        key=lambda j: (hyp[j].row, hyp[j].col),
    )
    hyp_by: dict[str, list[int]] = defaultdict(list)
    for j in hyp_order:
        hyp_by[norm(hyp[j].text)].append(j)

    matched, missing, spurious = [], [], []
    for key in set(obs_by) | set(hyp_by):
        o, h = obs_by.get(key, []), hyp_by.get(key, [])
        n = min(len(o), len(h))
        matched.extend(zip(o[:n], h[:n]))
        missing.extend(o[n:])
        spurious.extend(h[n:])
    return matched, missing, spurious


# ---------------------------------------------------------------------------
# 5-6. Score structural consistency + aggregate
# ---------------------------------------------------------------------------

def _dominant(counts: dict) -> int | None:
    return max(counts, key=counts.get) if counts else None


def score(obs, hyp, matched, missing, spurious, n_obs_cols) -> tuple[float, Diagnostics]:
    n_obs = sum(1 for t in obs if norm(t.text))
    n_hyp = sum(1 for t in hyp if norm(t.text))
    coverage = (len(matched) / n_obs) if n_obs else 1.0
    spurious_rate = (len(spurious) / n_hyp) if n_hyp else 0.0

    # hyp_row -> set of obs_row bands; hyp_col -> obs_col bands (over all tokens:
    # a shifted header is itself an error we want to catch).
    rows_of: dict[int, set] = {}
    cols_of: dict[int, dict] = {}
    for oi, hj in matched:
        rows_of.setdefault(hyp[hj].row, set()).add(obs[oi].row)
        cols_of.setdefault(hyp[hj].col, {}).setdefault(obs[oi].col, 0)
        cols_of[hyp[hj].col][obs[oi].col] += 1

    row_viol = sum(1 for s in rows_of.values() if len(s) > 1)
    row_violations = row_viol / len(rows_of) if rows_of else 0.0
    col_viol = sum(1 for d in cols_of.values() if len(d) > 1)
    col_violations = col_viol / len(cols_of) if cols_of else 0.0

    # Reverse: one OBSERVED column claimed by several hyp columns => a spurious
    # split (e.g. a header in one hyp col, its values in another — they share an
    # observed column but the parse dissociates them). The forward check above
    # misses this; it's exactly the phantom-column-split silent error.
    inv: dict[int, set] = {}
    for oi, hj in matched:
        inv.setdefault(obs[oi].col, set()).add(hyp[hj].col)
    col_split = sum(1 for s in inv.values() if len(s) > 1)
    col_split_violations = col_split / len(inv) if inv else 0.0

    # Permutation: hyp_col -> dominant obs_col, in hyp-col order. A faithful parse
    # maps hyp columns to observed columns in ascending order; any deviation is a
    # transposition (a pure column swap is a clean permutation, NOT a band
    # violation, so it only shows up here).
    perm = None
    perm_penalty = 0.0
    if cols_of:
        ordered = sorted(cols_of)
        perm = [_dominant(cols_of[hc]) for hc in ordered]
        defined = [v for v in perm if v is not None]
        if defined:
            expected = sorted(defined)
            displaced = sum(1 for v, e in zip(defined, expected) if v != e)
            perm_penalty = displaced / len(defined)

    # Merge sites (forward: one hyp col over many obs cols) and split sites
    # (reverse: one obs col under many hyp cols).
    sites = []
    for hc, d in cols_of.items():
        if len(d) > 1:
            sites.append((f"hyp_col {hc}", "merge"))
    for oc, s in inv.items():
        if len(s) > 1:
            sites.append((f"obs_col {oc}", "split"))

    error = (
        0.35 * (1 - coverage)
        + 0.10 * spurious_rate
        + 0.15 * row_violations
        + 0.15 * col_violations
        + 0.10 * col_split_violations
        + 0.15 * perm_penalty
    )
    error = max(0.0, min(1.0, error))
    diag = Diagnostics(
        coverage=coverage, spurious_rate=spurious_rate,
        row_violations=row_violations, col_violations=col_violations + col_split_violations,
        column_permutation=perm, merge_split_sites=sites,
        missing_tokens=[obs[i].text for i in missing][:20],
        spurious_tokens=[hyp[j].text for j in spurious][:20],
    )
    return error, diag


def validate_table(pdf_path, page, bbox, html) -> ReconResult:
    obs = observe(pdf_path, page, bbox)
    if not obs:
        return ReconResult("not_applicable", None, None)
    n_cols = band_cols(obs)
    band_rows(obs)
    grid, header_rows = parse_html_grid(html)
    hyp = hyp_tokens(grid, header_rows)
    matched, missing, spurious = align(obs, hyp)
    error, diag = score(obs, hyp, matched, missing, spurious, n_cols)
    return ReconResult("ok", error, diag)


# ---------------------------------------------------------------------------
# HTML rendering (for the self-test corruptions)
# ---------------------------------------------------------------------------

def render_html(grid: list[list[str]], header_rows: int = 1) -> str:
    s = ["<table>"]
    for r, row in enumerate(grid):
        tag = "th" if r < header_rows else "td"
        cells = "".join(f"<{tag}>{c}</{tag}>" for c in row)
        s.append(f"  <tr>{cells}</tr>")
    s.append("</table>")
    return "\n".join(s)


# ---------------------------------------------------------------------------
# Self-test: inject known corruptions, confirm error spikes + localizes
# ---------------------------------------------------------------------------

import copy


def swap_columns(grid, c1, c2):
    g = copy.deepcopy(grid)
    for row in g:
        if c1 < len(row) and c2 < len(row):
            row[c1], row[c2] = row[c2], row[c1]
    return g


def drop_cell(grid, r, c):
    g = copy.deepcopy(grid)
    g[r][c] = ""
    return g


def merge_adjacent(grid, r, c):
    g = copy.deepcopy(grid)
    g[r][c] = (g[r][c] + " " + g[r][c + 1]).strip()
    g[r][c + 1] = ""
    return g


def shift_header(grid):
    g = copy.deepcopy(grid)
    g[0] = [""] + g[0][:-1]  # slide header right by one => misaligns labels
    return g


def transpose_block(grid, r, c):
    g = copy.deepcopy(grid)
    g[r][c], g[r + 1][c + 1] = g[r + 1][c + 1], g[r][c]
    g[r][c + 1], g[r + 1][c] = g[r + 1][c], g[r][c + 1]
    return g


# A few clean base tables (right-aligned numeric columns, like real filings).
_BASES = [
    [
        ["Line item", "FY2024", "FY2023"],
        ["Product revenue", "1,200", "1,000"],
        ["Service revenue", "800", "650"],
        ["Total revenue", "2,000", "1,650"],
    ],
    [
        ["Item", "2024", "2023", "Change"],
        ["Cash and equivalents", "5,400", "4,100", "32"],
        ["Accounts receivable", "1,250", "1,100", "14"],
        ["Inventory", "900", "850", "6"],
    ],
    [
        ["Segment", "Revenue", "Op Income", "Margin"],
        ["Media", "3,310", "1,120", "34"],
        ["Parks", "8,430", "2,210", "26"],
        ["Studios", "2,540", "640", "25"],
    ],
]


def _selftest_pdf(grid, path):
    from reportlab.lib import colors
    from reportlab.lib.pagesizes import letter
    from reportlab.lib.units import inch
    from reportlab.platypus import SimpleDocTemplate, Table, TableStyle

    doc = SimpleDocTemplate(path, pagesize=letter, topMargin=inch, bottomMargin=inch)
    n_cols = len(grid[0])
    t = Table(grid, colWidths=[2.6 * inch] + [1.1 * inch] * (n_cols - 1))
    t.setStyle(TableStyle([
        ("GRID", (0, 0), (-1, -1), 0.5, colors.black),
        ("FONTNAME", (0, 0), (-1, 0), "Helvetica-Bold"),
        ("FONTSIZE", (0, 0), (-1, -1), 10),
        ("ALIGN", (1, 0), (-1, -1), "RIGHT"),
    ]))
    doc.build([t])


def run_selftest(workdir: str, taus: list[float]):
    os.makedirs(workdir, exist_ok=True)
    clean_errors, corrupt_rows = [], []

    for bi, base in enumerate(_BASES):
        pdf = os.path.join(workdir, f"base{bi}.pdf")
        _selftest_pdf(base, pdf)
        with pdfplumber.open(pdf) as p:
            pg = p.pages[0]
            bbox = (0, 0, pg.width, pg.height)
            page = 1

        clean_html = render_html(base)
        clean = validate_table(pdf, page, bbox, clean_html)
        clean_errors.append(clean.error)

        corruptions = {
            "swap_cols(1,2)": swap_columns(base, 1, 2),
            "drop_cell(1,1)": drop_cell(base, 1, 1),
            "merge(1,0..1)": merge_adjacent(base, 1, 0),
            "shift_header": shift_header(base),
            "transpose(1,1)": transpose_block(base, 1, 1),
        }
        for name, cg in corruptions.items():
            res = validate_table(pdf, page, bbox, render_html(cg))
            corrupt_rows.append((bi, name, res))

    # Report.
    print("=== clean tables (want LOW error) ===")
    for bi, e in enumerate(clean_errors):
        print(f"  base{bi}: error {e:.3f}")
    print("\n=== corruptions (want HIGH error + localized diagnostic) ===")
    for bi, name, res in corrupt_rows:
        print(f"  base{bi} {name:<16} error {res.error:.3f}   {res.top_diagnostic()}")

    # TP/FP sweep.
    print("\n=== TP/FP over tau ===")
    print(f"  {'tau':>5} {'TP(corrupt caught)':>20} {'FP(clean flagged)':>19}")
    n_pos = len(corrupt_rows)
    n_neg = len(clean_errors)
    for tau in taus:
        tp = sum(1 for _, _, r in corrupt_rows if (r.error or 0) > tau) / n_pos
        fp = sum(1 for e in clean_errors if (e or 0) > tau) / n_neg
        print(f"  {tau:>5.2f} {tp:>19.0%} {fp:>18.0%}")

    # Localization checks.
    print("\n=== localization (does the diagnostic point at the right error?) ===")
    ok = 0
    total = 0
    for bi, name, res in corrupt_rows:
        d = res.diagnostics
        total += 1
        hit = False
        if name.startswith("swap_cols"):
            hit = d.column_permutation is not None and d.column_permutation != sorted(
                x for x in d.column_permutation if x is not None
            )
        elif name.startswith("drop_cell"):
            hit = len(d.missing_tokens) > 0
        elif name.startswith("merge"):
            hit = d.col_violations > 0 or len(d.spurious_tokens) >= 0 and (res.error or 0) > 0.05
        elif name == "shift_header":
            hit = (res.error or 0) > 0.05
        elif name.startswith("transpose"):
            hit = d.row_violations > 0 or d.col_violations > 0
        ok += 1 if hit else 0
        print(f"  base{bi} {name:<16} {'localized' if hit else 'NOT localized'}")
    print(f"\nlocalized {ok}/{total} corruptions")


# ---------------------------------------------------------------------------
# Batch over a quarry artifact store
# ---------------------------------------------------------------------------

def _anchor_of(meta) -> tuple[int, tuple] | None:
    prov = meta.get("provenance", {})
    src = prov.get("Source") or (prov.get("Derived", {}) or {}).get("anchor")
    if not src or src.get("format") != "pdf":
        return None
    b = src["bbox"]
    return src["page"], (b["x0"], b["y0"], b["x1"], b["y1"])


def run_store(store_dir: str, pdf_path: str, out_csv: str | None):
    manifest = json.load(open(os.path.join(store_dir, "manifest.json")))
    rows = []
    for art in manifest["artifacts"]:
        if art.get("kind") != "HtmlTable":
            continue
        meta = art["meta"]
        anchor = _anchor_of(meta)
        if not anchor:
            continue
        page, bbox = anchor
        res = validate_table(pdf_path, page, bbox, art["html"])
        rows.append({
            "element": meta["id"], "page": page,
            "bbox": ",".join(f"{v:.0f}" for v in bbox),
            "status": res.status,
            "error": f"{res.error:.3f}" if res.error is not None else "",
            "top_diagnostic": res.top_diagnostic(),
        })
    rows.sort(key=lambda r: -(float(r["error"]) if r["error"] else -1))

    w = csv.DictWriter(sys.stdout if not out_csv else open(out_csv, "w", newline=""),
                       fieldnames=["element", "page", "bbox", "status", "error", "top_diagnostic"])
    w.writeheader()
    for r in rows:
        w.writerow(r)
    if out_csv:
        print(f"wrote {out_csv} ({len(rows)} table(s))", file=sys.stderr)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="Reconstruction-error validator for table parses.")
    sub = ap.add_subparsers(dest="mode", required=True)

    st = sub.add_parser("selftest", help="inject corruptions; emit TP/FP curve")
    st.add_argument("--workdir", default="corpus/recon_selftest")

    si = sub.add_parser("single", help="validate one region against one HTML")
    si.add_argument("pdf")
    si.add_argument("--page", type=int, required=True)
    si.add_argument("--bbox", required=True, help="x0,y0,x1,y1 (PDF points, top-left)")
    si.add_argument("--html", required=True, help="path to the parser's HTML")

    sr = sub.add_parser("store", help="batch over a quarry artifact store -> CSV")
    sr.add_argument("--store", required=True)
    sr.add_argument("--pdf", required=True)
    sr.add_argument("--out", help="CSV path (default: stdout)")

    args = ap.parse_args()
    if args.mode == "selftest":
        run_selftest(args.workdir, [0.02, 0.05, 0.10, 0.15, 0.20, 0.30])
    elif args.mode == "single":
        bbox = tuple(float(v) for v in args.bbox.split(","))
        html = open(args.html).read()
        res = validate_table(args.pdf, args.page, bbox, html)
        print(json.dumps({
            "status": res.status,
            "error": res.error,
            "top_diagnostic": res.top_diagnostic(),
            "diagnostics": res.diagnostics.__dict__ if res.diagnostics else None,
        }, indent=2))
    elif args.mode == "store":
        run_store(args.store, args.pdf, args.out)


if __name__ == "__main__":
    main()
