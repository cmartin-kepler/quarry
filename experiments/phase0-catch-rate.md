# Phase-0 experiment: silent-failure catch rate (first real data)

The brief's riskiest assumption (§2): **silent parse failures are detectable by
something other than the agent.** Metric: *of the extractions that are actually
wrong, what fraction did ≥1 detector flag?* — on real, hard, hand-labeled tables.

This is the first run on **hand-labeled real documents** (not synthetic, which the
brief warns "lie to you optimistically"). Ground truth was produced by reading the
rendered PDF crops and transcribing the true cells **independent of any parser**
(no parser output was used as truth — that would be circular).

## Batch 1 — Disney Q2 FY26 reconciliations (born-digital, 6 tables)

Chosen because it's exactly the brief's "hard": dense, footnoted, multi-level
headers, multi-section, wrapped cells, %-change columns, dash placeholders.
Parser under test: the cheap geometric reconstructor (the lazy first pass).

"Wrong" = the extraction would make an agent retrieve an incorrect value for a
(row, column-header) query — i.e. a value misaligned / transposed / dropped /
garbled — verified by a column-consistency check (do numeric values land in
consistent columns across rows?).

| table | what it is | cheap result | actually wrong? | why | flagged? | by |
|---|---|---|---|---|---|---|
| t0 | EPS bridge (Q) | 15×13 | **WRONG** | indented sub-rows shifted 1 col (10/12 rows off-column) | yes | structural (SUSPECT) |
| t1 | EPS bridge (6mo) | 15×12 | **WRONG** | same indent shift (8/12 rows off) | yes | structural |
| t2 | segment op income | 11×13 | **WRONG** | same (3 rows off) | yes | structural |
| t3 | cash flow | 3×9 | correct | values in consistent columns | no | — (true negative) |
| t4 | free cash flow | 4×13 | correct | aligned; wrapped-label empty row only | **yes** | structural (**false alarm**) |
| t5 | SVOD op income | 3×6 | correct | aligned | no | — (true negative) |

### Result (batch 1)

- **Wrong extractions: 3 / 6.**
- **Catch rate: 3/3 = 100%** — every wrong table was flagged.
- **Missed (wrong but unflagged): 0** — no silent failures slipped through.
- **False-alarm rate: 1/3 = 33%** (t4: a correct table flagged on a wrapped-label empty row).
- **Per detector:** StructuralValidity caught all 3. IntrinsicArithmetic and the
  reconstruction-error validator caught **0** (recon error ≈ 0.05, below threshold —
  the values *do* reconstruct, so recon sees low error).

## What this does and does not tell us

**Encouraging:** on the hardest real table type, every wrong extraction was
caught, nothing slipped silently, and we learn **structural validity carries the
weight** for this failure mode — not arithmetic, not reconstruction error.

**The important caveat — the scary case did NOT occur.** These tables failed
*loudly* (messy structure, column misalignment, empty cells), which is exactly
why structural flagged them. The brief's real fear is the **clean-looking** wrong
table — a transposed column or shifted row in an otherwise tidy grid, where the
text looks fine and no structural signal fires. The cheap parser didn't produce
that here; it mangled these tables visibly. So **the riskiest sub-case (detecting
a clean wrong table) is still under-tested.**

**Also:** N=6, one document, one failure mode (indent-shift). And a 33%
false-alarm rate would be costly at scale.

## Verdict so far

Per the brief's decision rule, this is evidence toward "high catch rate → lazy/
cheap-first justified," **but it is not yet conclusive**: we have not produced a
clean-but-wrong extraction and confirmed whether anything catches it. That is the
case the whole assumption hinges on.

## Batch 2 — JPMorgan 2023 annual report: audit the UNFLAGGED tables

Silent failures, by definition, hide among the tables the detectors *passed*. So
this is the brief's §7 audit: take cheap tables that came back `no_issues` and
check whether any are actually wrong (wrong + unflagged = a silent failure).

Of 52 cheap tables in jpm-2023-ar: **36 suspect, 13 no_issues.** Of the 13
unflagged, only **3** are non-trivial data tables. Audited all 3 against the
source:

| | what it actually is | cheap result | wrong? | flagged? |
|---|---|---|---|---|
| u0 | real multi-section "Stock total return" table | values correct, columns aligned | **no** | no → correct true-negative |
| u1 | a **bar chart** (Overhead ratio / ROTCE by bank) | bars interleaved into a scrambled grid | **YES** | **no → SILENT FAILURE** |
| u2 | an **infographic** (segment % vs competitors, colored boxes) | grid of %s + competitor labels | **YES** | **no → SILENT FAILURE** |

### The finding that matters

The silent failures we found are **not value-transpositions in real tables** —
they're **charts and infographics misdetected as tables**, reconstructed into
plausible-looking grids that no detector flagged. An agent would treat u1/u2 as
real tabular data and misread them.

Critically, the figure-guard heuristic ("no numeric columns ⇒ figure") does NOT
catch these: u1/u2 are full of percentages. Telling a bar chart from a table
needs a **layout/vector signal** (colored boxes, non-grid arrangement, drawing
density from the PDF), not just text typing.

## Combined verdict (batches 1–2, 9 tables audited)

- Wrong **real-table** extractions: 3 (q2 indent-shift) — **all caught** by
  structural validity.
- Wrong **non-table** extractions (charts as tables): 2 — **both missed**.
- Overall: of 5 wrong extractions, 3 flagged → **catch rate ≈ 60%**, with **both
  misses a single class: figures-as-tables.**
- No clean value-transposition in a *real* table has surfaced yet — with this
  parser, real tables fail loudly (structural) rather than silently.

**So, is the riskiest assumption confirmed?** Partly, and more precisely than
"yes/no":
1. For **real tables**, mechanical detection holds so far — wrong ones fail
   loudly and get flagged (structural validity carries the weight; arithmetic and
   reconstruction-error did not fire on these).
2. The actual silent-failure class is **non-tables misdetected as tables**, and
   the **single highest-value missing detector is a figure-vs-table guard** with a
   vector/layout signal — without it, charts reach the agent as clean data.
3. Still under-tested: a clean transposition inside a real table (rare with this
   parser), restated financials, and the scanned/OCR path.

## Batch 3 — built the figure-vs-table guard, re-audited

Built the detector the audit pointed at. The signal is **dark/saturated filled-
rectangle coverage** of the region (`figure_score`): a chart's bars and an
infographic's colored boxes are dark fills; a real table has at most a small dark
header (~7%), and pale row-shading doesn't count. (Key gotcha: these PDFs use
**CMYK** colors — the lightness conversion has to handle 4-tuples or every fill
reads as white.) Computed in the bridge (`scripts/pdf_to_qdoc.py`), carried on
`RiskMarkers.figure_score`, flagged in `evidence.rs` at >15%.

Re-audit:
- The two silent failures (jpm bar chart 7×8 = 0.21, infographic 10×7 = 0.17) are
  **now flagged** → SUSPECT. The class is caught.
- **No false positives on real tables**: q2's six tables (which have pale-blue
  row-shading) score 0; the synthetic born-digital tables score 0. Across the
  whole jpm report, only ~2 figure-flagged regions even *look* like real data
  tables — and one of those is the actual bar chart (a correct catch).

### Updated combined verdict

| failure class | wrong | caught | by |
|---|---|---|---|
| real tables, column-misaligned (q2) | 3 | 3 | structural validity |
| charts/infographics as tables (jpm) | 2 | **2** | **figure guard** |
| **total** | **5** | **5 → catch rate 100%** | |

With the figure guard, **every wrong extraction we have surfaced is now caught,
and we know which detector catches which class** (structural → table mis-parses;
figure guard → non-tables; arithmetic → reconciliation when total rows exist;
reconstruction-error → injected transpositions). That is exactly the Phase-0
output the brief asked for: a catch-rate number plus *which detector carries the
weight for which failure*.

## What is still genuinely untested

1. A **clean transposition inside a real table** — has not surfaced; this parser
   fails loudly. Needs clean digital income statements / balance sheets.
2. **Restated financials** and a **scanned page** (must return `NotApplicable`,
   never silently pass).
3. **False-alarm rate** — ~33% on real q2 tables (structural's high recall); the
   figure guard adds little FP, but the overall precision cost needs tracking.
4. N is still small (11 tables, 2 docs). Grow to 20–30 for a stable number.
