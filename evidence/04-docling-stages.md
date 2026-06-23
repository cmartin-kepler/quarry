# 04 — Where docling spends time (stage breakdown)

**Question.** What actually drives docling's per-page cost? (And: are image pages
expensive, as suspected?)

**Harness.** `scripts/docling_stages.py` — per page, all `do_ocr=False`, warm:
- `litparse` — the `lit` text-grid parser
- `dl-text` — docling, `do_table_structure=False` (layout + text, no tables)
- `dl-tables` — docling, `do_table_structure=True` (text + table-structure model)
- `table-model` = `dl-tables − dl-text` (the table-structure model's cost)

## Results (ms)

| page | litparse | dl-text | dl-tables | table-model |
|---|--:|--:|--:|--:|
| Q4 slide p20 — full image, 0 words | 2685 | 118 | 1074 | **957** |
| Q4 slide p22 — full image, 0 words | 2164 | 98 | 996 | **899** |
| gpmr p2 — image-dominant, ~28 words | 232 | 103 | 94 | −9 |
| 2026pres p23 — 22 imgs **+ text layer** | 2293 | 160 | 143 | −17 |
| 2026pres p5 — 6 img + 2 tables | 2536 | 114 | 547 | 433 |
| brk p50 — plain text | 203 | 147 | 137 | −10 |
| brk p55 — dense financial table | 204 | 113 | 1386 | **1273** |

## Analysis

- **docling's cost *is* the table-structure model.** `dl-text` (layout + text) is
  ~100–160ms on *any* page. The whole variation is the `table-model` column.
- It does **~1273ms of real work** on a dense financial table (wanted).
- It **wastes ~900–950ms on a full-page image** (0 words) — docling misreads the
  rasterized slide as table content and runs structure inference on it.
- It costs **~0** on text and image-**plus**-text pages (the model finds no table).
- **litparse also chokes on image pages** (~2.0–2.7s vs ~200ms on text/table) — the
  `lit` binary struggles with embedded images. So image pages are a trap for *both*
  parsers.

## Decision

The one gate that pays is skipping **image / no-text pages** (where the table model
wastes ~950ms), *not* no-table pages (already cheap — see `03`). Alternatively docling
could run `do_table_structure=False` on such pages (~1000ms → ~100ms), but skipping is
simpler and avoids litparse's image cost too.
