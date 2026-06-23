# 03 — Corpus cost: does gating docling save time?

**Question.** Does skipping docling on pages without tables save time across a real
corpus?

**Harness.** `scripts/corpus_yolo.py` (warm YOLO over every page) + `corpus_tables.py`
(warm docling: whole-page, on-crop, and litparse per table). Every page of **all 20
PDFs in `input/` = 1061 pages**. Four pipelines, reported as whole-document seconds
(exact — every page run, models loaded once):

- **o1** YOLO + litparse(table regions)
- **o2** YOLO + docling-on-crop(table regions)
- **o3** YOLO-gated docling-whole (+litparse on table pages)  — *the "gate to save" idea*
- **o4** docling-whole on **every** page

```
uv run scripts/corpus_yolo.py --dir input --sample 0 --out yolo.json
uv run scripts/corpus_tables.py --regions yolo.json
```

## Per-document (whole-doc seconds)

| document | pages | tbl% | o1 | o2 | o3 gated | o4 all | o4/o3 |
|---|--:|--:|--:|--:|--:|--:|--:|
| Pershing-Uber | 5 | 0 | 0.3 | 0.3 | 0.3 | 0.5 | 1.4 |
| RBRTEd | 5 | 100 | 1.5 | 1.9 | 1.9 | 0.4 | 0.2 |
| q2-fy26-recs | 5 | 80 | 1.6 | 4.1 | 5.5 | 4.1 | 0.7 |
| 2605.15184 | 9 | 33 | 1.5 | 3.2 | 3.9 | 3.3 | 0.8 |
| brk-2022-letter | 10 | 20 | 1.3 | 6.2 | 5.5 | 5.0 | 0.9 |
| Direct Short | 12 | 42 | 7.7 | 2.6 | 9.7 | 2.5 | 0.3 |
| 1706.03762 | 15 | 27 | 2.0 | 4.3 | 6.0 | 5.5 | 0.9 |
| brk-2023-letter | 16 | 12 | 1.5 | 3.8 | 4.7 | 4.5 | 1.0 |
| 2025ltr | 20 | 20 | 2.2 | 4.6 | 5.6 | 5.0 | 0.9 |
| 2402.01030 | 25 | 32 | 4.9 | 12.0 | 14.6 | 12.6 | 0.9 |
| Q4-FY24-Earnings | 25 | 48 | 26.0 | 11.0 | 35.3 | 11.1 | 0.3 |
| 2602.05014 | 26 | 27 | 4.1 | 5.6 | 8.3 | 7.6 | 0.9 |
| 2604.08538 | 36 | 47 | 10.6 | 12.8 | 22.3 | 14.9 | 0.7 |
| 2605.05242 | 51 | 12 | 6.6 | 7.4 | 12.0 | 10.5 | 0.9 |
| 2026-Annual-IR | 85 | 9 | 9.3 | 8.5 | 13.6 | 13.1 | 1.0 |
| Abacus CDO | 133 | 18 | 93.8 | 30.4 | 111.5 | 28.9 | 0.3 |
| brk-2023-ar | 152 | 46 | 48.7 | 88.7 | 141.4 | 103.7 | 0.7 |
| jpm-2023-ar | 364 | 53 | 100.7 | 274.7 | 365.6 | 288.3 | 0.8 |
| **TOTAL (1061)** | | | **351** | **495** | **804** | **536** | **0.7** |

## Result

**o4/o3 = 0.7× — gating docling by table-presence (o3, 804s) is the *most expensive*
option, pricier than running docling on every page (o4, 536s).** The gate's
YOLO-every-page + litparse-on-table-pages overhead exceeds the docling-on-text-pages
it skips — because docling on a text page is already cheap (~140ms).

Note the docs where `o4 << o3` (RBRTEd 0.2×, Direct Short 0.3×, Q4 0.3×, Abacus 0.3×):
table-heavy or image-heavy docs where the gate adds the most overhead relative to what
it saves.

## Caveats

- litparse's o1/o3 cost is inflated by per-call `lit` binary startup (~0.4s/table);
  its true parse cost is lower (so o1/o3 would shrink with batched litparse).
- This measures *page-level table-presence* gating. The gate that **does** pay —
  skipping **image/no-text** pages — is a different gate; see `04`/`05`.

## Decision

Do **not** gate docling by table-presence. Run docling whole-page on text pages; gate
only image/blank pages (cheap triage).
