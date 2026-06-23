# 07 — docling: whole-page vs region crop

**Question.** For docling on a table, crop to the table region (faster?) or run the
whole page?

## Timing

Wall-clock per call (synthetic table page, models cached):
- whole page: **6.6s**, crop-to-table: **5.1s**, crop no-OCR: 5.3s — modest, and
  dominated by ~4.6s per-process model load.

Warm per-`convert()` (model load amortized, 100× each, OCR off):
| input | min | median | mean |
|---|--:|--:|--:|
| crop (one table) | 312 | **318** | 320 |
| whole-page | 537 | **551** | 550 |

So the crop is ~1.7× faster *on the work* — but only once model load is amortized
(a persistent process). Per cold call, the ~4.6s load swamps the 0.23s difference.

## Coordinate behaviour (empirical)

Cropping `corpus/synthetic.pdf` table 0 (page top-left `78,149.2,438,257.2`) and
running docling on the crop: docling reports it as **its own 360×108 page in
crop-relative coordinates** (e.g. cell "Line item" at crop-local `6, 5.8`). So the
crate must translate back with `crop_to_page(+x0, +y0)` (coordinate map #2) and set the
real page number — implemented + tested as `rebase_crop_tables`.

## Why whole-page wins anyway

- **Clip-immune.** A crop only sees the (slightly tight) layout box, so it *inherits
  the region clip* from `01` (lost header/right-column). Whole-page docling bounds the
  table itself from the full page → clean bboxes, no clip.
- **Diagram-immune / cheap.** docling-whole on text/figure pages is cheap (`04`,`05`),
  and the corpus run (`03`) showed the crop pipeline (o2 = 495s) wasn't the winner.
- **Simpler.** No CropBox plumbing, no coordinate remap, no per-table model reload.

## Decision

**docling whole-page is the default parser** — clip-immune, diagram-immune, clean
bboxes. Cropping (`crop_to_page` + the region-scoped sidecar) is built and parked for a
possible future escalation, not the default.
