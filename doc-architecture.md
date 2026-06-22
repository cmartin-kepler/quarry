# Quarry architecture (converged 2026-06-22)

This is the current source of truth for the table pipeline. It **supersedes the
table-path decisions** in `doc-easy-path-plan.md` (litparse-default / docling-as-
escalation) — those were reversed by measurement. The forward-compatibility
substrate in `doc-build-plan.md` §A (append-only DAG, `Origin`, `resolve`) is
**unchanged** and is exactly what the repair loop below needs.

## Goal

Turn a PDF into a **queryable `DbTable` / dataframe**, not merely HTML. The parse
is *allowed to be imperfect*; a **repair loop** corrects each artifact "until it
makes sense." The end product is filterable relational data — multi-level headers
resolved, section-header rows promoted to columns you can filter on.

## Pipeline

```
PDF page
  │
  ├─ STAGE 0 — cheap triage (no ML, ~ms/page) ─────────────────────────
  │    free signals from the PDF: word_count, image_area_fraction
  │      • words ≥ W (real text layer)        → TEXT PAGE      → Stage 1
  │      • image-dominant / no text layer:
  │            render ~40dpi grayscale thumbnail (~10ms)
  │            complexity = stddev(thumbnail)
  │              • stddev ≈ 0   → BLANK / decorative → SKIP (record "blank")
  │              • stddev > ε   → IMAGE-WITH-CONTENT → ImageRef{ocr: deferred}
  │                               (skip docling's table-model waste now; OCR later)
  │
  ├─ STAGE 1 — parse (text pages) ─────────────────────────────────────
  │    docling whole-page  → HtmlTable(s) + clean table bboxes   (PRIMARY; finds tables itself)
  │    litparse (docling's bbox) → TextGrid (faithful text-layer tokens) (cross-check)
  │    [no external layout model — see below]
  │
  ├─ STAGE 2 — repair the HTML "until it makes sense" ─────────────────
  │    detectors define "makes sense": StructuralValidity, IntrinsicArithmetic,
  │      cross_tier(HtmlTable ↔ TextGrid value fidelity).
  │    flag → repair (rule or LLM) → re-check.  A repair is a Manual-origin
  │      HtmlTable that wins via resolve(). Nothing mutated.
  │
  └─ STAGE 3 — materialize → DbTable (the queryable payoff) ───────────
       HtmlTable → DbTable:
         • resolve multi-index / multi-level headers (e.g. "2023 | Amount/%")
         • promote section-header rows → a filterable column (forward-fill down)
       repair DbTable until it queries sensibly (Manual DbTable via resolve()).
```

## Stage 0 — the cheap triage gate (the *only* gate that saves cost)

Run **before** any heavy parse, per page, from signals that are nearly free:

| signal | source | cost |
|---|---|---|
| `word_count` | PDF text layer (pdfplumber) | ~0 |
| `image_area_fraction` | PDF image boxes | ~0 |
| thumbnail `stddev` | render ~40dpi grayscale + PIL stat | ~10ms |

Decision:
- **Text page** (`word_count ≥ W`, e.g. ~30): has a real text layer → Stage 1.
- **Image-dominant / no text layer:** render the thumbnail and look at **stddev
  (spatial complexity)**:
  - `stddev ≈ 0` → **blank / decorative** → skip; record a "blank" marker.
  - `stddev > ε` → **image with content** → record `ImageRef{ocr: deferred}` and
    skip the expensive parse *now*. A future OCR pass targets exactly these pages,
    knowing they aren't blank (invariant 11: recorded, never silently dropped).

**Why this is the gate that matters (measured):** docling's cost *is* its
table-structure model. It does ~1.3s of useful work on a real dense table, but
**wastes ~950ms on a full-page rasterized slide it misreads as a table** — and
litparse is *worse* on image pages (~2–2.7s). Image pages *with* a text layer are
cheap (the model doesn't fire). So skipping **image/no-text pages** is a real win;
skipping *no-table* pages is **not** (docling on text pages is already ~140ms — the
corpus bench showed gating-by-table-presence costs *more* than it saves). `stddev`
(not darkness) is the right "is there content" signal: a uniform decorative block
is dark but flat; real content has structure.

## Stage 1 — parse (docling default, whole-page, not cropped)

On a text page, run **both**:
- **docling whole-page → `HtmlTable`** (the primary, queryable artifact) + docling's
  own clean table bboxes. Whole-page (not crop) because a crop inherits YOLO's
  ~10–15pt region clip; docling bounds the table itself from the full page. Default
  (not escalation) because docling-on-text-pages is cheap and its HTML is clean
  (no column explosion; `$ (30)` preserved). Keep `do_ocr=False` and picture
  classification/description **off** so figure regions stay ~140ms.
- **litparse → `TextGrid`** (faithful text-layer tokens + geometry): the
  value-fidelity reference, cross-checked against the HTML.

**No external layout model — YOLO is removed.** Every job we once assigned it is
now covered without it: the "worth parsing" gate is Stage 0 (cheaper, no model),
and table detection + clean bboxes come from docling's own internal layout (which
was the reference parse in the probe). Removing YOLO also drops its env
(torch/ultralytics/doclayout-yolo), the layout sidecar, and **coordinate map #1**
(pixel→point — only existed to place YOLO's pixel boxes; docling reports points).
If we ever want an *independent* "did docling miss or mis-bound a table" check, it
is the **model-free geometry already built** — ruling-lines/vector graphics,
whitespace XY-cut segmentation, and column-alignment over the PDF text layer — which
is cheaper than YOLO, decorrelated, and needs no render or GPU. Add it only if
measurements show docling missing tables; until then docling's detection stands.

## Stage 2 — repair the HTML

The parse need not be perfect; "makes sense" is defined by the detectors and drives
repair:
- `StructuralValidity` (shape/type/empties — the *loud* column mangling),
- `IntrinsicArithmetic` (totals reconcile),
- `cross_tier(HtmlTable, TextGrid)` (does the HTML preserve the text-layer values? —
  catches docling dropping/altering a number),
flag → repair (rule or LLM) → re-check. A repair is a `Manual`-origin `HtmlTable`
that wins via `resolve()`. This is why **column/row mangling stops being fatal** —
it's corrected here, not required to be perfect at parse.

## Stage 3 — materialize → DbTable (the queryable payoff)

The genuinely new work, and where "queryable" is earned. `HtmlTable → DbTable`:
- **multi-index headers** — stacked headers (`2023 | 2022 | 2021` over `Amount | %`)
  parsed into a real multi-level column index. (docling already hints at this with
  dotted names like `2023.Amount`.)
- **section-header promotion** — label-only rows that head groups (`Insurance`,
  `Railroad`) detected and **promoted into a new column, forward-filled down**, so
  every data row carries its segment and you can `filter(segment=…)`.
then repair the DbTable until it queries sensibly (Manual `DbTable` via `resolve()`).

## Substrate (already built — `doc-build-plan.md` §A)

The repair loop *is* the append-only substrate, already in place:
- append-only DAG; content-addressed artifacts; `element_id` source-slot identity
  (assigned-and-stored, recognized by IoU match — see §5.3 of the build plan).
- `Origin{Parser|Manual}` on every artifact; a repair/correction at *any* level
  (HTML or DbTable) is a `Manual` artifact that wins via `resolve(candidates,
  verdicts)`. No mutation, full provenance/audit.
- `ArtifactKind` already includes `HtmlTable`, `TextGrid`, `DbTable`, and the
  `ImageRef` marker (invariant 11).

## Why these choices (the measurements that drove them)

- yolo26n layout ≈ **80ms/page** (doclayout was ~410ms — model choice, not fundamental).
- docling cost **= the table-structure model**: ~1.3s on a real dense table,
  ~**950ms wasted** on a full-page image, **~0** on image+text pages.
- corpus (1061 pages): docling-whole-every-page (536s) **beats** YOLO-gated-by-table
  (804s) — gating by table-presence *costs* more than it saves.
- blank vs content: thumbnail **stddev** separates cleanly (blank = 0.0; any content
  ≥ ~33), at ~10ms/page.
- region clip is subtle (~10–15pt), systematic across layout models, padding-fixable —
  **not** the failure that matters; **column/row mangling** is, and it's resolved at
  Stage 2/3, not required of the parser.

## Build status

- **Done:** the substrate (§A: append-only, `Origin`, `resolve`, `element_id`),
  reading-order structured text, `ImageRef`/figure markers, the docling/litparse
  sidecars + env via `uv`, and the measurement harnesses (probe, corpus cost, stage
  cost, blank discriminator).
- **Built but now OPTIONAL / not core** (consequence of removing YOLO): the
  region-quality checks + XY-cut segmenter + column-alignment become the *optional*
  model-free "did docling miss a table" cross-check; coordinate map #1 (pixel→point)
  and the layout sidecar are dropped; region-scoped docling + crop→page (map #2) are
  unused in the whole-page-default path (kept for a possible future crop/escalation).
- **To build, in order:**
  1. **Stage 0 triage** (word-count + image-area + thumbnail-stddev → text / blank /
     image-content), emitting `ImageRef{ocr: deferred}` for content image pages.
  2. **Stage 1 default path**: docling-whole → `HtmlTable` + litparse → `TextGrid`,
     wired through the sidecars (already pointed at `uv run`).
  3. **Stage 3 `Materialize` op**: `HtmlTable → DbTable` with the two normalizers
     (multi-index, section-promotion).
  4. **Stage 2 repair loop** wiring (detector flag → repair → re-check) at HTML and
     DbTable level.
```
