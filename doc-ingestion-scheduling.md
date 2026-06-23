# Ingestion scheduling — get the cheap pages done first

**Goal.** When ingesting many documents in parallel, minimize the **sum of per-page
finish times** (mean flow time) — i.e. have as many pages *done* as early as possible,
rather than minimizing when the *last* page finishes (makespan). Results then stream:
the bulk of the corpus is queryable quickly, with the expensive tail draining at the end.

## The policy: shortest-processing-time-first (SPT)

Minimizing the sum of completion times is a classic scheduling result — it's minimized by
processing units in **ascending order of cost** (SPT; optimal on a single worker, and
list-SPT is optimal for total completion time on identical parallel workers). So the whole
problem reduces to: *cheaply estimate each page's cost, then process cheapest-first.*

## Why this fits quarry: triage is a near-free cost oracle

Triage (`scripts/triage.py`) is ~2.5 ms/page — pypdfium native char-count plus a ~40dpi
thumbnail stddev on low-text pages. That's negligible next to parsing, so we can **triage
the entire intake up front, in parallel**, and get a per-page cost class *before*
committing any docling/OCR work. Cost tiers (from `evidence/09`, `evidence/10`):

| page class | ~cost | work |
|---|--:|---|
| blank | ~0 | skip (recorded, invariant 11) |
| text (born-digital) | ~0.4–0.85 s | docling, no OCR |
| text + figure | +0.5–1 s | docling OCRs only the figure region |
| image / scanned | ~2.3 s | docling+OCR, or ~1 s lean standalone OCR |

The cost label triage produces is exactly the split the OCR decision draws (`evidence/10`):
docling self-gates OCR, so a "text page" carries **no hidden OCR cost** — the cheap tier is
genuinely cheap, and the expensive tier is precisely the no-text pages.

## Shape

```
intake (many PDFs)
   │  triage-all  (cheap, parallel — classify every page)
   ▼
global priority queue of pages, keyed by estimated cost (ascending)
   │  warm worker pool pulls cheapest-first
   ▼
append-only store  ← pages complete out of order; artifacts land + are queryable immediately
```

- **Order:** blank → text → text+figure → image/scanned. Small all-text docs drain first;
  the scanned doc (Abacus, 133pg) and image decks form the tail.
- **Streaming:** the append-only store has no global ordering dependency, so a page's
  artifacts are consumable the moment it finishes — the OCR tail never blocks the cheap
  bulk.

## Caveats

1. **Granularity vs model load.** Page-level SPT only pays off with a **warm worker pool** —
   docling's model load is per-process, so don't spin it up per page. Either warm workers
   pulling cost-ordered page *batches*, or schedule at the *document* level (coarser but
   simpler: order docs by Σ estimated page cost; small all-text docs first, Abacus/jpm/decks
   last).
2. **SPT optimizes mean flow time, not fairness.** It pushes the big scanned docs to dead
   last *by design*. Correct for "max pages early"; wrong if any single document has its own
   latency deadline (then it needs priority, breaking SPT for that item).
3. **Estimate accuracy.** Triage classifies whole pages; a text page with a large embedded
   figure is under-costed (docling will OCR the figure). Minor — figures add ~0.5–1 s vs
   ~2.3 s for full scans — and refinable with an image-area signal if it ever matters.

## Sketch (not built)

`quarry ingest <dir> [--workers N]`: triage every page across all PDFs → a cost-sorted
work queue → N warm docling workers pull cheapest-first → append to per-doc stores,
emitting completions as they land. Pure SPT on the page cost from triage; the scanned/image
tail finishes last while the text bulk is already done and queryable.
