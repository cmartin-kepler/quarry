# Implementation Plan: folding the prototype into the Rust crate

Companion to `doc-parsing-design-brief.md`. The brief states the problem and the
trait skeleton; this plan reconciles **what the Rust crate already is** with **the
concepts the `scripts/trajectory_server.py` prototype evolved**, and proposes how
to bring them together without losing the brief's invariants or the Phase-0
catch-rate harness.

## 0. Where things stand

- **`src/` (Rust) = the brief's Phase 0, faithfully.** Object-safe `Artifact`
  (`Text`, `HtmlTable`), `Extractor` (`ExtractInput::{DocumentRegion, Artifacts}`),
  `QualityCheck` (`IntrinsicArithmetic`, `StructuralValidity`) + claim-time
  `AnswerSupport`, `Adjudicator`/`Verdict`, resolved `SourceAnchor`/`Provenance`
  DAG, `RiskMarkers`, a flat store with a current-view fn, and the `eval`
  catch-rate harness. External PDF/Docling parsing is done by sidecars whose output
  the crate ingests (`import-docling`).
- **`scripts/` (Python) = an evolved prototype** that discovered a richer model:
  a typed **op graph** (layout · extract · transform · merge), **Region** and
  **TextGrid** as first-class artifacts, **structure-by-word-geometry**, **region
  merge** (cross-model bbox agreement), **multi-index** typed tables, **crop-to-bbox
  LiteParse**, evidence-guided transforms, and an evolution/diff UI.

These two have drifted. This plan re-anchors the prototype's concepts on the
crate's traits.

## 1. The reconciling insight

The prototype's four op kinds are **all `Extractor`s** in the brief's sense — they
differ only in what they accept/produce and their fan-in/out arity:

| prototype op | `Extractor` shape | arity |
|---|---|---|
| **layout** (find_tables, yolo, surya, docling-layout) | `DocumentRegion(page)` → `[Region]` | 1→N |
| **extract** (pdf-text, Docling, reducto, llamaparse) | `DocumentRegion(region)` → `Table` | 1→1 |
| **text extract** (text-table, text-page = LiteParse) | `DocumentRegion(region|page)` → `TextGrid` | 1→1 |
| **structure** | `Artifacts([TextGrid])` → `Table` | 1→1 |
| **transform** (sign-fix, markdown, materialize) | `Artifacts([Table])` → `Table`/`Typed` | 1→1 |
| **merge** (region consensus) | `Artifacts([Region])` → `Region` | N→1 |

So we do **not** invent a parallel system. We: (a) add the missing `ArtifactKind`s,
(b) add concrete `Extractor`s for each op, (c) add one piece of descriptive
metadata (`OpKind`) for the graph, and (d) keep `QualityCheck`/`Adjudicator`
exactly as they are. Evidence is gathered at production by running the checks —
which is precisely what the prototype's `quarry explain` already does.

## 2. Concept → crate mapping

| prototype concept | lands as |
|---|---|
| Page / Region / text-grid / table / typed artifacts | `ArtifactKind::{… Region, TextGrid, …}`; `DbTable` → **`TypedTable`** payload |
| op graph edges | concrete `Extractor`s; new `Extractor::op_kind() -> OpKind` for display |
| append-only, inputs retained | `Provenance::Derived { parents, anchor }` + append-only store (brief §3, §7) |
| evidence at production (no validate node) | run `QualityCheck`s right after extract; attach `CheckOutcome`s; **no validate op** |
| vision (removed in prototype) | stays the brief's claim-time `AnswerSupport` trait — **not** a parse-time op |
| region merge / cross-model agreement | a **merge `Extractor`** (`[Region]→Region`, per-edge median bbox) + `Adjudicator` `Equivalent` when tiers agree |
| structure-by-word-geometry | native Rust `Extractor` (`TextGrid→Table`); pure geometry, unit-testable |
| multi-index headers | `Cell.is_header` already exists; add header-level tuples to `TypedTable` columns |
| evidence-guided transform (keep-if-improves) | pipeline policy: compare child vs parent `CheckOutcome`s; record the decision as an `AdjudicationRecord` |
| crop-to-bbox LiteParse | a sidecar `Extractor` that crops the PDF to the region bbox and shells out to `lit` |
| lazy / on-demand escalation (`route()`) | demand-driven re-parse policy over the store (brief §6/§7 — deferred, but the graph supports it) |

## 3. What becomes native Rust vs a sidecar

A lot of the prototype's "smarts" are pure functions that port cleanly to Rust and
should — that's where the structure the user is asking for comes from:

- **Native Rust extractors/transforms:** structure-by-geometry (`structure_words`),
  `sign-fix`, `markdown` round-trip, region **merge** (median bbox), `materialize`
  *metadata* (typed columns, header levels, DDL, per-cell provenance), and the
  existing geometric `pdf-text` reconstructor. Plus the existing detectors.
- **Sidecar extractors (shell out + ingest, like `import-docling`):** LiteParse
  (`lit`, crop-to-bbox), Docling, layout models (YOLO / DocLayout / Surya), and the
  cloud parsers (Reducto / LlamaParse). Each is an `Extractor` whose `extract()`
  invokes the tool and returns `Artifact`s with resolved anchors.
- **DuckDB / Polars** materialization stays a sidecar exporter; the Rust
  `TypedTable` owns the schema, header levels, units, and per-cell provenance, and
  emits the DDL. (Brief §4: this lives *alongside* the canonical DuckDB tables.)

Net effect: the engine (artifact graph, anchors, evidence, adjudication, store, and
the geometry/typing logic) is Rust; Python shrinks to thin sidecar adapters + the
UI.

## 4. New types to add (sketch, against existing traits)

```rust
// artifact.rs — extend the closed kind tag; payloads stay open via as_any().
enum ArtifactKind { Text, Region, TextGrid, HtmlTable, TypedTable, ChartData, Index }

struct Region   { meta: Meta, label: String, conf: f32 }              // located area; anchor = its bbox
struct TextGrid { meta: Meta, text: String, words: Vec<Word> }        // LiteParse ASCII + word geometry
struct TypedTable {                                                   // the non-reversible materialization
    meta: Meta, columns: Vec<TypedColumn>, n_rows: u32, ddl: String,
}
struct TypedColumn { name: String, levels: Vec<String>, dtype: DType, // multi-index path
                     cells: Vec<ProvCell> }                           // per-cell surface+transforms+anchor

// extract.rs — one bit of descriptive metadata; the trait is unchanged.
enum OpKind { Layout, Extract, Transform, Merge }
trait Extractor { /* … existing … */ fn op_kind(&self) -> OpKind; }
```

Everything else (`Meta`, `Provenance::Derived{parents,…}`, `Cell` w/ its own anchor,
`CheckOutcome`, `Verdict`) already supports the graph as-is.

## 5. Invariants we make explicit (prototype ↔ brief)

1. **Append-only, non-destructive ops** — every op produces a *new* artifact;
   inputs are retained as `parents`. (Brief §3; prototype's whole DAG model.)
2. **Resolved anchor on every node** — Region's anchor = its bbox; derived
   artifacts carry the materialized source anchor. O(1) citation. (Brief §3.)
3. **Evidence at production, not a verdict node** — checks run when an artifact is
   made; their outcomes are the evidence. Vision is claim-time only. (Brief §2/§4;
   prototype removed the modeled vision op.)
4. **Op arity is part of the taxonomy** — layout fans out (1→N), merge fans in
   (N→1), extract/transform preserve arity. (`OpKind`.)
5. **Path-dependent representations** — different parsers yield genuinely different
   artifacts (TextGrid vs HtmlTable vs TypedTable), not just quality tiers.
   (Brief §1/§4 note.)
6. **Quality is not a total order** — `Adjudicator` picks a default; agreement →
   `Equivalent`; only genuine ambiguity reaches agents. (Brief §4.)

## 6. Build order (riskiest-respecting, non-breaking)

Phase 0 (catch rate) must keep working; every new artifact kind keeps feeding the
checks and `eval`.

- **A. Artifact-model extension.** Add `Region`, `TextGrid`, `TypedTable` kinds +
  payloads + `StoredArtifact` variants; add `OpKind`. No behavior change; `eval`
  still green.
- **B. Native transforms/extractors in Rust.** Port `structure` (TextGrid→Table),
  `sign-fix`, `markdown`, `merge`, and `materialize`-metadata. Unit tests on the
  geometry/typing. This is where the bulk of the prototype's logic becomes Rust.
- **C. Sidecar extractors.** Generalize `import-docling` into an ingest path; wrap
  LiteParse (crop-to-bbox), layout models, and cloud parsers as `Extractor`s.
- **D. Append-only registry + lineage + current-view** (brief §7): persist the op
  graph as an artifact log + `element_lineage` edges (`same|split|merge`), current
  state = `DISTINCT ON (element_id) … generation DESC`. Fence reads behind one fn.
- **E. Orchestration + UI.** Demand-driven escalation policy (the prototype's
  `route()`), and repoint the interactive trajectory UI at the Rust core (see
  decision D1). The evolution/diff view becomes a view over the artifact DAG.

Throughout: keep the catch-rate number honest — re-run `eval` as each artifact kind
lands; add the new artifact kinds to the ground-truth diff.

## 7. Decisions (locked)

- **D1 — UI/orchestration boundary → thin Python client.** The Rust crate is the
  real engine used by the app; the Flask server is *only* the throwaway test UI.
  It stays thin: sidecar adapters + the UI, driving the Rust core (long-running
  `quarry serve` or per-op CLI). No app logic in Python.
- **D2 — Append-only store → at step D**, after the artifacts/ops exist (not up
  front). Flat store until then.
- **D3 — `OpKind` metadata → yes.** Add `Extractor::op_kind()` (Layout / Extract /
  Transform / Merge) for the graph.
- **D4 — Materialization → deferred (future work).** No DuckDB/Polars export this
  pass, and therefore **no `TypedTable` kind yet** — leave the `DbTable` enum slot
  as the placeholder. The non-reversible materialize op is out of scope for now.
- **D5 — Scope of first pass → A+B (narrow).** Land the pure-Rust core (Region +
  TextGrid kinds, `OpKind`, and the native transforms structure / sign-fix /
  markdown / merge), demoed end-to-end on the `.qdoc` fixtures via `eval`/CLU with
  **no sidecars** (a native TextGrid is built from the fixture's own `Span`s).
  Sidecars (C), registry (D), orchestration + UI repoint (E) follow in later passes.

## 8. First pass (A+B) — concrete checklist

- `ArtifactKind::{Region, TextGrid}` + `Region`/`TextGrid` payloads + `StoredArtifact`
  variants; `OpKind` + `Extractor::op_kind()`.
- Native extractors/transforms (pure, unit-tested):
  - `text-grid` : `DocumentRegion` → `TextGrid` built from the `.qdoc` `Span`s in
    the region (the native TextGrid source; LiteParse is a later sidecar swap-in).
  - `structure` : `[TextGrid]` → `HtmlTable` by word geometry (port `structure_words`
    incl. multi-row header detection).
  - `sign-fix` / `markdown` : `[HtmlTable]` → `HtmlTable`.
  - `merge` : `[Region]` → `Region` (per-edge median bbox).
- Wire them into `pipeline`/CLI so an op chain runs; keep `eval` green and add the
  new kinds to the checks path. Unit tests on geometry/typing/merge.

## Progress

- **A + B — done.** Region/TextGrid kinds, OpKind, and the native ops
  (regions/text-grid/structure/sign-fix/markdown/merge) in `ops.rs` + `grid.rs`,
  with the column coalescer (`grid::column_intervals`) extracted and intent-tested.
- **C — in progress.** `ExtractCtx` now carries `source_path` (the original file).
  `src/sidecar.rs` wraps external parsers as `Extractor`s that shell out (an
  injectable `Vec<String>` command, so tests drive it with a fixture-echoing stub):
  - **Docling** (`DoclingSidecar`, DocumentRegion → HtmlTable) reusing the proven
    `docling::artifacts_from_docling` adapter,
  - **LiteParse** (`LiteParseSidecar`, Region → TextGrid) + the pure
    `textgrid_from_json` adapter; bridges `scripts/{litparse_region,docling_parse}.py`.
  - **Layout** (`LayoutSidecar`, page → Region(s)) + `regions_from_json`; bridge
    `scripts/layout_detect.py` (reuses `yolo_layout.py`).
  - **Cloud** (`TableSidecar`, page → HtmlTable) + `tables_from_json` — a generic
    cell-based contract for Reducto/LlamaParse.
  - All shell-out + adapt paths tested via a fixture-echoing stub (52 tests).
  - **CLI wiring done:** `pipeline::extractor_by_id` (one registry for native ops +
    sidecars) and `run_document_extractor`; `quarry parse <file> --op <id>
    [--source <pdf>]` runs any document-region op, threading `source_path`.
    Artifact-consuming ops (structure/sign-fix/…) are rejected with a clear
    "run it inside an op chain" message (chains = orchestration, step E).
  - **Remaining C (glue only):** `scripts/cloud_parse.py` — wrap Reducto/LlamaParse
    into the cell-based contract (needs API keys). The crate side is complete.
- **D — done.** `store.rs` is now the append-only registry (brief §3, §7):
  - `write` APPENDS `observations.jsonl` (registry rows) + `lineage.jsonl` (DAG
    edges with `relation`: Derive / Merge; Same / Split await matching) +
    `verdicts.jsonl` — nothing overwritten.
  - `current_view` is the pure `DISTINCT ON (element_id) ORDER BY generation DESC`
    query, behind the one `current_artifacts` access function; tested directly on
    synthetic re-parses.
  - `manifest.json` is kept as a re-derived current-view snapshot for the Python
    tools. `check`/`explain` work unchanged. Cross-generation element matching is
    still deferred (brief §6) — `element_id == artifact id` for now.
- **Remaining: E** — demand-driven op-chain orchestration (so transforms/merge run
  as chains, with the route() escalation policy + lazy staleness) and repointing
  the thin Python UI at the Rust core.
