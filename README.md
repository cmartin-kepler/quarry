# Quarry

An example **Phase-0** implementation of the lazy/iterative document-parsing
system in [`doc-parsing-design-brief.md`](./doc-parsing-design-brief.md).

Phase 0 exists to attack the brief's riskiest assumption before building any
architecture around it:

> **Silent parse failures are detectable by something other than the agent.**

So the headline deliverable is not a parser — it's the `eval` harness that
measures the **silent-failure catch rate**: of the extractions that are actually
wrong, what fraction did at least one mechanical detector flag?

## Quick start

```bash
cargo build
cargo test

# THE point of the build — measure catch rate vs hand-labeled truth:
cargo run -- eval fixtures/filing.qdoc --truth fixtures/filing.truth.json

# Supporting commands:
cargo run -- inspect fixtures/filing.qdoc            # dump structure & anchors
cargo run -- parse   fixtures/filing.qdoc --out /tmp/out --tier 0
cargo run -- check   /tmp/out                         # run quality checks
```

Example `eval` output:

```
table                      matched  wrong   flagged_by
income_statement_p1        yes      ok      -
balance_sheet_p2           yes      WRONG   structural_validity,answer_support

=== silent-failure catch rate ===
wrong extractions: 1 / 2
CATCH RATE: 100%  (1 of 1 wrong extractions flagged by >=1 detector)
false-alarm rate on correct tables: 0%
per-detector (of the 1 wrong, how many each caught):
  intrinsic_arithmetic   0
  structural_validity    1
  answer_support         1
```

## Testing against real PDFs

The `.qdoc` files above are hand-authored fixtures. To run the parser against
**actual born-digital PDFs** (real text layer, real coordinates), use the corpus
tooling in [`scripts/`](./scripts/):

```bash
pip install pdfplumber reportlab

# Generate a born-digital PDF (tables + a bar-chart figure) with known truth...
scripts/latex/build.sh                      # via LaTeX (needs a TeX install), OR
python3 scripts/gen_synthetic_pdf.py --out corpus/synthetic   # via reportlab (no TeX)

# ...bridge it into .qdoc and eval:
python3 scripts/pdf_to_qdoc.py corpus/synthetic.pdf -o corpus/synthetic.qdoc
python3 scripts/build_truth.py --cells corpus/synthetic.cells.json \
        --qdoc corpus/synthetic.qdoc -o corpus/synthetic.truth.json
cargo run -- eval corpus/synthetic.qdoc --truth corpus/synthetic.truth.json
```

`scripts/pdf_to_qdoc.py` (pdfplumber) is the bridge from any PDF to `.qdoc`; the
same path ingests real SEC EDGAR filings via `scripts/fetch_edgar.py`. Running
this surfaced a real gap the toy fixtures hid — PDF extraction yields *words*, not
cells, so the reconstructor now groups words into cell-blocks by horizontal gap
before clustering columns (`src/extract.rs`). On the generated filing the cheap
parser reconstructs a uniform-width table correctly (no false alarm) but splits a
table of mixed-width right-aligned numbers — caught by the detectors.

## What's implemented (brief §6 Phase 0)

| Piece | Where | Notes |
|---|---|---|
| Object-safe `Artifact` core + `Text`/`HtmlTable` | `src/artifact.rs` | hybrid payload strategy (see below) |
| Cheap PDF extractor | `src/extract.rs` | naive geometric table reconstruction |
| `IntrinsicArithmetic` detector | `src/check.rs` | rows-sum-to-total reconciliation (~free) |
| `StructuralValidity` detector | `src/check.rs` | ragged columns, empty/merged cells, OCR conf |
| `AnswerSupport` (claim-time) | `src/check.rs` | crops cited bbox, verifies the claim |
| `Adjudicator` + append-only verdicts | `src/adjudicate.rs` | default-at-parse, surface only ambiguity |
| Flat store + single current-view fn | `src/store.rs` | `current_artifacts()` is the one access point |
| `eval` catch-rate harness | `src/eval.rs` | structural diff vs ground truth, per-detector |
| `parse`/`check`/`eval`/`inspect` CLIs | `src/main.rs` | brief §5 |

### Deliberately deferred (brief §6)

Element-identity matching across re-parses, the full derivation DAG + staleness
propagation, the append-only registry/current-view machinery, PPTX/XLSX
extractors, the async job queue, and the agents + semantic indexes themselves.
Higher cost tiers and non-PDF formats return clear "not built yet" errors rather
than silently doing the wrong thing.

## Design decisions resolved from the brief

The brief flags several tensions to "resolve with the user, not unilaterally."
The defaults chosen here (each is the brief's own recommendation and is cheap to
revisit):

1. **Payload strategy → hybrid.** Object-safe `Artifact` trait carries shared
   metadata/provenance/risk; an `ArtifactKind` enum drives matching; `as_any()`
   downcasts to the concrete payload. Open set, but ergonomic dispatch.
2. **`AnswerSupport` → its own claim-time trait**, not a `QualityCheck`. It takes
   a *claim* + the cited region, a different signature from a parse-time check.
3. **Store boundary → flat JSON, fronted by one current-view function.** Phase 0
   defers the append-only registry, but the day-one rule holds: "current state"
   is reached through exactly one call site, so swapping in the
   `DISTINCT ON (element_id) ORDER BY generation DESC` query later is local.

## The `.qdoc` fixture format (stand-in for original bytes)

A real build parses PDF/PPTX/XLSX behind the `Extractor` trait. For a runnable,
testable example, `.qdoc` is a JSON text-layer: pages of positioned spans
(`text`, `bbox`, optional `confidence`/`rotated`) plus marked `table_regions`.

This is not a toy shortcut around the hard part — it's the opposite. The cheap
extractor reconstructs tables **geometrically** (cluster spans into rows by
y-center, into columns by a global x0 model). That naive global column model is
genuinely defeated by right-aligned numbers of varying width, exactly as a real
cheap parser is — so it produces *clean-looking-but-wrong* tables. See
`fixtures/filing.qdoc` page 2: the value `5` is pulled into a phantom column,
yielding a full, rectangular, plausible HTML table that is wrong. That is the
silent failure the whole experiment is about, and the detectors catch it.

Each reconstructed cell is anchored to its **grid-band rectangle** (not the
source glyph box), so a misassigned span lands outside its cell's cited crop and
`AnswerSupport` can detect the misalignment.

## What the example demonstrates about the riskiest assumption

On the bundled hard fixture, the positional misassignment shifted only a small
value, so the column total stayed within `IntrinsicArithmetic`'s 1% tolerance and
that detector stayed silent — while `StructuralValidity` and `AnswerSupport`
both caught it. The takeaway matches the brief's framing: *which* detector
carries the weight depends on the failure mode, and arithmetic alone is not
sufficient. (The integration tests in `tests/detectors.rs` show each detector
firing on the failure mode it owns, including a genuine arithmetic break.)

To run the real experiment from the brief, replace the fixtures with 20–30 hard,
hand-labeled filings and read the catch-rate breakdown.
