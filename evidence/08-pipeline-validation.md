# 08 — Pipeline validation vs docling-full and lit

**Question.** Does the quarry pipeline (triage-gated docling) lose any real tables
or text versus running docling on every page, or lit? Run on all 20 corpus docs.

**Harness.** `scripts/validate.py` — per doc, all no-OCR: `quarry pipeline`
(triage-gated docling on text pages) vs docling whole-doc (every page,
`do_table_structure=True`) vs `lit` (whole doc). Counts only (timing here is *not*
comparable — quarry reloads models per `uv` call; see `03` for clean timing).

## Results

| document | pg | quarry t/txt/ocr | docling t/txt | lit tok | Δtbl |
|---|--:|--:|--:|--:|--:|
| Pershing-Uber | 5 | 0 / 32 / 0 | 0 / 101 | 163 | 0 |
| RBRTEd | 5 | 0 / 10 / 0 | 0 / 10 | 511 | 0 |
| q2-fy26-recs | 5 | 6 / 37 / 0 | 6 / 37 | 477 | 0 |
| 2605.15184 | 9 | 4 / 100 / 0 | 4 / 140 | 1174 | 0 |
| brk-2022-letter | 10 | 2 / 87 / 0 | 2 / 106 | 681 | 0 |
| Direct Short | 12 | 4 / 22 / 1 | 4 / 144 | 671 | 0 |
| 1706.03762 | 15 | 4 / 118 / 0 | 4 / 460 | 1812 | 0 |
| brk-2023-letter | 16 | 2 / 152 / 0 | 2 / 152 | 1098 | 0 |
| 2025ltr | 20 | 4 / 173 / 0 | 4 / 182 | 1095 | 0 |
| 2402.01030 | 25 | 12 / 190 / 0 | 12 / 340 | 6575 | 0 |
| **Q4-FY24-Earnings** | 25 | **0 / 0 / 25** | **13 / 0** | 5205 | **13** |
| 2602.05014 | 26 | 7 / 400 / 0 | 7 / 807 | 4140 | 0 |
| gpmr2026 | 33 | 1 / 317 / 0 | 1 / 900 | 2344 | 0 |
| **Q3-FY24-Earnings** | 34 | **0 / 0 / 34** | **11 / 0** | 7118 | **11** |
| 2604.08538 | 36 | 18 / 278 / 0 | 18 / 1223 | 6230 | 0 |
| 2605.05242 | 51 | 8 / 289 / 0 | 8 / 527 | 5745 | 0 |
| 2026-Annual-IR | 85 | 11 / 448 / 0 | 11 / 1590 | 3658 | 0 |
| Abacus CDO | 133 | 14 / 266 / 0 | 14 / 266 | 75640 | 0 |
| brk-2023-ar | 152 | 128 / 1653 / 0 | 128 / 1735 | 13390 | 0 |
| jpm-2023-ar | 364 | 291 / 4708 / 0 | 291 / 7240 | 51964 | 0 |

## Conclusions

**Tables — the gate is safe.** On all **18 text-containing docs, quarry tables ==
docling-full tables (Δ=0)**, including the large ones (brk-ar 128, jpm 291, CDO 14).
The triage gate loses **no real tables**.

The only Δ>0 are the two **all-image presentation decks** (Q4/Q3 earnings — 100%
rasterized slides, 0 text pages). docling-full reported 13 and 11 tables there, but
with **0 texts** — i.e. **empty/spurious tables** the table-structure model
hallucinates on image content (evidence `04`). The gate correctly skips them and
records 25 + 34 OCR-deferred markers instead. So it loses nothing real and avoids 24
noise tables.

**Text/sections — one real gap.** quarry's structured-text count is a subset of
docling's raw `texts`:
- *correct:* docling's raw count includes page furniture (headers/footers) outside
  `body.children`, which quarry rightly excludes;
- *bug:* `structured_doc_from_docling` walks only **top-level** `body.children` and
  skips `#/groups/...` refs, so **text nested in groups (lists, multi-column
  sections) is missed.** Hence the gap is ~0 on flat docs (RBRTEd 10=10, brk-letter
  152=152, CDO 266=266) and large on nested ones (1706: 118 vs 460; 2026-pres: 448 vs
  1590).

**Action:** recurse `body.children` through `groups` to capture all structured text.
The table above lists the exact docs to re-check after the fix.

**lit** captures the raw text layer densely (tokens), a low-level fidelity reference;
not directly comparable to docling's structured `texts`.
