# Input corpus

Drop source documents here (PDF first; XLSX/PPTX once those extractors land),
then run the pipeline across them:

```bash
uv sync                              # one-time: Python env for scripts/
cargo build                          # one-time: build the quarry binary

uv run scripts/run_corpus.py         # defaults to this dir -> corpus/input/
# or point elsewhere / tune limits:
uv run scripts/run_corpus.py input --out corpus/input --max-pages 50 --max-mb 40
```

Subdirectories are fine — `run_corpus.py` recurses. For each PDF it bridges to
`.qdoc` (pdfplumber), runs `quarry parse` + the detectors, and prints a per-doc
summary (pages, spans, regions, tables reconstructed, tables flagged). Outputs
(`.qdoc`, parsed artifacts) land under `corpus/input/` (gitignored).

## What's tracked vs ignored

The documents you add here are **gitignored** (large binaries) — only this
README and `.gitkeep` are tracked, so the folder exists in a fresh clone. See the
root `.gitignore`.

## No ground truth = no catch rate

`run_corpus.py` exercises the pipeline and reports which reconstructed tables the
detectors flag as suspicious. It does **not** compute a silent-failure catch rate
— that needs hand-labeled truth. To get a catch rate for a doc:

1. `uv run scripts/pdf_to_qdoc.py input/your.pdf -o corpus/input/your.qdoc`
2. Hand-label the worst few tables into `your.cells.json` (see `scripts/README.md`).
3. `uv run scripts/build_truth.py --cells your.cells.json --qdoc corpus/input/your.qdoc -o your.truth.json`
4. `cargo run -- eval corpus/input/your.qdoc --truth your.truth.json --detail`

## No-text-layer documents

Some PDFs (scanned, or decks exported with text outlined to vector paths) have no
text layer — the bridge extracts 0 spans and `run_corpus.py` reports
`no-text-layer`. Those need an OCR/VLM tier (deferred in the design brief); the
born-digital pipeline can't read them.
