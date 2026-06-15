#!/usr/bin/env python3
"""
run_corpus.py - Run the full Quarry pipeline across a directory of real PDFs.

For each PDF (recursively):  pdf_to_qdoc.py (bridge) -> `quarry parse` ->
`quarry check` (the detectors), then aggregate what was reconstructed and what
got flagged.

NOTE ON GROUND TRUTH: real documents have no hand-labeled truth, so this does
NOT compute a silent-failure catch rate (that needs to know which extractions are
actually wrong). It exercises the pipeline end to end on real input and surfaces
which reconstructed tables the detectors flag as suspicious. To get a catch rate,
hand-label a few of these (see scripts/README.md) and use `quarry eval`.

Robustness: skips non-PDFs, caps pages (--max-pages) and file size (--max-mb) for
pathological inputs, and time-limits each doc — reporting skips/timeouts rather
than silently dropping them.

Usage:
  uv run scripts/run_corpus.py /path/to/docs --out corpus/finance
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
BRIDGE = os.path.join(HERE, "pdf_to_qdoc.py")
QUARRY = os.path.join(REPO, "target", "debug", "quarry")


def find_pdfs(root: str) -> list[str]:
    out = []
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            if f.lower().endswith(".pdf"):
                out.append(os.path.join(dirpath, f))
    return sorted(out)


def run(cmd: list[str], timeout: int) -> tuple[int, str, str]:
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout, p.stderr


def process(pdf: str, outdir: str, max_pages: int, timeout: int) -> dict:
    stem = os.path.splitext(os.path.basename(pdf))[0]
    qdoc = os.path.join(outdir, stem + ".qdoc")
    art = os.path.join(outdir, stem + ".artifacts")
    row = {"doc": stem, "pages": 0, "spans": 0, "regions": 0,
           "tables": 0, "flagged": 0, "status": "ok"}
    try:
        # 1. bridge: pdf -> qdoc
        rc, _, err = run(
            [sys.executable, BRIDGE, pdf, "-o", qdoc, "--max-pages", str(max_pages)],
            timeout,
        )
        if rc != 0:
            row["status"] = "bridge-error"
            return row
        doc = json.load(open(qdoc))
        row["pages"] = len(doc["pages"])
        row["spans"] = sum(len(p["spans"]) for p in doc["pages"])
        row["regions"] = sum(len(p["table_regions"]) for p in doc["pages"])

        # No text layer at all (e.g. scanned or outlined-vector decks): the
        # born-digital path can't see these; they'd need a render+OCR/VLM tier.
        if row["spans"] == 0:
            row["status"] = "no-text-layer"
            return row

        # 2. parse: qdoc -> artifacts (HTML + manifest + verdicts)
        rc, _, _ = run([QUARRY, "parse", qdoc, "--out", art, "--tier", "0"], timeout)
        if rc != 0:
            row["status"] = "parse-error"
            return row
        manifest = json.load(open(os.path.join(art, "manifest.json")))
        row["tables"] = sum(1 for a in manifest["artifacts"] if a.get("kind") == "HtmlTable")

        # 3. detectors: count tables with >=1 flag (from append-only verdicts)
        verdicts = json.load(open(os.path.join(art, "verdicts.json")))
        row["flagged"] = sum(1 for v in verdicts if v.get("flagged"))
    except subprocess.TimeoutExpired:
        row["status"] = f"timeout(>{timeout}s)"
    except Exception as e:  # noqa: BLE001 - surface any failure per-doc, keep going
        row["status"] = f"error: {type(e).__name__}"
    return row


def main():
    ap = argparse.ArgumentParser(description="Run Quarry across a directory of PDFs.")
    ap.add_argument("input", help="directory to scan recursively for PDFs")
    ap.add_argument("--out", default="corpus/finance", help="output dir for qdoc/artifacts")
    ap.add_argument("--max-pages", type=int, default=50, help="page cap per doc")
    ap.add_argument("--max-mb", type=float, default=40.0, help="skip PDFs larger than this")
    ap.add_argument("--timeout", type=int, default=120, help="per-doc wall-clock limit (s)")
    args = ap.parse_args()

    if not os.path.exists(QUARRY):
        sys.exit(f"build the binary first: (cd {REPO} && cargo build)")
    os.makedirs(args.out, exist_ok=True)

    pdfs = find_pdfs(args.input)
    print(f"found {len(pdfs)} PDF(s) under {args.input}\n", file=sys.stderr)

    rows, skipped = [], []
    for pdf in pdfs:
        mb = os.path.getsize(pdf) / 1e6
        name = os.path.relpath(pdf, args.input)
        if mb > args.max_mb:
            skipped.append((name, f"{mb:.0f} MB > --max-mb {args.max_mb:.0f}"))
            print(f"  skip  {name}  ({mb:.0f} MB)", file=sys.stderr)
            continue
        t0 = time.monotonic()
        row = process(pdf, args.out, args.max_pages, args.timeout)
        row["doc"] = name
        rows.append(row)
        print(f"  {row['status']:<14} {name}  "
              f"({row['tables']} tables, {row['flagged']} flagged, "
              f"{time.monotonic() - t0:.1f}s)", file=sys.stderr)

    # Summary table to stdout.
    print(f"\n{'document':<48} {'pages':>5} {'spans':>7} {'regions':>7} "
          f"{'tables':>6} {'flagged':>7}  status")
    print("-" * 100)
    for r in rows:
        print(f"{r['doc'][:48]:<48} {r['pages']:>5} {r['spans']:>7} {r['regions']:>7} "
              f"{r['tables']:>6} {r['flagged']:>7}  {r['status']}")

    ok = [r for r in rows if r["status"] == "ok"]
    no_text = [r for r in rows if r["status"] == "no-text-layer"]
    tot_tables = sum(r["tables"] for r in ok)
    tot_flagged = sum(r["flagged"] for r in ok)
    print(f"\n{len(ok)}/{len(rows)} processed OK; "
          f"{tot_tables} tables reconstructed, {tot_flagged} flagged by >=1 detector "
          f"({100 * tot_flagged / tot_tables:.0f}%)" if tot_tables else
          f"\n{len(ok)}/{len(rows)} processed OK; no tables reconstructed")
    if no_text:
        print(f"{len(no_text)} doc(s) had NO text layer (scanned/outlined-vector — "
              f"need an OCR/VLM tier): {', '.join(r['doc'] for r in no_text)}")
    if skipped:
        print(f"\nskipped {len(skipped)} (size cap):")
        for name, why in skipped:
            print(f"  {name} — {why}")
    print("\nNote: no ground truth for these, so no catch rate. 'flagged' = tables the "
          "detectors marked suspicious; hand-label a few + use `quarry eval` for a catch rate.")


if __name__ == "__main__":
    main()
