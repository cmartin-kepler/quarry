# 05 — Image pages: which are actually expensive (reconciliation)

**Question.** Are "image pages" expensive in docling? An early single measurement
suggested images were *cheap* — but the intuition (from hands-on testing) was that
image pages were the slow ones. Resolve the contradiction.

## The two conflicting data points

- Early: `2026-Annual-Investor-Presentation` page 23 — **22 embedded images** — docling
  whole-page = **144ms** (fast). And the whole 85-page deck averaged ~154ms/page in the
  corpus run. → "images are cheap."
- Claim: image pages are the expensive ones.

## Finding the genuinely image-dominant pages

`pdfplumber` scan by image-*area* fraction (not count) across the corpus:

```
most image-dominant pages (img-area-frac, words, doc, page):
  1.00  words=0   Q4-FY24-Earnings-Presentation.pdf  p13..p25  (13 pages)
  1.01  words=28  gpmr2026...                         p2
```

So there are **two different kinds of "image page":**
1. **Image + text layer** (slide with embedded images *and* selectable text) — e.g.
   2026pres p23: cheap (~144ms; the table model doesn't fire — `04`).
2. **Full-page rasterized / 0-word slides** — e.g. Q4 earnings p13–25 are 100% image
   area with **zero words** (no text layer at all). These are the expensive ones.

## Confirmed by the stage breakdown (`04`)

Q4 full-image slides (0 words): `dl-tables` = 1074 / 996ms, with the **table-structure
model burning ~950ms** — docling tries to parse table structure out of the rasterized
slide. The same pages with `do_table_structure=False` are ~100ms.

## Resolution

Both observations were right, about different page types:
- "images are cheap" — true for **image + text** pages (model doesn't fire).
- "image pages are expensive" — true for **full-page rasterized / no-text** pages
  (model fires on the image, ~950ms wasted).

**Decision.** Triage on **text-layer presence**, not image count. A 0-word,
image-dominant page is the one to skip (→ `06` for blank-vs-content). Keep
`do_ocr=False` and picture classification/description **off** so figure pages stay
~140ms.
