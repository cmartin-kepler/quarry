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
cargo run -- eval fixtures/filing.qdoc --truth fixtures/filing.truth.json --detail  # full breakdown

# Supporting commands:
cargo run -- inspect fixtures/filing.qdoc            # dump structure & anchors
cargo run -- parse   fixtures/filing.qdoc --out /tmp/out --tier 0
cargo run -- check   /tmp/out                         # run quality checks
```

Example `eval` output (summary):

```
table                      matched  wrong   iou    flagged_by
income_statement_p1        yes      ok      1.00   -
balance_sheet_p2           yes      WRONG   1.00   structural_validity,answer_support

=== silent-failure catch rate ===
wrong extractions: 1 / 2
CATCH RATE: 100%  (1 of 1 wrong extractions flagged by >=1 detector)
false-alarm rate on correct tables: 0%
per-detector (of the 1 wrong, how many each caught):
  intrinsic_arithmetic   0
  structural_validity    1
  answer_support         1

MISSED (wrong but unflagged): none
```

### `--detail`: what was reconstructed, where it failed, and how we know

`--detail` prints, per table, the reconstructed grid beside the ground truth,
every cell-level divergence, the parse-time risk markers, and each detector's
verdict **with the evidence it acted on**. Excerpt for a table whose right-aligned
columns split into phantom columns:

```
▌ income_statement   [difficulty: right-aligned-varying-width]   WRONG
  matched art_48f0… at anchor IoU 1.00;  reconstructed 6x5, truth 6x3

  RECONSTRUCTED (what the cheap parser produced):
    │ Line item       │ FY2024 │       │ FY2023 │       │
    │ Product revenue │        │ 1,200 │        │ 1,000 │
    ...
  GROUND TRUTH (hand-labeled):
    │ Line item       │ FY2024 │ FY2023 │
    │ Product revenue │ 1,200  │ 1,000  │
    ...
  diff: 18 divergence(s)
    dimensions: got 6x5, want 6x3
    [1,1] got "" want "1,200"
    [1,2] got "1,200" want "1,000"
    ...
  parse-time risk markers: col_count_variance 0.00, merged_rows 6, empty_cells 12, min_ocr_conf 1.00

  detectors (the evidence — how we know):
    intrinsic_arithmetic   pass        pass (confidence 0.95)
    structural_validity    FLAG/ERROR  6 row(s) with missing cells; 12 empty cell(s)
    answer_support         pass        15 sampled cell(s) all present in their cited crops

  VERDICT: WRONG, caught by [structural_validity]
```

This case shows the detectors are complementary: the column split is a structural
artifact, so `structural_validity` fires while `answer_support` honestly passes
(each cell's text still sits inside its own band — the failure is an index shift,
not a value in the wrong place). The `MISSED` section at the end of every run
lists wrong extractions that slipped past *all* detectors — the silent failures
the whole experiment exists to surface.

## Testing against real PDFs

The `.qdoc` files above are hand-authored fixtures. To run the parser against
**actual born-digital PDFs** (real text layer, real coordinates), use the corpus
tooling in [`scripts/`](./scripts/):

```bash
uv sync                                     # one-time: Python env for scripts/ (uv-managed)

# Generate a born-digital PDF (tables + a bar-chart figure) with known truth...
scripts/latex/build.sh                      # via LaTeX (needs a TeX install), OR
uv run scripts/gen_synthetic_pdf.py --out corpus/synthetic   # via reportlab (no TeX)

# ...bridge it into .qdoc and eval:
uv run scripts/pdf_to_qdoc.py corpus/synthetic.pdf -o corpus/synthetic.qdoc
uv run scripts/build_truth.py --cells corpus/synthetic.cells.json \
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

## Using a real parser (Docling / Reducto / LlamaParse)

`.qdoc` and the cheap reconstructor are the *low-end* path. A real
table-producing parser already does layout + table-structure recognition and
emits tables with cells and bounding boxes — it is the parser **and** the
reconstructor — so it **bypasses `.qdoc` entirely**. The only glue is a thin
adapter onto the `Artifact` / `SourceAnchor` model; the detector / adjudicator /
eval core then runs unchanged. That core — not the cheap parser — is the
reusable, differentiated part of Quarry.

`src/docling.rs` is a reference adapter for [Docling](https://github.com/docling-project/docling)
(open-source, no API key). It maps Docling JSON tables → `HtmlTable` artifacts
(per-cell anchors, handling Docling's BOTTOMLEFT bbox origin):

```bash
# Docling is heavy (torch + models); run on demand, not a project dep:
uv run --with docling scripts/docling_to_json.py input/foo.pdf -o corpus/foo.docling.json
cargo run -- import-docling corpus/foo.docling.json --pdf input/foo.pdf --out corpus/foo.docling.artifacts
cargo run -- check corpus/foo.docling.artifacts        # detectors run on Docling's tables
```

Validated on real Docling output: a multi-level-header cash-flow table is
preserved faithfully with anchors, and the detectors run on it. Notably this
shifts the bottleneck — on a *clean* Docling parse, `intrinsic_arithmetic` still
misfires on multi-section tables and `structural_validity` flags legitimately
sparse hierarchical headers as "empty cells." With the parser no longer the weak
link, the detectors' table-semantics understanding becomes the thing to improve —
exactly the kind of finding this harness exists to produce.

To add Reducto / LlamaParse: write the analogous `their JSON → Vec<Box<dyn
Artifact>>` adapter; nothing downstream changes.

## What's implemented (brief §6 Phase 0)

| Piece | Where | Notes |
|---|---|---|
| Object-safe `Artifact` core + `Text`/`HtmlTable` | `src/artifact.rs` | hybrid payload strategy (see below) |
| `PdfTextLayerReconstructor` | `src/extract.rs` | naive geometric table reconstruction from a `.qdoc` text layer (NOT a PDF-byte parser — that's `scripts/pdf_to_qdoc.py`) |
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
