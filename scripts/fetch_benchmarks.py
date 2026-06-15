#!/usr/bin/env python3
"""
fetch_benchmarks.py - Download parsing benchmarks from HuggingFace.

Pulls:
  - piushorn/pdf-parse-bench : ~200 SYNTHETIC PDFs generated from LaTeX, with
        automatic ground truth. The rare source that gives you actual born-digital
        PDFs (real text layer, true PDF coordinates) + free labels. Clean-path
        smoke test; NOT representative of real-world messiness.
  - llamaindex/ParseBench    : ~2,000 human-verified enterprise pages (insurance,
        finance, government), 5 dimensions (tables, charts, faithfulness,
        formatting, visual grounding), Apache-2.0. Almost certainly page IMAGES +
        rule annotations, not source PDFs -- exercises the VLM/OCR tier, not your
        born-digital text-layer path. Check the file tree (printed below).

After download it prints a small inventory (file-type counts) so you can see at a
glance whether you got PDFs or page images.

Usage (Python deps via uv: run `uv sync` once at the repo root):
  uv run scripts/fetch_benchmarks.py --out ./corpus/benchmarks
  uv run scripts/fetch_benchmarks.py --only piushorn/pdf-parse-bench
"""
from __future__ import annotations

import argparse
import os
from collections import Counter

DATASETS = ["piushorn/pdf-parse-bench", "llamaindex/ParseBench"]


def inventory(root: str, top_n: int = 12) -> str:
    ext_counts: Counter[str] = Counter()
    total = 0
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            ext = os.path.splitext(f)[1].lower() or "(none)"
            ext_counts[ext] += 1
            total += 1
    lines = [f"  files: {total}"]
    for ext, n in ext_counts.most_common(top_n):
        lines.append(f"    {ext:8} {n}")
    # The signal you care about: PDFs vs images.
    pdfs = ext_counts.get(".pdf", 0)
    imgs = sum(ext_counts.get(e, 0) for e in (".png", ".jpg", ".jpeg", ".webp", ".tiff"))
    verdict = ("source PDFs present" if pdfs else
               "no PDFs -- page images only" if imgs else
               "no PDFs or images found (likely annotations/parquet only)")
    lines.append(f"  >> {verdict} (pdf={pdfs}, images={imgs})")
    return "\n".join(lines)


def main():
    p = argparse.ArgumentParser(description="Download parsing benchmarks from HuggingFace.")
    p.add_argument("--out", default="./corpus/benchmarks", help="output directory")
    p.add_argument("--only", action="append", choices=DATASETS,
                   help="download only this dataset (repeatable)")
    p.add_argument("--allow", action="append",
                   help="glob(s) to restrict download, e.g. '*.pdf' '*.json' (repeatable)")
    args = p.parse_args()

    try:
        from huggingface_hub import snapshot_download
    except ImportError:
        raise SystemExit("Install first:  pip install huggingface_hub")

    targets = args.only or DATASETS
    os.makedirs(args.out, exist_ok=True)

    for repo in targets:
        local = os.path.join(args.out, repo.replace("/", "__"))
        print(f"\n=== {repo} ===")
        path = snapshot_download(
            repo_id=repo,
            repo_type="dataset",
            local_dir=local,
            allow_patterns=args.allow,  # None => everything
        )
        print(f"downloaded to: {path}")
        print(inventory(path))

    print(
        "\nNote: ParseBench also ships its own harness "
        "(git clone https://github.com/run-llama/ParseBench && uv sync --extra runners),\n"
        "which can run a parser through its pipeline registry directly."
    )


if __name__ == "__main__":
    main()
