# 06 — Blank vs content: the cheap discriminator

**Question.** If we skip image / no-text pages (`05`), how do we avoid throwing away a
page that has *real content* we'd want to OCR later — i.e. distinguish a genuinely
blank/decorative page from an image page with content?

**Idea.** Render a tiny grayscale thumbnail (~40 DPI) and measure ink + spatial
complexity with PIL (`ImageStat`, no numpy):
- `ink = 255 − mean` (darkness)
- `stddev` (spatial complexity)
- `dark-frac` (fraction of non-near-white pixels)

The render runs **only** on low-text pages, so it's near-free overall.

## Results

A genuinely blank page exists in the corpus (brk-2023-ar page 2):

| page | words | ink (255−mean) | **stddev** | dark-frac | render ms |
|---|--:|--:|--:|--:|--:|
| **blank** (brk p2) | 0 | 0.0 | **0.0** | 0.0 | 1 |
| Q4 full-image content slide | 0 | 16.5 | **33.5** | 0.09 | 6 |
| Q4 full-image content slide | 0 | 17.0 | **36.5** | 0.09 | 6 |
| dense table | 244 | 17.2 | 37.6 | 0.12 | 8 |
| plain text | 361 | 23.4 | 45.7 | 0.20 | 9 |
| chart slide | 117 | 33.3 | 70.7 | 0.18 | 7 |

## Analysis

- **`stddev` separates cleanly:** blank/decorative ≈ **0.0**; *any* content (even a
  0-word full-image slide) ≥ **~33**. Enormous margin.
- **stddev (not darkness) is the right signal.** A solid-color decorative block would
  be *dark* (high ink/dark-frac) but *flat* (low stddev). stddev distinguishes
  structured content (text/charts/tables) from a uniform block; darkness alone can't.
- **Cost ≈ 10ms/page** for the render, only on no-text pages (≈10s across the
  1000-page corpus).

## Decision

Stage-0 triage for a no-text / image-dominant page:
- `stddev ≈ 0` → **blank / decorative** → skip (record "blank").
- `stddev > ε` → **image with content** → `ImageRef{OcrDeferred}` — a recorded OCR
  target, never silently dropped (invariant 11). A future OCR pass hits exactly these
  pages, knowing they aren't blank.

Thresholds used: `W_text≈30` words, `W_low≈5`, `image_frac≈0.5`, `ε_stddev≈5`. Wired in
`scripts/triage.py` + `src/triage.rs`.
