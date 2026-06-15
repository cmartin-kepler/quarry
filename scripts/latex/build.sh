#!/usr/bin/env bash
# Build a LaTeX filing into a Quarry .qdoc + ground truth, then run the eval.
#
# Chain:  filing.tex --pdflatex--> filing.pdf
#                    --pdf_to_qdoc.py--> filing.qdoc   (spans + detected regions)
#                    --build_truth.py--> filing.truth.json   (known cells + anchors)
#                    --quarry eval--> silent-failure catch rate
#
# Requires a TeX install (pdflatex or tectonic). Python deps are managed by uv
# (run `uv sync` once); the Python steps run via `uv run`.
# Usage:  scripts/latex/build.sh [name]      (default name: filing)
set -euo pipefail

NAME="${1:-filing}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
cd "$HERE"

# MacTeX installs here but may not be on PATH in a fresh shell.
export PATH="$PATH:/Library/TeX/texbin:/usr/local/texlive/2025/bin/universal-darwin"

if command -v pdflatex >/dev/null 2>&1; then
  echo "== pdflatex (pass 1/2) =="; pdflatex -interaction=nonstopmode -halt-on-error "$NAME.tex" >/dev/null
  echo "== pdflatex (pass 2/2) =="; pdflatex -interaction=nonstopmode -halt-on-error "$NAME.tex" >/dev/null
elif command -v tectonic >/dev/null 2>&1; then
  echo "== tectonic =="; tectonic "$NAME.tex"
else
  echo "ERROR: need pdflatex or tectonic on PATH (install MacTeX or 'brew install tectonic')." >&2
  exit 1
fi

echo "== bridge: pdf -> qdoc =="
uv run --project "$REPO" "$REPO/scripts/pdf_to_qdoc.py" "$NAME.pdf" -o "$NAME.qdoc"

echo "== build truth from cells + detected regions =="
uv run --project "$REPO" "$REPO/scripts/build_truth.py" --cells "$NAME.cells.json" --qdoc "$NAME.qdoc" -o "$NAME.truth.json"

echo "== quarry eval =="
( cd "$REPO" && cargo run -q -- eval "$HERE/$NAME.qdoc" --truth "$HERE/$NAME.truth.json" )
