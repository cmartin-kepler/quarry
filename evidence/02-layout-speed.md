# 02 — Layout model speed, and removing YOLO

**Question.** Is YOLO layout cheap? Which model? And — once docling parses
whole-page — is an external layout model needed at all?

## Is the model preloaded? (sanity check)

Suspecting the ~0.4s/page YOLO cost was startup, we isolated it (model loaded once
via `yolo_layout._models` cache, then timed on a *cached* page image):

```
render ms/page (scale 2.0):      [8, 6, 6, 5]
PURE detect ms (warm, x10):      min=401  median=402  max=409
torch: 2.12.1   mps available: True   model device: cpu
```

So the model **is** preloaded (warm inference is a stable ~402ms, render is ~6ms,
no reload). The ~0.4s is genuine CPU inference at `imgsz=1024`. **MPS gave no
speedup** (cpu 405ms vs mps 402ms); torch used 6 threads.

## The real lever is input size

doclayout, same page, sweeping `imgsz`:

| imgsz | inference | boxes |
|---|---|---|
| 512 | 133ms | 12 |
| 640 | 188ms | 13 |
| **768** | **236ms** | **14** (same as 1024) |
| 1024 (default) | 402ms | 14 |

`imgsz=768` is a free ~1.7× (identical detections).

## Model choice: yolo26n vs doclayout

In the warm 3-pipeline bench (`speed_yolo.py`), per-page render+detect:
- **doclayout ≈ 410ms/page**
- **yolo26n ≈ 80ms/page** (~5× faster; nano net, even at imgsz 1280)

(I had been using `doclayout` incidentally; standardized on **yolo26n**. Note: both
draw nearly identical — and identically clipping — table boxes; see `01`.)

## The bigger conclusion — remove YOLO

Tracing the converged design (`doc-architecture.md`):
- the "worth parsing" page-gate is the **cheap triage** (word-count + image-area +
  thumbnail stddev) — no model, and cheaper than an 80ms forward pass;
- **table detection + clean bboxes come from docling's own internal layout** (it was
  the reference parse in `01`);
- an independent "did docling miss a table" cross-check, if ever needed, is the
  **model-free geometry** already built (ruling-lines / XY-cut / column-alignment).

**Decision: remove YOLO entirely.** Drops the torch/ultralytics/doclayout-yolo env,
the layout sidecar, and coordinate map #1 (pixel→point). The region-quality modules
become an optional, parked cross-check.
