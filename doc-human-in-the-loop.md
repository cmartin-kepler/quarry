# Draft impl note: human-in-the-loop (LLM/human adjudicators + correction PRs)

Companion to `doc-parsing-design-brief.md` / `doc-parsing-implementation-plan.md`.
Premise: **neither needs new architecture.** A human/LLM judge is another
`Adjudicator` impl; a manual correction is a top-trust-tier `Extractor` output —
both ride the append-only DAG, the cell contract, and the verdict log we already
have. "Current for region R" stays a query (best-adjudicated branch), so a
correction wins without mutating or invalidating anything.

## 1. Tiered adjudication (mechanical → LLM → human)

`Adjudicator` already returns a `Verdict{Winner|Equivalent|Ambiguous}`. Run judges
**lazily**: the cheap mechanical one resolves the easy cases; only `Ambiguous`
escalates to a costlier judge. Each verdict is an append-only `AdjudicationRecord`
(inputs, checks, verdict, confidence, rationale, **judge id**).

```rust
/// Escalate only the cases the cheaper judge couldn't settle.
pub struct Tiered { pub judges: Vec<Box<dyn Adjudicator>> } // cheap → expensive

impl Adjudicator for Tiered {
    fn adjudicate(&self, candidates: &[&dyn Artifact], checks: &[CheckOutcome]) -> AdjudicationRecord {
        let mut rec = self.judges[0].adjudicate(candidates, checks);
        for j in &self.judges[1..] {
            if !matches!(rec.verdict, Verdict::Ambiguous(_)) { break; }
            rec = j.adjudicate(candidates, checks); // e.g. LLM, then human
        }
        rec
    }
}
```

- **LlmAdjudicator** — hand the candidate tables + evidence + the cropped source
  bbox to a vision model ("which is right, and does the crop support it?"). This is
  the brief's claim-time `AnswerSupport`, promoted to adjudication time. Run it as a
  sidecar (the §C command pattern); **check the judge** (its verdict is itself
  vision-verifiable) and record the rationale.
- **HumanAdjudicator** — surface the candidates to a person; their pick is a verdict
  with `judge = "human:alice"`. An override that disagrees with the mechanical
  default is a training signal *against* the adjudicator (brief §4).

Nothing is mutated: judges only *append* a preference with provenance.

## 2. A correction is a top-tier branch (the "PR")

A manual fix is just another artifact derived from the region — its "parser" is a
person. It rides the **existing cell contract** (`sidecar::tables_from_json`): a
person is the highest-trust parser submitting `{tables:[{page,bbox,cells}]}`.

```rust
/// `quarry submit <region.json> --author alice` → a manual HtmlTable branch.
pub struct ManualEntry { pub author: String }

const MANUAL_ACCEPTS: [InputKind; 1] = [InputKind::DocumentRegion];
impl Extractor for ManualEntry {
    fn id(&self) -> ExtractorId { ExtractorId(format!("manual:{}", self.author)) }
    fn cost_tier(&self) -> CostTier { CostTier(u8::MAX) }   // top: most trusted
    fn op_kind(&self) -> OpKind { OpKind::Extract }
    fn accepts(&self) -> &[InputKind] { &MANUAL_ACCEPTS }
    fn produces(&self) -> ArtifactKind { ArtifactKind::HtmlTable }
    // extract: read the submitted cells JSON → tables_from_json(...) → HtmlTable,
    //          tagged with this author (see Origin below). Provenance::Source(anchor).
    // ...
}
```

The Git mapping is exact:

| Git | here |
|---|---|
| commit on a branch | the submitted corrected element |
| open a PR | submit a candidate correction (a branch under region R) |
| CI on the PR | run the `QualityCheck`s on it |
| reviewer approves | the (human/LLM) adjudicator marks it `Winner` |
| merge → `main` moves | `current_view` for R resolves to the correction |
| `git blame` | provenance: who corrected what, when, citable to source |

## 3. The one small new field: `Origin`

Today an artifact knows its `Provenance` (where in the bytes) but not *who/what made
it*. Add an `Origin` to `Meta` so corrections and judges are attributable and the
trust-aware adjudicator can weight by source.

```rust
pub enum Origin {
    Parser { extractor: ExtractorId, version: Version }, // pdf-text, docling, …
    Manual { author: String },                           // a correction PR
}
// Meta { …, origin: Origin }
```

A **trust-aware adjudicator** then prefers `Manual` over parser branches, then breaks
ties by checks — so an approved correction is current; the parser branches remain as
history (no staleness).

## 4. Why this closes the loop
One correction does three jobs at once: it's the **better current answer**, it's
**hand-labeled ground truth** for the `eval` catch-rate harness (brief §2's
highest-value input), and where it overrules the mechanical adjudicator it's a
**calibration signal** for it.

## 5. Tensions (all Git-shaped)
- **Trust / gaming.** A submitted "correct" table is *untrusted input* until reviewed
  — it's a *candidate* branch, not truth, until an adjudicator approves it; `Origin`
  + provenance make every assertion attributable (brief: documents are untrusted).
- **Judge cost / reliability.** LLM judges run only on ambiguity and are themselves
  checkable (vision-verify the crop); never let a judge move `current` without a
  recorded, checkable rationale.
- **Approval authority.** "Who can approve" is policy in the `Tiered` ladder (any
  human override wins / only a reviewer's does / LLM may approve low-stakes only) —
  configurable and append-only-logged.

## 6. Smallest first step
1. add `Origin` to `Meta` (+ thread it through the extractors; default
   `Parser{id,version}`),
2. `ManualEntry` extractor + a `quarry submit` CLI reusing `tables_from_json`,
3. a `TrustAdjudicator` that orders by `Origin` then checks,
4. (later) `LlmAdjudicator`/`HumanAdjudicator` as `Tiered` rungs.

Steps 1–3 are pure-Rust and stub-testable; step 4 is a sidecar + a UI affordance.
