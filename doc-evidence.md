# Evidence & decisions log

The measurements that drove `doc-architecture.md` (the converged pipeline) and
`doc-build-order.md`. Each section: the question, how we measured it, the numbers,
and the decision. All docling runs are `do_ocr=False` unless noted; all timings are
warm (models loaded once) unless noted "wall-clock". Harnesses live in `scripts/`
(`probe.py`, `corpus_yolo.py`/`corpus_tables.py`, `docling_stages.py`, `triage.py`,
`speed_yolo.py`/`speed_tables.py`).

## Summary: evidence → decision

| # | Finding | Decision |
|---|---|---|
| 1 | Cheap-parse "wrong answers" on real tables trace to **region clipping**, and the multiset probe is blind to column/row mangling | Region detection is a real (subtle) failure; the *parse-quality* concern (column/row) is fixed by **repair + materialize**, not by a perfect parser |
| 2 | yolo26n ≈ 80ms/page vs doclayout ≈ 410ms (5×); then: nothing in the core path needs layout | **Remove YOLO entirely** |
| 3 | Over 1061 pages, gating docling by table-presence (804s) costs **more** than docling-every-page (536s) | Don't gate docling by table-presence |
| 4 | docling's cost **is** the table-structure model; it wastes ~950ms on full-page images | Gate **image/no-text pages**, not no-table pages |
| 5 | Full-page rasterized (0-word) slides are the genuinely expensive pages | Triage must catch image/no-text pages |
| 6 | A ~40dpi thumbnail's **stddev** cleanly separates blank (0.0) from content (≥33), ~10ms | Cheap blank-vs-content gate; content → OCR-deferred, not dropped |
| 7 | docling whole-page is clip- and diagram-immune and competitive on cost | **docling whole-page** (not cropped, not escalation) is the default parser |
| 8 | Pipeline vs docling-full (20 docs): gate loses **no real tables** (Δ=0 on all 18 text docs); only drops spurious empty tables on all-image decks; structured text misses nested groups | **Gate validated**; fix: recurse `body.children` groups for complete sections |

**Detailed write-ups** (full method, data tables, analysis, caveats) in `evidence/`:
1. [`evidence/01-step0-probe.md`](evidence/01-step0-probe.md) — cheap-parse wrong answers / region clipping
2. [`evidence/02-layout-speed.md`](evidence/02-layout-speed.md) — layout model speed; removing YOLO
3. [`evidence/03-corpus-cost.md`](evidence/03-corpus-cost.md) — 1061-page 4-pipeline cost
4. [`evidence/04-docling-stages.md`](evidence/04-docling-stages.md) — where docling spends time
5. [`evidence/05-image-pages.md`](evidence/05-image-pages.md) — which image pages are expensive
6. [`evidence/06-blank-discriminator.md`](evidence/06-blank-discriminator.md) — blank vs content
7. [`evidence/07-whole-vs-crop-docling.md`](evidence/07-whole-vs-crop-docling.md) — whole-page vs crop
8. [`evidence/08-pipeline-validation.md`](evidence/08-pipeline-validation.md) — pipeline vs docling-full vs lit (corpus)

The sections below are the short version; follow the links for the full evidence.

---

## 1. Step-0 probe — do cheap parses give *wrong answers*?

**Q:** On born-digital tables, does the cheap path produce wrong numeric answers, or
is its messiness cosmetic?

**Method:** `scripts/probe.py` on `brk-2023-ar.pdf` (8 table pages). Cheap path =
YOLO table region → litparse over the text layer; numeric content (litparse word
tokens) compared to docling's table, matched by bbox IoU.

**Result:** 7 tables matched; **1/7 answer-faithful** (numeric multisets identical).
Adjudicating the divergences: litparse tokens were *clipped* — `4,807` → `4,8`,
header + first data row missing. On page 59 the YOLO box was `[44, 116.8, 549.4,
245.6]`, but the rightmost column (`4,807`) runs to `x1=562` and the first data row
sits at `top=108.5` — i.e. the box is ~10–15pt too tight, and **the page text layer
has the full values** (the whole table is present when read un-cropped).

**Conclusions / decisions:**
- The divergences are **region clipping**, not value errors. Region scope is a real
  failure — but **subtle and systematic** (same on yolo26n and doclayout), so it
  reads as "regions are basically fine."
- The probe's numeric-multiset test is **blind to column/row mangling** (right
  numbers in wrong cells still "match"), and it bypassed the crate's grid coalescer.
  So it can't be used to claim "region, not column/row." The column/row mangling is
  the real parse-quality issue → addressed by **Stage 2 repair + Stage 3
  materialize**, not by demanding a perfect parse.

## 2. Layout model & speed → remove YOLO

**Q:** Is YOLO layout cheap, and is it even needed?

**Method:** per-page inference timing, model preloaded (verified: warm inference
stable, render isolated ~6ms).

**Result:**
- doclayout (imgsz 1024, CPU) ≈ **402ms/page**; **yolo26n ≈ 80ms/page** (~5×).
- imgsz sweep on doclayout (same image): 512=133ms, 640=188ms, **768=236ms (same
  detections as 1024)**, 1024=402ms. MPS gave no speedup.
- Tracing the converged design: the page-gate is the cheap triage (no model), and
  table detection + clean bboxes come from **docling's own internal layout**.

**Decision:** **Remove YOLO entirely.** Drops the torch/ultralytics env, the layout
sidecar, and coordinate map #1 (pixel→point). The region-quality checks / XY-cut /
column-alignment we built are parked as an *optional* model-free "did docling miss a
table" cross-check.

## 3. Corpus cost — does gating docling save?

**Q:** Does skipping docling on no-table pages save time?

**Method:** `corpus_yolo.py` + `corpus_tables.py` over **all 20 input/ PDFs = 1061
pages**, every page, warm. Four pipelines (whole-document seconds):

| | o1 YOLO+litparse | o2 YOLO+docling-crop | o3 YOLO-gated docling-whole | o4 docling-whole every page |
|---|---|---|---|---|
| corpus total | 351s | 495s | **804s** | **536s** |

**Result:** `o4/o3 = 0.7x` — **gating docling by table-presence is the *most
expensive* option**, pricier than just running docling on every page. The gate's
YOLO+litparse overhead exceeds the docling-on-text-pages it skips, because docling
on a text page is already cheap. (litparse's o1/o3 cost is also inflated by per-call
`lit` startup.)

**Decision:** Do **not** gate docling by table-presence.

## 4. Where docling spends time — the table-structure model

**Q:** What drives docling's per-page cost?

**Method:** `scripts/docling_stages.py` — per page, `litparse` vs `docling text`
(`do_table_structure=False`) vs `docling text+tables` (`=True`); `table-model` =
the difference.

| page | litparse | dl-text | dl-tables | table-model |
|---|---|---|---|---|
| full-page image slide (0 words) | 2685 | 118 | 1074 | **957** |
| full-page image slide (0 words) | 2164 | 98 | 996 | **899** |
| image + text layer (22 imgs) | 2293 | 160 | 143 | −17 |
| plain text | 203 | 147 | 137 | −10 |
| 6 imgs + 2 tables | 2536 | 114 | 547 | 433 |
| dense financial table | 204 | 113 | 1386 | **1273** |

**Result:** docling's cost **is** the table-structure model. It does ~1.3s of real
work on a dense table, ~0 on text/image+text pages, and **~950ms wasted on a
full-page image it misreads as a table**. `litparse` *also* chokes on image pages
(~2–2.7s vs ~200ms).

**Decision:** The one gate that pays is skipping **image / no-text pages**.

## 5. Image pages — the real expensive case (reconciliation)

**Q:** Are "image pages" expensive? (one early measurement said images were cheap.)

**Result:** Two different page types. **Image + text layer** (e.g. a slide with 22
embedded images and a text layer) is cheap (~144ms — the table model doesn't fire).
**Full-page rasterized / 0-word slides** (e.g. the Q4 earnings deck, pages 13–25 are
100% image, 0 words) are expensive (~950ms — the table model fires on the image).

**Decision:** Triage on **text-layer presence**, not image count. A 0-word image
page is the one to skip.

## 6. Blank vs content — the cheap discriminator

**Q:** If we skip image/no-text pages, how do we avoid throwing away a page that has
real content we'd want to OCR later?

**Method:** render a ~40dpi grayscale thumbnail (~10ms), measure PIL stats.

| page | words | ink (255−mean) | **stddev** | render ms |
|---|---|---|---|---|
| genuinely blank (brk p2) | 0 | 0.0 | **0.0** | 1 |
| full-image content slide | 0 | 16.5 | **33.5** | 6 |
| dense table | 244 | 17.2 | 37.6 | 8 |
| chart slide | 117 | 33.3 | 70.7 | 7 |

**Result:** **stddev (spatial complexity)** separates cleanly — blank/decorative ≈
0.0, any content ≥ ~33 — at ~10ms. stddev (not darkness) is the right signal: a
solid decorative block is dark but flat.

**Decision:** Stage-0 triage: no-text page → thumbnail stddev. Flat → `blank` (skip);
varied → `image_content` → `ImageRef{OcrDeferred}` (recorded OCR target, never
silently dropped).

## 7. Region-scoped docling — whole-page beats crop

**Q:** Crop the table for docling (faster?) or run whole-page?

**Result:**
- wall-clock per call: whole-page 6.6s, crop 5.1s; warm per-convert: crop **318ms**
  vs whole **551ms** (crop ~1.7× faster on the *work*, but model-load dominates per
  call).
- Empirically, docling reports a crop as its **own page in crop-relative
  coordinates** → needs `crop_to_page(+x0,+y0)`; a crop also **inherits the region
  clip** (it only sees the tight box). Whole-page is clip-immune (docling bounds the
  table from the full page) and diagram-immune (§4/§5).
- Corpus (§3): the crop pipeline (o2, 495s) wasn't the winner anyway.

**Decision:** **docling whole-page** is the default parser — clip-immune,
diagram-immune, clean bboxes; cropping reserved for a possible future escalation.

---

*Generated 2026-06-22. Harness scripts are committed; re-run to refresh numbers.*
