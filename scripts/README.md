# Corpus tooling

Scripts to assemble a real/synthetic document corpus and feed it to the Quarry
eval harness. Two halves:

- **Get real documents** — `fetch_edgar.py`, `fetch_benchmarks.py`.
- **Generate born-digital PDFs with known ground truth** — `latex/` (preferred)
  or `gen_synthetic_pdf.py` (no-TeX fallback), then bridge them into `.qdoc` with
  `pdf_to_qdoc.py` and assemble truth with `build_truth.py`.

## TL;DR — generate a PDF and measure catch rate

```bash
pip install pdfplumber reportlab          # bridge + no-TeX generator

# Option A: LaTeX (needs a TeX install; MacTeX/TeX Live or `brew install tectonic`)
scripts/latex/build.sh                    # tex -> pdf -> qdoc -> truth -> quarry eval

# Option B: no TeX — reportlab generates an equivalent born-digital PDF
python3 scripts/gen_synthetic_pdf.py --out corpus/synthetic
python3 scripts/pdf_to_qdoc.py corpus/synthetic.pdf -o corpus/synthetic.qdoc
python3 scripts/build_truth.py --cells corpus/synthetic.cells.json \
        --qdoc corpus/synthetic.qdoc -o corpus/synthetic.truth.json
cargo run -- eval corpus/synthetic.qdoc --truth corpus/synthetic.truth.json
```

Both produce a real born-digital PDF (true text layer, real PDF coordinates) with
right-aligned numeric columns — the layout that defeats a cheap parser's global
column model and yields realistic *clean-looking-but-wrong* tables.

## The PDF → `.qdoc` bridge — `pdf_to_qdoc.py`

Quarry's extractors consume `.qdoc` (positioned spans + table regions). This
bridge produces that from any PDF via pdfplumber, so **real EDGAR PDFs and
generated PDFs both flow through the same pipeline**. Word boxes come out in
points with a top-left origin — exactly the `.qdoc` convention, no conversion.
Table regions default to pdfplumber's ruled-line detection (override with
`--regions`, or `--no-detect` for spans only). The region only *scopes* which
spans the cheap extractor reconstructs — the naive reconstruction and its failure
modes stay in Quarry, so the eval still measures something real.

## Ground truth — `build_truth.py`

A generator emits the logical table values in document order (`*.cells.json`);
the bridge emits detected regions (`*.qdoc`). This pairs them positionally
(i-th known table ↔ i-th detected region, page then top order) and writes
`*.truth.json` with each table's cells + its `(page, PDF-bbox)` anchor — the
anchor format the brief asks for. It **warns** if region and table counts differ
(e.g. a chart frame detected as a table) rather than silently truncating; for the
sample doc the figure is last, so order-pairing drops the spurious region cleanly.

## `fetch_edgar.py` — real born-digital financial docs from SEC EDGAR

## `fetch_edgar.py` — real born-digital financial docs from SEC EDGAR

```bash
export SEC_USER_AGENT="Kepler AI corpus-builder you@kepler.ai"   # REQUIRED by SEC
python3 fetch_edgar.py --out ./corpus --per-company 1
```

What you get, by format (this is just what EDGAR exposes):

| Format | Source | Notes |
|---|---|---|
| **PDF** | 8-K exhibits (EX-99.x) | Investor/earnings **decks**, frequently born-digital PDFs. Your best free real-PDF source. |
| **XLSX** | `Financial_Report.xlsx` on 10-K/10-Q | XBRL-derived, multi-sheet. Stresses the XLSX *interpretation* tier. |
| **HTML** | 10-K/10-Q primary doc | Densest footnoted multi-level tables, but **not PDF**. Good HTML-table fodder. |

Every file is logged in `corpus/manifest.csv` with its exact `source_url`, so each
local file already carries provenance into your labeling.

Useful flags: `--forms 8-K 10-K 10-Q`, `--decks-only` (PDFs only),
`--ticker AAPL --ticker JPM` (add issuers by ticker), `--per-company N`.

SEC etiquette: a contact-bearing `User-Agent` is mandatory (the script refuses to
run without one) and it self-throttles under SEC's ~10 req/s limit.

> **Reality check (observed 2026-06):** the curated large-cap issuers
> (JPM, BofA, Apple, MSFT, NVIDIA, …) now file 8-K earnings exhibits as
> **HTML/iXBRL, not PDF** — `--decks-only` returns nothing for them. Their 10-Q
> primary docs *are* available (dense footnoted HTML tables, ~1–9 MB each) and
> were fetched successfully. For real *PDF* decks you'll need smaller issuers
> that still post PDF press releases (find via EDGAR full-text search), or just
> use the LaTeX/reportlab generators below, which give born-digital PDFs **with**
> ground truth.

## `fetch_benchmarks.py` — pre-labeled benchmarks from HuggingFace

```bash
pip install huggingface_hub
python3 fetch_benchmarks.py --out ./corpus/benchmarks
```

- **`piushorn/pdf-parse-bench`** — ~synthetic PDFs from LaTeX with automatic ground
  truth. The rare source of *actual born-digital PDFs* (real text layer, true PDF
  coords) + free labels. Clean-path smoke test; not real-world-messy.
- **`llamaindex/ParseBench`** — ~2,000 human-verified enterprise pages (insurance,
  finance, government), 5 dimensions, Apache-2.0. Almost certainly page **images +
  rules**, not source PDFs — exercises the VLM/OCR tier, not your born-digital path.

After each download the script prints a file-type inventory and a one-line verdict
(`source PDFs present` vs `page images only`) so you can see immediately what you got.
Restrict large pulls with `--allow '*.pdf' '*.json'`.

## What still needs hand-labeling (and why)

No public benchmark exercises **born-digital text-layer extraction with real PDF
anchors** — your highest-volume production path. So self-label a small set:

- Pull 8-K deck PDFs + a few 10-K HTMLs via `fetch_edgar.py`.
- Label **only the worst few tables per doc**, not whole documents. Capture:
  (1) correct cell values/structure (HTML or CSV),
  (2) the **source anchor** as `(page, PDF-coordinate bbox)` — *not* rasterized
      pixel bbox, so the harness tests your real anchor path,
  (3) a difficulty tag (merged-cells / hierarchical-header / footnote-numeric /
      rotated / multi-table-page / restated).
- Reuse OmniDocBench's JSON schema (`layout_dets` with `poly`, `html`/`latex` per
  table) so the harness speaks one ground-truth format for borrowed and homegrown
  truth alike.

~10 hard docs is enough for the Phase 0 detection experiment; grow to 30–50 across
all three formats once the catch-rate number says the approach holds.

## Network note

`fetch_edgar.py` was run successfully against live SEC EDGAR (it needs the
`requests` package and a contact-bearing `SEC_USER_AGENT`). `fetch_benchmarks.py`
needs `huggingface_hub`. In a restricted network, allow `data.sec.gov`,
`www.sec.gov`, and `huggingface.co`.
