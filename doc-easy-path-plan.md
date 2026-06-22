# Plan: the easy-path pipeline (YOLO layout + docling-on-crops), forward-compatible with adjudicated branching

Companion to `doc-parsing-design-brief.md`, `doc-parsing-implementation-plan.md`,
`doc-human-in-the-loop.md`. Premise: build the **simple** extraction path now, but
on the **full append-only data model**, so graduating to LLM/human adjudication and
correction branches later is *additive* — no rewrite, no migration.

> **Two adversarial reviews shaped this plan.** Round one added the measurement gates
> (B′, D′), made litparse the default, and demoted docling to escalation. Round two
> argued the plan may be *too big*: it builds verification for a silent value-error
> failure we never observed, while the failures we *did* see have not been shown to
> cause wrong **answers**. Its conclusions are §0 (run a claim-level probe first; it may
> shrink everything below), pinned thresholds on B′/D′, and an *independent* region
> cross-check in §1a. The architecture is unchanged; the build is now gated on evidence.

---

## 0. Step zero — the claim-level probe (run before building A–F)

The only measurement that answers *"does any of this verification matter?"* is at the
**claim**, not the artifact. Run it first; its outcome decides how much of §1–§7 to build.

**Probe.** Sample ~30 real tables (**widen beyond finance** — the corpus is currently
one domain). For each, pose 2–3 concrete questions a real consumer would ask — a column
total, a cell by (row-label, column-header), a year-over-year delta. Derive each answer
from the **cheap parse** (litparse fed a YOLO region + CropBox text layer — the §1
default). Check each answer against the source crop. Score answer-accuracy, **bucketed by
the table's failure class** (faithful / structural-noise / region-fragment).

**Pre-registered decision rules** (commit before looking):
- **structural-noise tables answer ≥ 95% correct** → structural noise is cosmetic to a
  consumer ⇒ **drop cross-tier + docling for born-digital**; easy path shrinks to
  `YOLO + litparse + GridToHtml`. (F and the docling path become OCR-only.)
- **region-fragment tables answer wrong** (expected) ⇒ confirms region detection is the
  priority; invest in YOLO + B′.
- **any value error on born-digital** (not seen yet) ⇒ cross-tier + docling earn their
  place; build the full §1–§7.

**What the probe does *not* gate.** The three *single-parse* quality checks —
`StructuralValidity` (shape), `IntrinsicArithmetic` (internal reconciliation),
`ReconstructionError` (parsed cells faithful to the region's source text) — are cheap
(grid-only, or grid + the region's *free* born-digital text layer; **no second parser**)
and stay **always-on regardless of the outcome**. They are basic fidelity invariants,
not bets on the value-error class. What the probe gates is specifically
`cross_tier_agreement` **plus the docling escalation that produces the second parse it
consumes** — the only *two-parse*, ML-cost machinery (and the codebase already reflects
this split: the three are `QualityCheck` impls; cross-tier is a standalone binary
comparator). `ReconstructionError` is blind to pure rearrangements by design, and
catching those is exactly cross-tier's job — so whether born-digital needs that
expensive patch *is* the probe question.

If the probe says "shrink," the rest of this document is the *upper bound* of what to
build, not the spec.

---

## 1. Spine

```
page ──YOLO──▶ [Region{role}]   (region-quality check — §1a)
                 │ Table   ──▶ litparse(spans-in-region) ─▶ TextGrid + HtmlTable
                 │              └─ low confidence / scanned ─▶ docling(crop) ─▶ HtmlTable  (escalation)
                 │ Text    ──▶ litparse                   ─▶ Text(section)
                 │ Figure  ──▶ ImageRef marker (extraction deferred, recorded — never a silent gap)
                 │ Caption ──▶ litparse                   ─▶ Text(caption)
                 page-wide ──▶ litparse                   ─▶ Text(page)
```

YOLO is the **sole layout authority** (fast, one forward pass). **litparse is the
default table parser**, now that it gets what it was missing — a correct region (YOLO)
and a clean text layer (CropBox, §4). **docling is escalation, not always-on** (§1b):
it runs on a table only when the cheap parse is low-confidence or the doc is scanned.
When both run, they feed the existing `cross_tier_agreement` check.

### 1a. Region-quality check (first-class — YOLO is the new dominant failure surface)

The experiment's #1 finding was that **region detection** is where silent failures
live. Swapping pdfplumber for YOLO does not retire that risk — it relocates it, and
the downstream cross-check runs *inside* the YOLO box, so a bad box makes both parsers
**agree while wrong**. YOLO therefore gets its own cheap, independent check *before*
its boxes are trusted:

- **coverage** (diagnostic, *not* a gate) — find content YOLO put no box around, in two
  layers: (i) **typed-orphan spans** — words inside no `Table/Text/Caption/Figure` region
  (ink inside a known `Figure` is *expected*, not a miss); and
  (ii) **ink-difference** — rendered ink (or the union of content boxes) minus all
  regions, which also catches content with **no text layer at all** (a scanned/image
  table, vector-drawn text) that span-coverage is blind to. The strongest miss signal is
  *ink present, but no region **and** no span*. Orphans should be page furniture
  (headers, footers, page numbers); a *body-content* orphan means YOLO missed a box.
  Auto-classify furniture by **cross-page repetition** (same text at the same (x,y)
  across pages = running header/footer ⇒ expected orphan), leaving one-off
  content-column orphans as the real misses. Eyeballed, not pass/fail — the page-text
  catch-all covers every span by construction (a "zero orphans" test is vacuous) and
  furniture is legitimately un-regioned (a zero bar would be wrong). Regions with
  near-zero spans are still a detection error worth flagging.
- **overlap/gap** — regions that overlap heavily, or a table box that cuts through a
  span's bbox, signal a mis-drawn boundary.
- **span-vs-box** — every word the region claims should be a word physically inside it
  (re-use the reconstruction-residual idea at the region level).

These geometry checks catch a **gross** miss (no box where text is) but are blind to a
**plausible-but-wrong** box — two stacked tables merged, or a table clipped at a ruling
line — where every span still sits inside *some* region. That is the dangerous, silent
case, and self-consistency cannot see it. So §1a also includes an **independent region
opinion** — and the value of a cross-check is *decorrelation of errors*: it only helps
where the second source fails **differently** from YOLO. Choose the source for *opposite
bias*, not accuracy, and prefer a **different evidence channel** than YOLO's learned
pixel priors:

- **whitespace / projection segmentation** (XY-cut, whitespace rectangles) — cheap,
  deterministic, computed straight from span boxes. Its bias is the *opposite* of
  YOLO's: it over-*splits* on gaps where YOLO over-*merges* on learned priors, so its
  disagreement is informative.
- **ruling-line / vector graphics** — for born-digital the PDF content stream *contains*
  the drawn lines and rectangles (read with pikepdf / `pdfplumber.rects`/`.lines`); no
  vision needed. Ruling lines bounding a table are near-ground-truth; a YOLO box that
  cuts *through* a ruling rectangle is a loud red flag. A wholly different channel ⇒
  strongly decorrelated.
- **column-alignment clustering** — cluster the x-edges of word boxes across rows;
  shared alignment edges = real columns — an independent statement about whether a
  `Table` box actually contains tabular structure.
- compare each to YOLO by IoU; where they **disagree** on a boundary, flag the region.
  **Rule out a second neural layout model (incl. docling-layout) as the routine source**
  — it shares YOLO's learned biases (*correlated*) and would nod along on exactly the
  separator-less merge case, at the heavy per-page ML cost §1b just expelled.
- **escalation, not always-on:** only *when* a cheap source disagrees with YOLO (or the
  table later trips low cross-tier downstream) spend the strong opinion — docling-layout
  or an LLM-vision call — on that one page as the tiebreaker. Same escalation pattern as
  §1b, one level up.

**Named residual (don't oversell the gate):** when YOLO *and* the cheap source are
fooled the *same* way — two tables with no gap and identical columns — they agree
wrongly, nothing escalates, and region-level checks stay silent by construction. That
case is caught only downstream, at **claim time** (a wrongly-merged region yields a wrong
*answer*), which is what §0's probe measures. The region-level defense is layered, not
airtight.

A region failing either check is flagged for escalation/adjudication just like a bad
table — region quality is evaluated with the **same corpus harness** before the rest of
the pipeline is trusted (build step B′).

### 1b. docling's role is decided by measurement, not assumed

The experiment showed cheap and docling **agree on values** where comparable; docling's
wins were front-end (region + text layer) — exactly what YOLO+CropBox now give litparse.
So **litparse-with-a-good-region may match docling on born-digital**, making always-on
docling an unjustified per-table ML cost at ingest scale. We do not assume; we measure
(build step D'): re-run the cheap-vs-docling quality comparison with litparse fed a
YOLO region + CropBox text layer. Outcome decides docling's tier:

- if litparse-fixed ≈ docling on born-digital → **docling = escalation only** (low
  cross-tier agreement, or scanned/low-OCR-confidence docs);
- if docling still clearly better → **docling = always-on for tables**, accepting the cost.

Either way the wiring is the same (docling is an `Extract` op); only *when it fires*
changes, gated by the confidence signal.

### 1c. Structured text extraction (sections, paragraphs, reading order)

Text is a **first-class output**, not a dump. The goal is a *structured document* — a
reading-order flow of paragraphs grouped under a heading hierarchy, with tables and
captions placed in the flow — so a consumer can ask "what does §X say about Y," not just
grep a blob. The current `TextExtract`/`PageText` ops (§2) produce flat `Text`; this
adds the structuring on top.

**Reuse the cheap evidence channels — don't invent a parser.** The same model-free
signals that cross-check regions (§1a) *build* the structure, each on a different
channel:

- **blocks** — YOLO `Region{Text}` already segments the page into text blocks
  (paragraphs / columns). That is the starting partition.
- **paragraphs** — within a block, y-cluster spans into lines, group lines into
  paragraphs by line-spacing + indentation (projection profiles again).
- **headings + hierarchy** — **font-style runs** (size/weight/style from the text layer)
  rank into levels H1/H2/…; a Transform assembles the section tree (a heading owns the
  body until the next same-or-higher heading).
- **reading order** — XY-cut / column structure orders the blocks. **This is the crux
  and where bugs hide** (multi-column interleaving), so pin it like the coordinate maps
  in §3 — a wrong order silently scrambles the document.
- **furniture exclusion** — cross-page repetition (the §1a coverage signal) drops running
  headers/footers/page numbers out of the body flow.
- **lists / captions** — bullet/number-prefix detection → list items (cheap, optional);
  captions associate to the nearest figure/table (existing heuristic).

**One source of truth per slot.** The structured outline **references tables and
captions by `element_id`** rather than re-parsing them — so a corrected table (§6)
appears in its section automatically, and `resolve()`/`current_view` (§5) governs text
artifacts exactly as it governs tables. Structured text is just more artifacts on the
same append-only DAG; a corrected heading or paragraph is a `Manual`-origin artifact
resolved by the same rule. **Zero new mechanism.**

**Don't over-build the checks** (same discipline as §0). Cheap structural *diagnostics*
only: heading hierarchy well-formed (no H3 directly under H1), reading order monotonic,
every body span lands in exactly one paragraph. The probe (§0) should include a
text-structure question ("what does section X say about Y") to see whether structure
errors actually cause wrong answers before any heavier verification is built.

## 2. Components

| op (`OpKind`) | in → out | notes |
|---|---|---|
| `YoloLayout` (Layout) | Page → `[Region{role}]` | render page→image, detect, **pixel→point** convert |
| `RegionCheck` (—) | `Region` → flag | §1a coverage / overlap / span-vs-box; pure geometry |
| `LiteParseGrid` (Extract) | `Region` → `TextGrid` | **default** table parse; spans whose center ∈ region |
| `GridToHtml` (Transform) | `TextGrid` → `HtmlTable` | existing column-coalescer/`to_html`; the queryable view |
| `DoclingTable` (Extract) | `Region{Table}` → `HtmlTable` | **escalation only** (§1b): CropBox crop → docling → **crop→page** offset |
| `TextExtract` (Extract) | `Region{Text\|Caption}` → `Text` | flat text per block |
| `FigureMarker` (Extract) | `Region{Figure}` → `ImageRef` | bbox/page/optional crop, **extraction deferred**; recorded so it's never a silent gap; future extractor is additive on same `element_id` |
| `PageText` (Extract) | `Page` → `Text{page}` | all spans, reading order |
| `Paragraphs` (Transform) | `Region{Text}` → `[Paragraph]` | §1c: line y-cluster → paragraph by spacing/indent |
| `Headings` (Transform) | `[Region{Text}]` → heading levels | §1c: font-style runs → H1/H2/… |
| `ReadingOrder` (Transform) | `[Region]` → ordered blocks | §1c: XY-cut/column order — **pin like §3** |
| `DocOutline` (Merge) | paragraphs + headings + `HtmlTable`/`Text{Caption}` refs → `StructuredDoc` | §1c: section tree; refs tables/captions by `element_id` |

`role ∈ {Table, Text, Figure, Caption}` — a new enum on `Region`. The default table
path is `litparse → TextGrid → GridToHtml`; `DoclingTable` is a *competing* `HtmlTable`
for the same `element_id`, produced only on escalation — which is precisely a branch
the resolver (§5.6) chooses between, so escalation and adjudication share one mechanism.

## 3. The two coordinate maps (where bugs hide — pin these first)

1. **YOLO pixels → PDF points.** YOLO sees the page rendered at scale `s` (px = pt·s).
   Convert every box: `pt = px / s`. Must match the point system litparse spans use,
   or every downstream anchor is silently offset.
2. **docling-on-crop → page points.** docling parsing a crop returns cell boxes
   relative to the crop origin `(cx0, cy0)`; add it back: `page = crop + (cx0, cy0)`.
   (The crate's docling adapter already does BOTTOMLEFT→top-left; this adds a translate.)

## 4. Feeding docling a crop without losing the text layer

- **Born-digital (default):** set the page `/CropBox` to the table bbox (`pikepdf`),
  hand docling that 1-page PDF. docling parses only the table, **text layer intact —
  no OCR**, so no value-error risk reintroduced. (Validate docling honors CropBox; if
  not, fall back to a high-DPI image crop, which forces OCR.)
- **Scanned/OCR:** image crop + docling OCR — unavoidable, and the population where
  docling's OCR actually earns its keep.

---

## 5. The forward-compatibility contract (the "don't preclude" answer)

The easy path is the full model with a **degenerate resolver**. Hold these invariants
now and graduation costs no migration:

1. **Append-only, never mutate.** A correction or re-parse is a new `Observation`
   appended, never an edit. (Store is already append-only: `observations/lineage/verdicts.jsonl`.)
2. **Content-addressed identity.** `ArtifactId::mint(content_hash, generation)` — same
   input → same id, so re-runs are idempotent and branches are distinguishable.
3. **`element_id` is the branch anchor (the "file path") — assigned and stored, not re-derived.**

   `element_id` identifies a **source slot** (a region in a doc), *never* the parse
   content — so every parse and every correction of that slot share it. The slot's id
   is **assigned once and stored**, then recognized on later runs by *matching*, never
   recomputed on read. All identity flows through one seam:

   ```rust
   /// The single identity entry point. Degenerate now (match path off), additive later.
   fn register_or_match(r: &Region, prior: &[Region]) -> ElementId {
       let best = prior.iter().filter(|p| p.role == r.role)
                       .max_by(|a, b| iou(a, r).total_cmp(&iou(b, r)));
       match best {
           Some(m) if iou(m, r) >= TAU => m.element_id, // recognize → reuse stored id
           _                           => mint(r),       // new region → name it
       }
   }
   /// `mint` is deterministic, so same-input re-runs of a fresh doc collide naturally.
   fn mint(r: &Region) -> ElementId { blake3(doc_hash ‖ page ‖ r.role ‖ r.bbox) } // τ ≈ 0.7
   ```

   - **Mint vs match — the distinction that matters.** The bbox hash *names a new
     region*; it is **never re-derived to re-identify an existing one**.
     Re-identification across re-runs and model versions is the **match path**
     (IoU ≥ τ), not re-derivation. That is why a model upgrade which nudges the bbox
     **reuses** the stored id instead of minting a fresh one and orphaning the
     correction bound to it.
   - **Why identity can't be a pure function of source.** Within a page you must
     disambiguate multiple same-`role` tables, and the only disambiguators — bbox or
     reading-order index — are *both* model-dependent. So no pure function of stable
     inputs can name a region; identity is irreducibly assigned-and-stored. (This is
     also why quantizing the bbox buys nothing: snapping just moves the jitter to
     bucket boundaries. The fix isn't a cleverer derivation — it's to stop re-deriving.)
   - **bbox stored on the region** (already is — it's the `Region`) so the match path
     has something to compare. It is stored *alongside* the id, not *as* the id.
   - **Resolution is per `(element_id, ArtifactKind)`.** One table element owns an
     `HtmlTable` slot *and* a `TextGrid` slot; a correction competes only within
     `(element_id, HtmlTable)`, never against the grid.

   **Easy-path degenerate form, and why graduation is genuinely zero-migration.** Until
   corrections exist there is nothing to preserve, so the match path can stay **off**
   (`register_or_match` always mints). Turning it **on** later is purely additive —
   `mint` becomes match-then-mint over the *same* id space; no stored id ever changes,
   and matched ids and freshly-minted ids are the same kind of id. That additivity *is*
   the no-migration guarantee §5 promises. Re-deriving the hash as canonical identity
   quietly broke that guarantee (a model swap silently re-minted every id); routing all
   identity through the one `register_or_match` seam restores it.

   **The genuinely hard cases stay deferred, not precluded.** A table that **splits**
   into two (or two that **merge**) is real entity resolution — two priors each
   half-match one new box, neither cleanly ≥ τ. These map to the existing `Split`/`Merge`
   relations (already in the enum) and are exactly *why* the matcher is deferred: it's
   its own problem, not something to smuggle into a hash.
4. **`Origin` on every `Meta`** now: `Parser{id, version}` (default) — `Manual{author}`
   and `Llm{...}` slot in later without schema change.
5. **Lineage edges recorded.** Every artifact writes a `Derive` edge from its region.
   `Merge/Same/Split` relations already exist in the enum — reserved, unused.
6. **One seam — `current_view = resolve(candidates, verdicts)`.** `current_view`
   already collapses per `element_id` by `generation`. Generalize the pick into a
   single swappable, **verdict-aware** function and ship the trivial one:

   ```rust
   /// EASY-PATH resolver. Same signature the adjudicated path will use.
   fn resolve<'a>(cands: &'a [Artifact], verdicts: &[Verdict]) -> Option<&'a Artifact> {
       // 1. an explicit verdict wins  (never present in the easy path — yet)
       if let Some(v) = verdicts.latest_for(cands[0].element_id) {
           return cands.iter().find(|a| a.meta.id == v.winner);
       }
       // 2. a Manual correction beats any Parser output
       if let Some(m) = cands.iter().filter(|a| a.meta.origin.is_manual())
                              .max_by_key(|a| a.meta.generation) { return Some(m); }
       // 3. else the newest parser artifact for this (element_id, kind)
       cands.iter().max_by_key(|a| a.meta.generation)
   }
   ```
7. **Every consumer reads through `current_view`/`resolve`, never a raw parser
   artifact.** This is the one piece expensive to retrofit and free to do now: output
   assembly (sections + tables + captions) resolves each slot, so a future correction
   or adjudicated winner just *appears*.
8. **`verdicts.jsonl` exists and is consulted (even when empty).** So the day an
   adjudicator writes one, resolution already honors it.

### What graduation adds later — all additive, zero migration
- A richer `resolve` (the `Tiered` ladder: mechanical → LLM → human) — swap step 1's
  source, add tiers. Call sites unchanged.
- Adjudicators that **write** `verdicts.jsonl` (cross-tier, vision, human).
- New `Origin` variants; the `ManualEntry` extractor + `quarry submit` (writes `Manual`
  artifacts — already resolvable by rule #6 step 2).
- Cross-detector-version `element_id` matching by IoU (geometry already stored, #3).

None of these touch the extraction pipeline or rewrite stored data — that is the whole
point of append-only, and holding §5 is what keeps the door open.

## 6. The correction hook you asked for (it *is* §5, nothing extra)

A corrected HTML table = an appended `HtmlTable` with `Origin::Manual`, same
`element_id`. Rule #6 step 2 makes it win; provenance/audit are free; nothing mutated.
Cost to enable: the `Origin` field (#4) + the two-line Manual branch in `resolve`
(#6). That's the entire "leave space for correction" — the heavy adjudication
(multi-parser arbitration, confidence, LLM/human tiers, submit UI) stays unbuilt.

## 7. Build order

- **0.** **Claim-level probe (§0)** — answer real questions off the cheap parse, score
  vs source by failure class. *This decides how much of the rest to build.* Run first.
- **A.** `Region.role` enum; `Origin` on `Meta` (default `Parser`); `element_id` via
  the `register_or_match` seam — degenerate/always-mint for now (§5.3) + region bbox
  stored on the element so the match path can be enabled later without migration.
- **B.** `YoloLayout` op: render → detect → **pixel→point** → `Region{role}`.
- **B′.** `RegionCheck` (§1a) + a **region-quality eval pass on the corpus harness** —
  before trusting any boxes. **Pre-registered pass bar — two gating bars:** IoU-overlap
  between distinct table regions < 0.1; YOLO-vs-second-source boundary agreement
  (IoU ≥ 0.7) on ≥ 90% of regions. Below bar ⇒ fix layout before proceeding. **Plus one
  diagnostic (not a gate):** print the typed-orphan spans (words inside no `Table/Text/
  Caption` region) and eyeball them — they should be page furniture (headers, footers,
  page numbers), not table rows; a body-content orphan means YOLO missed a box. (Kept
  out of the gate deliberately: the page-text catch-all covers every span by
  construction, so a zero-orphan bar is vacuous, and legitimately un-regioned furniture
  makes a zero bar wrong anyway.) YOLO is the new dominant failure surface; do not skip
  this.
- **C.** Default table path: `LiteParseGrid` (region spans) → `GridToHtml` → `HtmlTable`.
  Also `TextExtract`, `PageText` (sections, captions, page text). The three cheap
  single-parse checks (`StructuralValidity`, `IntrinsicArithmetic`,
  `ReconstructionError`) attach here, **always-on** — they need no second parser (§0).
- **D′.** **Measure, then decide docling's tier (§1b):** re-run the cheap-vs-docling
  quality comparison with litparse fed a YOLO region + CropBox text layer.
  **Pre-registered rule:** docling is justified as **always-on** only if it beats
  litparse-fixed by **≥ 1.0 mean quality (0–5)** on **≥ 20%** of born-digital tables;
  otherwise **escalation-only** (fires on low cross-tier agreement or scanned/low-OCR
  docs). *Do not wire docling-always before this gate.*
- **C-docling.** `DoclingTable`: CropBox crop → docling → **crop→page** offset →
  `HtmlTable`, fired per D′'s verdict (escalation on low agreement / scanned, or always).
- **E.** Generalize `current_view` to `resolve(candidates, verdicts)` (trivial, verdict-aware);
  point all output assembly at it.
- **F.** Wire `cross_tier_agreement(html, grid)` per table as the confidence signal — it
  is *also* the escalation trigger from §1b. **This is the one detector §0 gates**
  (it needs a second parse; it is *doubly* gated — it only has two parses to compare
  because escalation already fired). Deploy only if the probe shows born-digital value
  errors; otherwise it stays OCR/escalation-only.
- **G.** Structured text (§1c), building on C's `Region{Text}` and E's resolve seam:
  `Paragraphs` (line y-cluster → paragraph) → `Headings` (font-style runs → levels) →
  `ReadingOrder` (XY-cut/column — **pin like §3**, multi-column is where it breaks) →
  `DocOutline` (Merge → `StructuredDoc`, refs tables/captions by `element_id`). Cheap
  structural diagnostics only (well-formed hierarchy, monotonic order, one paragraph per
  span); furniture dropped via cross-page repetition (§1a). No heavy verification unless
  the probe's text-structure question shows structure errors cause wrong answers.

A establishes the forward-compat substrate; B′ checks the stage the data says matters
most; D′ is the gate that decides whether the expensive parser is even justified; E is
the seam that prevents precluding the full path; F does double duty (confidence + the
escalation trigger); G assembles structured text on the same substrate. **B′ and D′ are
measurement gates, not optional polish** — they are the corrections the
proponent/contrarian review forced: *check YOLO, earn docling.*

## 8. Risks / watch-items
- **YOLO region errors are the new dominant failure surface**, and the content
  cross-check runs *inside* the box so it can't see a bad box — addressed head-on by
  the §1a `RegionCheck` and the **B′** region-quality eval gate, not left to chance.
- **docling may be unjustified on born-digital** once litparse has a good region + text
  layer — resolved by the **D′** measurement gate before any always-on commitment.
- **`element_id` across YOLO versions (§5.3):** until the `register_or_match` match
  path is enabled, re-ingesting a doc under a *new* YOLO model mints fresh ids (new
  bbox → new mint) and orphans prior corrections. The fix is additive, **not** a
  migration: bbox is stored, so enabling IoU matching reuses ids going forward and
  back-fills `Same` aliases over the append-only log — no stored id changes. Don't
  re-ingest corrected docs under a new model until matching is on. The seam to watch.
- **docling throughput** even on crops, at ingest scale (bounded now by escalation, per D′).
- **CropBox honored by docling** — verify; image-fallback means OCR.
- **Caption→figure association** is a nearest-neighbor heuristic.
- **Reading order (§1c, step G)** is the silent failure for structured text — multi-column
  pages interleave wrongly and the body text scrambles with no obvious error. Pin the
  XY-cut/column ordering like the coordinate maps in §3, and make "reading order
  monotonic" a structural diagnostic.
- **Thin evidence base** — this plan rests on N≈13 hand-labeled tables in one domain.
  **Step 0 (§0) is the mitigation**: it widens the corpus and measures at the claim level
  before any expensive commitment, and can shrink the plan to `YOLO + litparse` if
  structural noise turns out not to corrupt answers.
- **Over-building risk** — the two-parser verification edifice targets a silent
  value-error class not yet observed on born-digital. §0 is the check against architecting
  for a ghost; if it fires "shrink," treat §1–§7 as an upper bound, not a spec.
