# Design Brief: Lazy, Iterative Document Parsing for LLM Agents

A brief to hand to a Claude Code agent. It states the problem, the riskiest
assumption to validate first, the core invariants, a Rust trait skeleton to
design against, and a build order that attacks the risk before the architecture.

---

## 1. Problem

Build a document parsing system that turns documents (PDF first; also PPTX and
XLSX) into collections of artifacts — primarily HTML, but also DB tables (DuckDB),
chart/figure data extractions, indexes, and cheaper agent-facing projections —
for LLM agents to interrogate and extract knowledge from.

Parsing is **lazy and iterative**. A cheap rough parse runs on upload. Better and
**different** parses run asynchronously, on demand, only when current artifacts
can't answer the questions agents are asking. Subsequent passes are not merely
*higher quality* — they often produce *different, more usable* artifact types
(HTML table → typed DB table, figure → extracted series, etc.). Laziness exists
because the space of derivable artifacts is open-ended and combinatorial: we
guard against burning compute materializing artifacts nobody needs, not merely
against using expensive parsers.

Every artifact resolves back to coordinates in the original bytes
`(doc_hash, page, bbox)` so every agent claim is citable to its source.

## 2. Riskiest assumption — validate this FIRST

> **Silent parse failures are detectable by something other than the agent.**

The whole quality story rests on this. A cheap parse can produce a clean-looking
table with a transposed column or a shifted row; the agent can't see the original
and won't know to escalate. If our mechanical checks can't catch these at a useful
rate, no architecture saves us — we'd want to know in week 2, not month 6.

**The first deliverable is an eval harness that measures the silent-failure catch
rate**, not the full system. Everything in §4–§5 is scaffolding for that experiment.

Metric: *of the extractions that are actually wrong, what fraction did at least
one detector flag?* Use 20–30 **real, deliberately hard** filings (dense footnoted
tables, multi-level headers, scanned-not-digital pages, restated financials). Clean
digital tables will lie to you optimistically. Hand-label ground truth — this is
the highest-value hour in the project.

Three detectors to test, cheapest first:
1. **Intrinsic consistency** — does the extracted table's own arithmetic hold
   (rows sum to totals, %s sum to 100, balance-sheet identities, YoY columns
   derivable from neighbors)? Nearly free; expected to carry most of the weight
   for financial tables.
2. **Parse-time risk markers** — OCR confidence, column-count variance across rows,
   merged-cell ambiguity, rotated text, suspiciously empty cells.
3. **Answer-side vision verification** — crop the cited source bbox, hand the image
   plus the claim to a vision model, ask "does this support it?" Targeted to what
   was actually used; catches parse errors and agent misreadings alike.

Outcomes and what each means:
- High catch rate → lazy/demand-driven design is justified; build with confidence.
- Mediocre → learn *which* detector carries the weight; redesign around it.
- Low across the board → cheap-first is the wrong default for high-stakes numbers;
  top-tier-parse anything destined for a number an analyst acts on.

## 3. Core invariants (guard these jealously; everything else stays flexible)

- **Append-only.** No in-place mutation of artifacts, geometry, lineage, or
  adjudication records. "Current state" is always a *query* (latest non-superseded
  row), never a file overwrite. This gives total time-travel and a uniform audit
  story. The single failure mode is reading the raw tables instead of the
  current-view query — fence that behind one access function on day one.
- **Resolved geometric anchors on every node.** Every artifact carries a
  *materialized* resolved `SourceAnchor`, even when derived three levels deep, so
  citation lookup is O(1). Derivation forms a DAG (artifacts derive from the
  document *or from other artifacts*), not a star.
- **Agents never touch coordinates.** Agents query semantic indexes (full-text,
  embeddings, structure/TOC, element-type) and receive **opaque element IDs**. A
  registry maps element ID → current geometry. Coordinates live in exactly two
  places: citation records and the job queue.
- **Element IDs are opaque and maintained by matching, not naming.** No positional
  IDs (`page12_table3`) and no quantized/geohash IDs — both have boundary cliffs.
  On re-parse, match new elements to the prior generation by content similarity +
  bbox IoU; a confident match inherits the old ID, otherwise mint a fresh one.
  Splits/merges are recorded as lineage, not forced winners. IDs churn exactly when
  content genuinely changed. (Deferred past the first build — no re-parses yet.)
- **Per-document, per-user keying.** Artifacts are not shared across tenants.
  Everything is keyed by `doc_hash`, so deletion is one keyed sweep with no orphans
  (don't forget index vectors and the eval corpus — they're derived from the doc too).
- **doc_hash IS document identity for now.** Amended filings are wholly new
  documents; choosing the relevant version is upstream and out of scope.

## 4. Rust trait system (the core ask — sketches to design against, not final)

The hard constraint: artifact types and extractors are **open-ended**, and
extractors form a **runtime-decided DAG**. So we need an object-safe core trait for
shared metadata/provenance, plus a strategy for heterogeneous payloads. Two viable
payload strategies — **resolve this explicitly**:
- `enum ArtifactKind { Text, HtmlTable, DbTable, ChartData, Index, ... }` — simpler,
  closed set, easy matching.
- `Box<dyn Artifact>` + `Any` downcasting — open set, more dynamic, looser typing.
Recommended hybrid: object-safe `Artifact` trait for metadata/provenance/risk;
typed payload via downcast or a payload enum.

```rust
// ---- Provenance & geometry (immutable, content-addressed) ----

pub struct DocHash(pub [u8; 32]);
pub struct BBox { pub x0: f32, pub y0: f32, pub x1: f32, pub y1: f32 }

pub enum SourceAnchor {
    Pdf  { doc: DocHash, page: u32, bbox: BBox },
    Pptx { doc: DocHash, slide: u32, shape_id: String },
    Xlsx { doc: DocHash, sheet: String, range: CellRange },
}

pub enum Provenance {
    /// Derived directly from the original bytes.
    Source(SourceAnchor),
    /// Derived from other artifacts; `anchor` is the resolved source anchor,
    /// materialized so citation lookup never walks the chain.
    Derived { parents: Vec<ArtifactId>, anchor: SourceAnchor },
}

// ---- Artifact: object-safe core. Shared metadata only; payload is typed. ----

pub trait Artifact: Send + Sync {
    fn id(&self) -> ArtifactId;          // opaque; stable across re-parse via matching
    fn content_hash(&self) -> DocHash;   // dedup + fast identity fast-path
    fn provenance(&self) -> &Provenance; // DAG edge + resolved anchor
    fn kind(&self) -> ArtifactKind;
    fn generation(&self) -> Generation;  // per-job monotonic counter per document
    fn risk(&self) -> &RiskMarkers;      // parse-time confidence signals (see §2.2)
    fn as_any(&self) -> &dyn std::any::Any; // downcast to concrete payload
}

// Concrete artifacts implement Artifact and expose their own payload accessors:
//   ExtractedText { spans, reading_order, .. }
//   HtmlTable     { html, cells: Vec<Cell /* each cell carries its own anchor */> }
//   DbTable       { schema, duckdb_ref, column_types, unit_normalization }
//   ChartData     { series, cropped_image_ref, description }
//   Index         { kind: Fts|Embedding|Structure, /* region = whole document */ }

// ---- Extractor: consumes a document region OR prior artifacts, produces artifacts ----

pub enum ExtractInput<'a> {
    DocumentRegion { doc: DocHash, anchor: SourceAnchor },
    Artifacts(&'a [&'a dyn Artifact]),
}

pub trait Extractor: Send + Sync {
    fn id(&self) -> ExtractorId;
    fn version(&self) -> Version;        // part of the job dedup key
    fn cost_tier(&self) -> CostTier;     // tiers are FORMAT-SPECIFIC, not a global scale
    fn accepts(&self) -> &[InputKind];   // raw region, and/or specific ArtifactKinds
    fn produces(&self) -> ArtifactKind;
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx)
        -> anyhow::Result<Vec<Box<dyn Artifact>>>;
}
// Note: "quality" means fidelity for PDFs but interpretation for XLSX (which cells
// are headers / unit rows / where one logical table ends). Don't model tiers as one
// global ladder.

// ---- Quality checks: run BEFORE agents see an artifact (the riskiest subsystem) ----

pub enum CheckOutcome {
    Pass { confidence: f32 },
    Flag { reason: String, severity: Severity }, // surfaced to agent / can auto-escalate
}

pub trait QualityCheck: Send + Sync {
    fn id(&self) -> CheckId;
    fn applies_to(&self, kind: ArtifactKind) -> bool;
    fn check(&self, artifact: &dyn Artifact, ctx: &CheckCtx) -> CheckOutcome;
}
// Concrete checks:
//   IntrinsicArithmetic  — table sums/totals/identities (the high-value, ~free one)
//   StructuralValidity   — column counts, header detection, empty-cell patterns
//   CrossTierAgreement   — diff against another tier's artifact for the same anchor
//   AnswerSupport        — vision-verify a CLAIM against the cropped source bbox
//                          (claim-time, slightly different signature; see harness)

// ---- Adjudicator: quality is NOT a total order. Pick a default at parse time;
//      surface only genuine ambiguity to query-time agents. ----

pub enum Verdict {
    Winner(ArtifactId),          // clear best → becomes the default
    Equivalent(Vec<ArtifactId>), // agreement → confidence boost, either works
    Ambiguous(Vec<ArtifactId>),  // ONLY this reaches agents, with alternatives attached
}

pub trait Adjudicator: Send + Sync {
    fn adjudicate(&self, candidates: &[&dyn Artifact], checks: &[CheckOutcome])
        -> Verdict;
}
// Adjudication verdicts are themselves append-only records (inputs, checks, verdict,
// confidence) so "why did the system prefer reading A" is auditable. Agent overrides
// of a default feed the eval loop as a signal against the adjudicator.
```

Design tensions to resolve with the user, not unilaterally:
- payload strategy (enum vs `dyn` + `Any`) above;
- whether `AnswerSupport` is a `QualityCheck` or a separate claim-time trait
  (it takes a claim + artifact, not just an artifact);
- where the artifact store boundary sits relative to the existing per-document
  DuckDB canonical tables (this lives *alongside* them, not in front).

## 5. CLIs / scripts (so the traits are testable on real documents)

Build these against the trait skeleton. The `eval` command is the point of the
whole first build; the others are supporting.

- `parse <file> --tier <n> --out <dir>`
  Run the tier-n extractor(s) for the file's format; emit artifacts (HTML +
  sidecar JSON with anchors, risk markers, content hashes) to `<dir>`.

- `check <artifact-dir>`
  Run all applicable `QualityCheck`s over emitted artifacts; print a table of
  flags (element id, check id, severity, reason).

- `eval <file> --truth <truth.json> [--tier <n>]`  ← **build this first**
  Cheap-parse the file, run the detectors, diff extraction against hand-labeled
  ground truth, and report the **silent-failure catch rate**: of extractions that
  are actually wrong, the fraction flagged by ≥1 detector. Break down by detector
  so you learn which one carries the weight.

- `inspect <file>`
  Dump structure: pages, detected elements, anchors, reading order — for sanity
  checking and for producing ground-truth labels faster.

Ground-truth format: per-document JSON listing the correct extraction for each
labeled table (cells + types + the source anchor it should map to), so `eval` can
diff structurally, not just textually.

## 6. Build order — riskiest first; aggressively stub the rest

**Phase 0 — detection experiment (the only phase that matters at first).**
- Minimal `Artifact` (Text + HtmlTable), one PDF cheap extractor, `RiskMarkers`.
- `IntrinsicArithmetic` + `StructuralValidity` + sampled `AnswerSupport`.
- `eval` CLI + 20–30 hard real filings + hand-labeled truth.
- Output: the catch-rate number. This decides whether lazy/cheap-first is viable
  and which detector to build the system around.

**Stub / defer until Phase 0 answers the question:**
- element identity matching across re-parses (no re-parses yet),
- the full derivation DAG and staleness propagation,
- the append-only registry and current-view machinery (use a flat store),
- PPTX / XLSX extractors,
- the async job queue, dedup/coalescing, escalation budgets,
- agents and the semantic indexes themselves.

These are real and mostly already-believed-in architecture. The first build exists
to attack the one belief that, if false, makes all of them moot.

## 7. Things to get right once Phase 0 passes
- Append-only registry of element observations (`element_id, generation, doc, page,
  bbox, content_hash, status, source_artifact_id`) + `element_lineage`
  (`parent, child, generation, relation: same|split|merge`); current state is a
  `DISTINCT ON (element_id) ORDER BY generation DESC` view.
- Region-scoped, idempotent, coalesced re-parse jobs keyed by
  `(doc_hash, region, target_type, parser_version, cost_tier)`.
- Lazy staleness: on completion, mark DAG children stale but only auto-re-derive
  those recently accessed (last-accessed timestamp), else laziness collapses into
  eager parsing.
- Non-blocking resolution: return stale-but-honest artifacts immediately with a
  marker; reserve synchronous waits for cases where plausibility checks fail.
- Random audit sampling of answered-from regions to get an *unbiased* silent-failure
  rate and a per-element-class risk prior (escalation-driven corpus is biased toward
  failures agents noticed).
- Documents are untrusted input: tag all artifact content as quoted data at the
  prompt level; provenance lets a human eyeball any asserted number.
