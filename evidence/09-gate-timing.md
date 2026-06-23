# 09 — Does the triage gate actually save time?

**Question.** Not losing tables (08) is required but not sufficient — the gate must
be *more efficient* than just running docling on everything, net of its own cost.

**Harness.** `scripts/timing_gate.py` — warm, per page across all 1061 corpus pages:
time docling-whole on each single page, classify it, then `full = Σ all pages`,
`gated = Σ text pages only`, `saved = full − gated`, `triage = Σ classify()`,
`net = saved − triage`.

## Result — and the triage-cost trap

| | docling-all | triage-gated docling | triage cost | net saved |
|---|--:|--:|--:|--:|
| corpus (1061 pg) | 527s | 506s | — | — |
| docling saved | | | | **21s (4%)** |
| with **pdfplumber** triage | | | **35s** | **−14s (LOSS)** |
| with **pypdfium2** triage | | | **2.7s** | **+18s (WIN)** |

The docling saving (21s) comes **entirely from two all-image decks** (Q4/Q3 earnings,
each 98–99% saved — docling skipped on 25/34 rasterized slides). Every text doc saves
~0 (no pages to skip).

**The trap:** the triage runs on *every* page, so its per-page cost is decisive. The
first cut used pdfplumber to read the text layer:
- `pdfplumber.extract_words()` — 65 ms/page
- `pdfplumber len(page.chars)` — 31 ms/page (char *parsing* is the cost, not the
  word-clustering — the "0ms" micro-bench was caching)

→ 35s of triage > 21s of docling saved → **the gate was a net loss (−14s).**

**The fix:** pypdfium2's native `count_chars` — **2.5 ms/page** (≈16×), with a native
thumbnail render for the blank/content check. Triage drops to 2.7s → **net +18s.**
Classification is unchanged (image=67, blank=2; Q4 25 + Q3 34 image slides + 2 blanks).

## Honest verdict

- **Image-dominant docs (decks, scans): big win** — 98–99% saved; the gate's whole
  value lives here.
- **Text-heavy docs (arxiv, filings — most of this corpus *and* most docling cost):
  the gate saves ~nothing**, and is only break-even if triage is pypdfium2-cheap.
- So the gate is **net-positive overall (+18s / ~4% here) but entirely from the image
  decks.** On a pure text+table workload it is ~neutral (≈2.7s triage cost, ~0 saved)
  — worth keeping as cheap insurance only if the corpus contains image/scanned pages.

(Aside: there's no saving to recover on text-page *figures* — docling's table model
adds ~0 on embedded figures, only firing on real tables / full-page images; see `04`.)
