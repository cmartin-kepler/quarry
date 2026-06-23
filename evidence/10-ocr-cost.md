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

## Is docling smart enough to OCR only what needs it? — yes

Tested on 1706 page 3 (body text + the Transformer architecture figure; the figure's
labels are only obtainable via OCR):

```
do_ocr=False   108ms | body text: ✓ | figure labels OCR'd: ✗ |  8 text items
do_ocr=True    546ms | body text: ✓ | figure labels OCR'd: ✓ | 41 text items
```

docling does **targeted** OCR (default `force_full_page_ocr=False`): it reads the
programmatic text layer and runs OCR **only on bitmap regions that have no text** — here,
the figure (8 → 41 text items, "Multi-Head Attention" recovered). It does **not** re-OCR
the body text (identical both ways). The +438ms is the cost of OCRing that one figure,
not the page. So docling's OCR cost scales with bitmap content, not page count.

### Corpus-wide: docling do_ocr=False vs True (20 docs, 1061 pages)

`scripts/docling_ocr_corpus.py` (warm, whole-doc, per doc → `docling_ocr_corpus.json`):

```
docling no-OCR  622s  →  with-OCR 1036s   (OCR adds 414s, 67%)
```

But the 67% is concentrated — **89% of the OCR overhead is 3 documents**, exactly the
pure scan/image ones:

| document | pages | OCR overhead | share |
|---|--:|--:|--:|
| Abacus CDO (fully scanned) | 133 | +304s | 73% |
| Q3 deck (all image) | 34 | +36s | 9% |
| Q4 deck (all image) | 25 | +27s | 7% |
| **the other 17 text/mixed docs** | 869 | **+47s** | **11%** |

So across the 17 docs that have a text layer, `do_ocr=True` adds only **~8%** (47s over
584s) — and that buys every embedded figure/chart OCR'd surgically. jpm (364pg, 1 scanned)
adds 5%; brk-ar's −17% is run variance (≈4 scanned pages → ≈0 real overhead).

**Design split (backed by these numbers):**
- *Text/mixed docs* → docling already runs the text-page pass, so flipping it to
  `do_ocr=True` gets embedded-figure OCR for ~8% — replacing the separate region-text
  scan + standalone figure-OCR with something built-in and cheaper.
- *Pure scan/image docs* → triage already gates these out of docling (no table-model
  waste), so they never pay docling's ~2.3s/pg OCR; the lean standalone RapidOCR (~1s/pg)
  handles them.

This means much of our separate OCR machinery (triage's text-less-figure scan +
standalone RapidOCR per figure) is redundant for **embedded figures on text pages** —
`do_ocr=True` already does it surgically. The standalone pass still matters for **whole
image/scanned pages**, which we gate *out* of docling (to avoid the ~950ms table-model
waste) — those have no docling pass to piggyback OCR on.

## Conclusions

- **Don't engineer an OCR "sparing" gate — docling already does it.** With `do_ocr=True`
  (default `force_full_page_ocr=False`) docling OCRs *only* the bitmap regions that lack a
  text layer and reads everything else from the text layer. So the right move is to **let
  docling choose**, not to build our own text-less-region detector + standalone-OCR
  targeting. (The earlier "OCR everything = 80% waste" figure is about *blind standalone*
  OCR — a strawman you'd never actually run; it's an argument against the naive standalone
  path, not for a hand-built gate.)
- **The cost lands exactly where it should, automatically.** Corpus: `do_ocr=True` adds
  67% overall but **89% of that is 3 pure scan/image docs**; the 17 text/mixed docs add
  ~8%, which *is* their figures being OCR'd. No tuning needed — overhead ∝ bitmap content.
- **Is it worth it?** On no-text content OCR is the only way to get text, so yes; on text
  it's ≈free because docling doesn't run it there. Worth-it is decided per-region by
  docling, not per-doc by us.
- **Where we still choose:** the *pure scan/image* docs that triage gates **out** of
  docling (to avoid the ~950ms table-model waste). They have no docling pass to ride on,
  so they need a path — and docling-with-everything is ~2.3s/pg there vs ~1s/pg for a lean
  standalone OCR. That's "this doc needs full OCR", not "sparing".

**Net:** flip the docling text-page pass to `do_ocr=True` and delete the bespoke
figure-OCR machinery (region-text scan + standalone per-figure OCR); keep a lean OCR-only
path solely for the gated-out scans/image decks.

Harnesses: `scripts/ocr_cost.py` (per-page cost), `scripts/docling_ocr_cost.py` (docling
OCR on/off per page), `scripts/docling_ocr_corpus.py` (corpus do_ocr false vs true).
