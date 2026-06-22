# Quarry build plan — invariants and concrete steps

Standalone. PDFs → validated, queryable artifacts (tables **and** structured text). This
document states the properties the system must hold, then the ordered steps to build it.
Companion design notes (`doc-easy-path-plan.md`, `doc-human-in-the-loop.md`) expand the
reasoning; this is the buildable spec.

---

## Part 1 — Intended properties (invariants)

These hold *by construction* at every stage. They are the load-bearing guarantees; every
step below either establishes one or is constrained by it.

1. **Append-only.** Artifacts are never mutated or deleted. A re-parse or a correction is
   a *new* appended artifact; the full history is always recoverable.

2. **Content-addressed artifacts.** Same input → same artifact id. Re-runs are idempotent;
   no duplicate work, no staleness to invalidate.

3. **Source-slot identity is assigned-and-stored, not re-derived.** Every region is a
   *slot* with an `element_id` assigned once and stored. Re-identifying a slot across
   re-runs or model versions is done by **IoU matching against stored regions**, never by
   recomputing a hash from geometry. Every parse and every correction of a slot share its
   id. (Minting a brand-new slot may use a deterministic hash for idempotency; that hash
   is never re-derived to *re-identify* an existing slot.)

4. **One resolution seam.** Every consumer reads "the current answer" through a single
   `resolve(candidates, verdicts)` function — never a raw parser artifact. Resolution is
   per `(element_id, ArtifactKind)`: one slot owns an `HtmlTable` *and* a `TextGrid`
   independently.

5. **Everything is attributable.** Each artifact records `Origin` (who/what made it —
   `Parser{id,version}` by default, `Manual{author}` for corrections) and `Provenance`
   (where in the source). Every assertion is auditable to its source.

6. **Single layout authority, tiered extraction.** Exactly one layout authority (YOLO)
   draws regions per page. Extraction is **cheap-by-default, expensive-on-escalation**:
   the heavy parser fires only when a confidence signal says the cheap one is in doubt.

7. **Cross-checks are decorrelated.** Any independent check must draw on a *different
   evidence channel* than the thing it checks, so its errors are uncorrelated. A second
   model that shares the first's biases is disallowed as a routine check.

8. **Verification is evidence-gated.** No verification machinery is *deployed* against a
   failure not shown by measurement to corrupt answers. Cheap, single-parse checks run
   always-on; expensive checks are gated behind a probe and pre-registered thresholds.

9. **Graduation is additive — zero migration.** Adding adjudicators, a correction-submit
   path, cross-version identity matching, or a richer resolver never rewrites stored data
   or changes call sites. The simple system *is* the full system with a degenerate
   resolver.

10. **Tables and structured text share one substrate.** The structured document references
    tables and captions by `element_id` rather than re-parsing them, so there is one
    source of truth per slot and the same resolver governs both.

11. **No silent gaps — every region is accounted for.** A region the system does not (yet)
    extract still produces a *marker* artifact recording its role and that extraction was
    deferred — e.g. `Figure`/image regions yield an `ImageRef` (bbox, page, optional
    rendered crop, "extraction deferred"). An un-extracted area is therefore an explicit,
    attributable decision, never an empty hole someone later mistakes for a bug. Coverage
    (invariant 8's diagnostics) is complete: ink inside a known image region is *expected*,
    not a miss.

**Cross-cutting: two coordinate maps are where bugs hide — pin them first.**
(i) YOLO pixels → PDF points (`pt = px / s`); (ii) docling-on-crop → page points
(`page = crop + crop_origin`). A wrong conversion silently offsets every downstream
anchor.

---

## Part 2 — Concrete build steps

Order logic: **foundation → fix the proven failure → cheap default extraction →
measure-then-buy the expensive parts → seam and structured text.** Three gates (Step 0,
B′, D′) sit exactly where you'd otherwise build on assumption.

### Step 0 — Claim-level probe *(decision gate; run first)*
Decides how much of A–G to build.
- Sample ~30 real tables across **≥2 domains**.
- Per table, pose 2–3 consumer questions (a column total; a cell by row-label×column-header;
  a year-over-year delta). Answer them off the **cheap parse** (litparse + YOLO region +
  CropBox text layer); score each answer against the source crop.
- Bucket failures by class (faithful / structural-noise / region-fragment) **and** by
  whether the region was correctly scoped — this localizes failures to *layout* vs *parse*.
- **Pre-registered decision:** if cheap answers are ≥95% correct on structural-noise
  tables, drop the expensive cross-tier/docling path for born-digital (shrink to
  `YOLO + litparse`); if cell-lookup answers are wrong, invest where the bucketing points.
- **Deliverable:** a scored sheet + an explicit go/shrink decision that scopes the rest.

### Step A — Substrate *(build first regardless of the probe)*
The forward-compat foundation (invariants 1–5, 9).
- `Region { role: Table|Text|Figure|Caption, bbox, … }`.
- `Origin` on `Meta` (default `Parser{id,version}`); `Provenance` already present.
- `element_id` via a single `register_or_match(region, prior)` seam — **degenerate
  (always-mint) now**, bbox stored on the region so IoU matching can be enabled later
  without migration.
- `resolve(candidates, verdicts)`: trivial rule — explicit verdict wins (none yet) →
  `Manual` beats `Parser` → newest generation. `verdicts.jsonl` consulted even when empty.
- Point **all** output assembly at `resolve`/`current_view`.
- **Deliverable:** append-only store with these types; stub-tested, no parsers needed yet.

### Step B — YoloLayout
- Render page → image at scale `s`; YOLO detect; **pixel→point** convert; emit
  `Region{role}`. **Pin coordinate map (i).**

### Step B′ — Region check + eval gate *(gate)*
Region detection is the proven dominant failure; prove it before trusting it.
- `RegionCheck`:
  - **coverage** (diagnostic): typed-orphan spans (words inside no
    `Table/Text/Caption/Figure` region) **+ ink-difference** (catches content with no text
    layer); auto-classify furniture by cross-page repetition. Ink inside a `Figure` region
    is *expected* (it's a known image, invariant 11), not an orphan.
  - **overlap/gap** and **span-vs-box** (pure geometry).
  - **independent, decorrelated opinion** (invariant 7): whitespace/projection
    segmentation (over-splits where YOLO over-merges), ruling-line/vector graphics (read
    from the PDF content stream — no vision), column x-alignment; compared to YOLO by IoU.
    Disagreement → flag. A *second neural model is ruled out* as routine; a strong opinion
    (docling-layout / LLM-vision) is **escalation-on-disagreement only**.
- Eval on the corpus harness. **Pass bar (two gating bars):** distinct-region IoU-overlap
  < 0.1; YOLO-vs-independent-source agreement (IoU ≥ 0.7) on ≥ 90% of regions. Orphan
  check is a diagnostic, not a bar.
- **Gate:** below bar → fix layout before proceeding.
- **Known residual:** when YOLO and the cheap source are fooled the same way, they agree
  wrongly and this stays silent — caught only downstream at claim time.

### Step C — Default table path + text + cheap checks
- `LiteParseGrid` (spans whose center ∈ region) → `TextGrid`; `GridToHtml` → `HtmlTable`.
- `TextExtract` (Text/Caption regions), `PageText` (whole page, reading order).
- `FigureMarker` (Extract): `Region{Figure}` → `ImageRef` — bbox, page, optional rendered
  crop, `extraction: deferred` (invariant 11). No pixels parsed now; the region is
  recorded as a known image so it's never a silent gap, and a future figure/chart
  extractor is a competing artifact on the same `element_id` (additive, invariant 9).
- Attach the **three cheap, always-on, single-parse checks**: `StructuralValidity`
  (shape/type/empty — catches the *loud* forms of column-mangling: ragged rows, type
  violations, spurious empties), `IntrinsicArithmetic` (total-row reconciliation),
  `ReconstructionError` (parsed cells faithful to the region's source text; blind to pure
  rearrangement by design).

### Step D′ — docling tier decision *(gate)*
- Re-run the cheap-vs-docling quality comparison with litparse fed a YOLO region + CropBox
  text layer.
- **Pre-registered rule:** docling is justified **always-on** only if it beats
  litparse-fixed by ≥ 1.0 mean quality (0–5) on ≥ 20% of born-digital tables; otherwise
  **escalation-only**. Do not wire docling-always before this gate.

### Step C-docling — docling escalation
- `DoclingTable`: set `/CropBox` to the table bbox → hand docling the 1-page PDF (text
  layer intact, no OCR) → **crop→page** offset. **Pin coordinate map (ii).** Fired per
  D′'s verdict (escalation on low agreement / scanned, or always-on).

### Step E — Resolver seam
- Generalize `current_view` into the verdict-aware `resolve(candidates, verdicts)`
  (invariant 4); point all output assembly at it. This is the piece expensive to retrofit
  and free to do now — it keeps the adjudication door open (invariant 9).

### Step F — cross_tier *(gated; the silent-residual insurance)*
- `cross_tier_agreement(html, grid)` by **bbox-IoU** (not label text) — the confidence
  signal *and* the escalation trigger.
- Deploy **only if** the probe shows born-digital schema/value errors that the cheap
  checks miss — i.e. the silent, shape-and-type-preserving column mis-assignment that
  `StructuralValidity` cannot see. Otherwise it stays escalation/OCR-only.

### Step G — Structured text
Tables and text on one substrate (invariant 10).
- `Paragraphs` (line y-cluster → paragraph by spacing/indent), `Headings` (font-style runs
  → levels), `ReadingOrder` (XY-cut/column order — **the crux; multi-column scrambles
  silently — pin it like the coordinate maps**), `DocOutline` (Merge → `StructuredDoc`,
  referencing tables/captions/**figures** by `element_id`). Figures appear in the flow as
  their `ImageRef` marker (with caption if associated), so the outline is complete even
  where extraction was deferred (invariant 11).
- Cheap structural diagnostics only: well-formed heading hierarchy, monotonic reading
  order, every body span in exactly one paragraph. No heavy verification unless the probe
  shows structure errors cause wrong answers.

---

**Throughline:** check YOLO before trusting it (B′), earn docling before paying for it
(D′), and prove failures cause wrong *answers* (Step 0) before building machinery to catch
them (F). Build the substrate (A) and the seam (E) eagerly because they are free now and
expensive to retrofit; gate everything expensive on measurement.
