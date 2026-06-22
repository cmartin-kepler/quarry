# Build order — YOLO-free pipeline (exact setup)

Companion to `doc-architecture.md`. The pipeline is **triage → docling + litparse →
repair → materialize**, all on the append-only/resolve substrate. This is the
ordered, concrete build.

## Already built — reuse as-is (no work)

- **Substrate** (`store.rs`): `FlatStore` (append-only `write`/`current_artifacts`),
  `Observation`, `resolve(observations, verdicts)`, `register_or_match` + `element_id`,
  `Origin{Parser|Manual}`. Corrections at any level are `Manual` artifacts that win
  via `resolve` — the repair loop needs nothing new here.
- **Artifacts** (`artifact.rs`): `HtmlTable`, `TextGrid`, `ImageRef`, and
  `ArtifactKind::DbTable` (tag exists; payload is Phase 4).
- **Detectors** (`check.rs`): `StructuralValidity`, `IntrinsicArithmetic`,
  `ReconstructionError`, `cross_tier_agreement` — the "makes sense" oracle. `quarry
  judge` already runs the panel.
- **Sidecars + adapters** (`sidecar.rs`, `docling.rs`): `DoclingSidecar` +
  `artifacts_from_docling`; `LiteParseSidecar` + `textgrid_from_json` +
  `scripts/litparse_region.py`. All shell out via `uv run` (PEP 723 envs).
- **Structured text** (`structure.rs`): reading-order blocks, if we want page prose.

## Parked (removed from core, kept for a future need)

YOLO env + layout sidecar; `coords.rs` map #1 (pixel→point); `region_check.rs` /
`segment.rs` / `columns.rs` (→ the optional model-free "did docling miss a table"
cross-check); region-scoped docling + map #2 (crop→page).

## Config (the knobs, set once)

- **triage:** `W_text` ≈ 30 words; `ε_stddev` ≈ 5; `image_frac` ≈ 0.5.
- **docling:** `do_ocr=False`, `do_table_structure=True`, `generate_picture_images
  =False`, picture classification/description off.
- **identity:** `element_id` IoU τ = 0.7.

---

## Phase 1 — Stage 0 triage  (independent; cheap; do first)

1.1 **`scripts/triage.py`** (PEP 723: `pdfplumber`, `pypdfium2`, `Pillow`) → per-page
   JSON `[{page, words, image_frac, stddev, klass}]`, `klass ∈ {text, image_content,
   blank}`. (`words`, `image_frac` from the PDF; `stddev` from a ~40dpi grayscale
   thumbnail.)
1.2 **Rust router** consuming that JSON: `text` → Stage 1; `image_content` →
   `ImageRef{status: OcrDeferred}`; `blank` → record blank, skip. Add a `status`
   enum field to `ImageRef` (`Parsed`/`OcrDeferred`/`Blank`).
1.3 **CLI** `quarry triage <pdf>` → per-page classes + emitted markers. Test on the
   known pages: brk p2 (blank), Q4 slide p20 (image_content), brk p50 (text).

**Deliverable:** every page routed in ~10ms; image/blank pages never reach docling;
content-image pages recorded as OCR targets (invariant 11).

## Phase 2 — Stage 1 parse  (mostly wiring existing sidecars)

2.1 **docling per text page → `HtmlTable`(s):** run `DoclingSidecar` with the config
   above; ingest via `artifacts_from_docling` (bbox + cells). Register each table as
   a source slot: `register_or_match` on docling's bbox → `element_id`.
2.2 **litparse on docling's bbox → `TextGrid`:** for each `HtmlTable`, run
   `LiteParseSidecar`/`litparse_region.py` on that bbox → `TextGrid`, **same
   `element_id`** (a sibling cross-check artifact).
2.3 **Persist** via `FlatStore.write`; everything read through `resolve`.
2.4 **CLI** `quarry parse <pdf>` (the new default) → store of `HtmlTable`+`TextGrid`
   per table.

**Deliverable:** real PDF → `{HtmlTable, TextGrid}` per table in the store, end-to-end.

## Phase 3 — Stage 2 repair HTML  ("until it makes sense")

3.1 **Run the panel** per table: `StructuralValidity`, `IntrinsicArithmetic`,
   `cross_tier_agreement(HtmlTable, TextGrid)` → flags. (Reuse `judge`.)
3.2 **Repair actuator** → corrected `HtmlTable` with `Origin::Manual`:
   - v1 rule-based: cross_tier value disagreement → prefer the litparse token;
     obvious structural fixes (split `$`/sign rejoin, ragged-row patch).
   - v2 LLM sidecar: HTML + TextGrid + flags (+ source crop) → corrected HTML.
3.3 **Loop:** re-run the panel on the repair; stop when clean or max-iters; otherwise
   surface as ambiguous. Repairs win via `resolve` (Manual > Parser).

**Deliverable:** tables that pass the panel, corrections recorded + auditable.

## Phase 4 — Stage 3 materialize → `DbTable`  (the queryable payoff)

4.1 **`DbTable` payload** + `Artifact` impl + `StoredArtifact::DbTable`
   (`from_dyn`/`into_dyn`/`meta`). Holds typed columns (incl. multi-index), rows,
   dtypes, and the source `HtmlTable` `element_id`.
4.2 **`Materialize` op** `HtmlTable → DbTable`: grid → typed columns/rows.
4.3 **Normalizer — multi-index headers:** N header rows → multi-level column index;
   parse docling's dotted names (`2023.Amount`).
4.4 **Normalizer — section-header promotion:** detect label-only rows → new column,
   forward-fill down to the next section (filterable `segment`).
4.5 **Repair `DbTable`** (Manual via `resolve`) "until it queries sensibly."
4.6 **CLI** `quarry materialize` / a query/export path (dataframe / DB).

**Deliverable:** queryable `DbTable` — multi-index resolved, sections as filterable
columns.

---

## Sequencing

Recommended: **Phase 1 → Phase 2 → a thin Phase 4 (MVP) → Phase 3 → full Phase 4.**

The **MVP vertical slice** to prove the whole thing end-to-end early:
`triage(text) → docling HtmlTable → basic Materialize (no normalizers/repair) →
DbTable loads as a dataframe`. Then thicken with the litparse cross-check (2.2),
repair (Phase 3), and the normalizers (4.3/4.4).

Rationale: Phase 1 is independent, cheap, and immediately useful (and is the only
real cost win). Phase 2 is the core parse and is mostly wiring sidecars that already
exist. Phase 4 is the payoff and only needs Phase 2's `HtmlTable`. Phase 3 needs
Phase 2's `{HtmlTable, TextGrid}` pair and can interleave with Phase 4.
