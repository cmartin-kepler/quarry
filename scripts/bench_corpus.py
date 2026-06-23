#!/usr/bin/env python3
"""Run the real pipeline (+ materialize + OCR) across the whole corpus and collect
per-document stage timings into bench_corpus.json (CLI-interrogable).

Each doc's <store>/_timings.json holds the actual per-stage times the pipeline
recorded: triage, docling, ocr-scan, materialize, ocr. 'parse' (no OCR) = total − ocr.
"""
import glob
import json
import os
import subprocess
import tempfile

Q = "target/release/quarry"
STAGES = ["triage", "docling", "materialize", "ocr-scan", "ocr"]


def run(*args):
    subprocess.run([Q, *args], capture_output=True)


def main():
    pdfs = sorted(glob.glob("input/**/*.pdf", recursive=True))
    rows = []
    print(f"{'document':40}{'triage':>7}{'docling':>8}{'mat':>6}{'ocrscan':>8}{'ocr':>7}{'parse':>7}{'+ocr':>7}", flush=True)
    for pdf in pdfs:
        store = tempfile.mkdtemp(prefix="bench_")
        run("pipeline", pdf, "--out", store)
        run("materialize", store)
        run("ocr", store, "--pdf", pdf)
        try:
            t = dict(json.load(open(os.path.join(store, "_timings.json"))))
        except Exception:
            t = {}
        t = {k: int(t.get(k, 0)) for k in STAGES}
        parse_ms = t["triage"] + t["docling"] + t["materialize"] + t["ocr-scan"]
        total_ms = parse_ms + t["ocr"]
        row = {"document": os.path.basename(pdf), **t, "parse_ms": parse_ms, "total_ms": total_ms}
        rows.append(row)
        print(
            f"{os.path.basename(pdf)[:39]:40}{t['triage']:>6}m{t['docling']:>7}m{t['materialize']:>5}m"
            f"{t['ocr-scan']:>7}m{t['ocr']:>6}m{parse_ms/1000:>6.1f}s{total_ms/1000:>6.1f}s",
            flush=True,
        )
        subprocess.run(["rm", "-rf", store])

    json.dump(rows, open("bench_corpus.json", "w"), indent=2)
    agg = {k: sum(r[k] for r in rows) for k in STAGES + ["parse_ms", "total_ms"]}
    print(
        f"\nCORPUS ({len(rows)} docs): parse(no-OCR) {agg['parse_ms']/1000:.0f}s "
        f"(triage {agg['triage']/1000:.0f}s · docling {agg['docling']/1000:.0f}s · "
        f"materialize {agg['materialize']/1000:.0f}s · ocr-scan {agg['ocr-scan']/1000:.0f}s) "
        f"· OCR adds {agg['ocr']/1000:.0f}s → total {agg['total_ms']/1000:.0f}s"
    )
    print("wrote bench_corpus.json")


if __name__ == "__main__":
    main()
