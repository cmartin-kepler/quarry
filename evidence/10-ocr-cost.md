# 10 — What does OCR cost, and is it worth it?

OCR is opt-in (`quarry ocr`), targeting the `OcrDeferred` markers (whole image pages +
text-less embedded figures). Two questions: how expensive is it, and how much does
restricting it to no-text content actually save?

## OCR cost per page

| approach | text page | image page |
|---|--:|--:|
| **standalone RapidOCR** (`scripts/ocr.py` — render @216dpi + recognize whole page) | 1024 ms | 757 ms |
| **docling `do_ocr=True`** (over docling-no-ocr ~90ms) | **+83 ms** | **+580 ms** |

The decisive difference: **docling's OCR is text-layer-aware.** With `do_ocr=True` it only
OCRs regions that have *no* text layer — so on a born-digital text page it adds ~83ms
(nearly nothing), and only does real work (580ms) on an image page. Our standalone
RapidOCR pass is blind: it renders and recognizes the whole region regardless, ~1s/page.

## Corpus economics — OCR everything vs only no-text pages

`scripts/ocr_cost.py`, blended ~1007 ms/page (standalone), over all 1061 corpus pages:

```
CORPUS 1061 pages: OCR-everything 1069s  →  OCR-sparingly 211s
                   saved 858s (80%) by skipping the 852 pages that already have text
```

Where the no-text pages are (the only pages that *need* OCR — 209 of 1061):

| document | pages | no-text | note |
|---|--:|--:|---|
| Abacus CDO | 133 | **133** | fully scanned — no text layer at all |
| Q3-FY24 deck | 34 | 34 | rasterized slides |
| Q4-FY24 deck | 25 | 25 | rasterized slides |
| 2026 Investor pres | 85 | 10 | image slides among text |
| brk-2023-ar | 152 | 4 | scattered scanned pages |
| Direct Short | 12 | 2 | |
| jpm-2023-ar | 364 | 1 | |
| *every other doc* | — | 0 | born-digital, full text layer |

## Conclusions

- **OCR is expensive** (~1s/page standalone) and **80% of corpus pages don't need it** —
  they already have a text layer. Running OCR on everything (~18 min) is almost all waste.
- **Sparing use is clearly right.** Restricting OCR to no-text content (the `OcrDeferred`
  gate) saves **80%** of OCR time. The pages that need it are concentrated: scanned docs
  and image decks.
- **Is it worth it?** On a no-text page OCR is the *only* way to get text, so yes — for
  those 209 pages it's necessary, not optional. On the 852 text pages it's pure cost.
- **docling `do_ocr=True` may beat the separate RapidOCR pass.** It's text-layer-aware
  (≈free on text pages, 580ms on image pages — cheaper per image page than our 1000ms
  standalone) and needs no second tool. Trade-off: `do_ocr` is global, so it adds ~83ms
  to every text page (≈71s across the corpus) unless gated to docs/pages that actually
  have no-text content. A future option: enable docling OCR only when triage finds
  image/no-text pages, instead of the standalone pass.

Harnesses: `scripts/ocr_cost.py` (per-page cost + corpus economics), `scripts/docling_ocr_cost.py`
(docling OCR on/off per page).
