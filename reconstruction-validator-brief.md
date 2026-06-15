# Implementation Brief: Reconstruction-Error Validator for Table Parses

A spec for a coding agent. Build a **label-free validator** that scores whether a
hypothesized HTML table faithfully represents the source PDF region it claims to,
by checking that the hypothesis can *reconstruct the observed glyph layout*. High
reconstruction error = likely silent parse failure → flag / escalate. No ground
truth required.

## Why this works (the one idea)

HTML→PDF rendering is (per renderer) deterministic; PDF→HTML is not. So instead of
trusting a parser's HTML, we treat extraction as an inverse problem and **verify by
synthesis**: a correct HTML table, taken together with the observed glyph
positions, explains where every glyph landed. A wrong one doesn't. A transposed
column, a dropped merge, a shifted header all produce HTML that *cannot* account
for the observed token layout — so they fail reconstruction even when the text
"looks clean." That failure signal is exactly the silent-error detector we
otherwise lack.

This is the regularized-inverse view: `argmin_x [ distance(render(x), observed) +
λ·complexity(x) ]`. **This brief implements only the `distance` term as a single-
hypothesis validator.** The `complexity`/MDL term (for *choosing among* candidate
parses) is a later extension, noted at the end — do not build it yet.

## Core principles (hold these; they make it robust)

1. **Compare in relational space, not absolute coordinates.** Do NOT render the
   HTML and pixel-diff. Font substitution and subpixel drift between the source
   renderer and yours would swamp the signal. Compare *structure*: which tokens
   share a row band, which share a column band, alignment, and reading order.
   No headless browser is needed or wanted in the default path.
2. **Born-digital only.** The observation is the PDF content stream (glyphs +
   positions), which lives in the same space as the hypothesis. If the region has
   no text layer (scanned), the validator returns `NotApplicable` — it must NOT
   silently pass. Scanned pages are the OCR tier's problem.
3. **Output diagnostics, not just a score.** The scalar drives escalation; the
   structured disagreements (which tokens, suspected transposition/merge) feed the
   adjudicator and the eval corpus.

## Algorithm

Input: `(pdf_path, page, bbox)` for the source region, and `html` (the parser's
hypothesized table for that region).

**1. Observe.** Extract tokens in the region with a born-digital extractor
(pdfplumber `extract_words`, or pymupdf/pdfium). Each observed token:
`{text, x0, x1, y0, y1}`. If zero tokens with a text layer → return `NotApplicable`.

**2. Band the observation.** Cluster observed tokens into **row bands** (cluster on
y-centers) and **column bands** (cluster on x-positions; detect per-column
alignment — left/right/decimal — since right-aligned numeric columns cluster on
x1, not x0). Each observed token now has `(obs_row, obs_col)`.

**3. Lay out the hypothesis as a logical grid.** Parse `html` into a cell matrix,
expanding `rowspan`/`colspan`, so each cell has `(hyp_row, hyp_col)` and its text.
Tokenize cell text into hypothesis tokens.

**4. Align hypothesis tokens to observed tokens.** Match by normalized text;
disambiguate duplicates by reading order (top-to-bottom, left-to-right sequence
alignment, diff-style). Produces:
   - matched pairs,
   - `missing` = observed tokens with no hypothesis match (dropped content),
   - `spurious` = hypothesis tokens with no observed match (hallucinated content).

**5. Score structural consistency over matched pairs.** A faithful hypothesis
induces a clean mapping between its grid and the observed bands:
   - **Row consistency:** tokens sharing a `hyp_row` should share one `obs_row`
     band. Count rows whose tokens straddle multiple observed bands.
   - **Column consistency:** tokens sharing a `hyp_col` should share one `obs_col`
     band. Count column violations.
   - **Permutation check (transposition detector):** build the
     `hyp_col → dominant obs_col` mapping. Identity = aligned; a non-trivial
     permutation localizes a transposition (which columns were swapped).
   - **Merge/split signal:** one observed band mapping to two hyp cells (or vice
     versa) flags a merge/split error at that location.

**6. Aggregate.** Return a reconstruction error in `[0, 1]` combining coverage
(`1 - missing/observed`), spurious rate, and the row/column violation rates, plus
the full diagnostic record.

## Interface (prototype in Python; port later — see Integration)

```python
from dataclasses import dataclass, field

@dataclass
class Diagnostics:
    coverage: float                 # fraction of observed tokens explained
    spurious_rate: float            # hypothesis tokens with no observed match
    row_violations: float           # fraction of hyp rows spanning >1 obs band
    col_violations: float
    column_permutation: list[int] | None   # non-identity => transposition; localizes it
    merge_split_sites: list[tuple]  # (region, kind) for suspected merge/split errors
    missing_tokens: list[str]
    spurious_tokens: list[str]

@dataclass
class ReconResult:
    status: str                     # "ok" | "not_applicable"
    error: float                    # [0,1]; higher = worse. None if not_applicable
    diagnostics: Diagnostics | None

def validate_table(pdf_path: str, page: int, bbox: tuple[float, float, float, float],
                   html: str) -> ReconResult:
    ...
```

A scalar threshold (`error > τ`) drives flag/escalate; ship the diagnostics
regardless so the adjudicator can act on *why* it failed.

## Self-test FIRST — how you know the validator actually works (no ground truth)

Before trusting it on real parses, prove its sensitivity by **injecting known
corruptions into correct HTML** and confirming the error spikes and localizes.
This is the validator's analog of the Phase-0 catch-rate experiment and needs no
labels:

1. Take HTML tables you believe are correct (e.g. from `piushorn/pdf-parse-bench`,
   which ships PDFs + ground truth) and their source PDF regions.
2. Generate corrupted variants: swap two columns, drop a cell, merge two adjacent
   cells, shift a header row, transpose a 2×2 block.
3. Assert: `validate_table` on the clean HTML returns low error; each corruption
   returns high error, and the diagnostic *localizes the right corruption*
   (transposition → correct `column_permutation`; dropped cell → that token in
   `missing`; etc.).

Report the validator's **true-positive rate** (corruptions caught) and
**false-positive rate** (clean tables wrongly flagged) across a sweep of `τ`.
That curve is the deliverable that says whether reconstruction error is a usable
validator. Tune `τ` from it.

## CLI (run it over the corpus)

```
recon-validate <pdf> --page N --bbox x0,y0,x1,y1 --html parse.html
recon-validate --corpus ./corpus --parses ./parses   # batch; emits error per region
recon-validate selftest --pdf clean.pdf --html clean.html   # runs the injection sweep
```

Batch mode emits a CSV of `(doc, page, region, error, top_diagnostic)` so you can
sort by error and eyeball the worst — your first look at real silent failures.

## Integration as a `QualityCheck` (only after the self-test passes)

Slots into the trait system from the design brief. Prototype in Python for speed
(pdfplumber); once the catch-rate curve justifies it, port or wrap behind the
trait. Decide: pure-Rust (pdfium/lopdf for tokens) vs. a Python sidecar — Rust
shop, but pdfplumber's word extraction is the fast path to a working prototype.

```rust
// CheckOutcome::Flag carries the reconstruction error + localized diagnostic,
// which the adjudicator uses to decide escalate-vs-surface-ambiguity.
impl QualityCheck for ReconstructionCheck {
    fn applies_to(&self, kind: ArtifactKind) -> bool { kind == ArtifactKind::HtmlTable }
    fn check(&self, artifact: &dyn Artifact, ctx: &CheckCtx) -> CheckOutcome {
        // resolve artifact -> source (pdf,page,bbox) via the element registry,
        // run reconstruction, map error>τ to Flag with diagnostics.
    }
}
```

## Scope guard (do NOT build these yet)

- **MDL / candidate selection.** This validator scores *one* hypothesis. Choosing
  the *best* among several parses by `distance + λ·complexity` is the adjudicator's
  job and a separate task. Reconstruction error is the `distance` input it will
  eventually consume; the complexity term and the calibrated operator-cost prior
  are future work.
- **Rendering / rasterization.** Default path is renderer-free relational matching.
  An optional high-fidelity render-and-compare mode can come later; it reintroduces
  the renderer-mismatch problem and a browser dependency, so it is not the default.
- **Scanned/OCR regions.** Out of scope; return `NotApplicable`.

## Definition of done

`recon-validate selftest` shows clean tables scoring low and each injected
corruption scoring high with a correctly localized diagnostic, summarized as a
TP/FP curve over `τ`; and batch mode over the corpus produces a sorted error CSV.
That curve — not a passing unit test — is what tells you the idea holds.
