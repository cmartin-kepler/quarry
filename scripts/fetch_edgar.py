#!/usr/bin/env python3
"""
fetch_edgar.py - Build a small, real document corpus from SEC EDGAR.

What EDGAR actually gives you, by format:
  - PDF  : 8-K exhibits (EX-99.x) are frequently investor/earnings decks as PDFs.
           This is your best free source of real, born-digital financial PDFs.
  - XLSX : Every 10-K/10-Q with XBRL has an auto-generated Financial_Report.xlsx
           (machine-made, multi-sheet, one logical statement per sheet) -- a good
           stressor for the XLSX "interpretation" tier (headers / unit rows /
           where one logical table ends).
  - HTML : 10-K/10-Q primary documents are HTML, not PDF, but they carry the
           densest footnoted multi-level financial tables. Useful as HTML-table
           ground-truth fodder even though they aren't PDFs.

Everything downloaded is recorded in manifest.csv with its exact source URL, so
each local file already has provenance you can carry into your labeling.

SEC etiquette (enforced by SEC, not optional):
  - A descriptive User-Agent with contact info is REQUIRED. Set SEC_USER_AGENT
    or pass --user-agent. Requests without it get blocked.
  - Keep under ~10 requests/sec. This script self-throttles.

Usage (Python deps via uv: run `uv sync` once at the repo root):
  export SEC_USER_AGENT="Kepler AI corpus-builder you@kepler.ai"
  uv run scripts/fetch_edgar.py --out ./corpus --per-company 1
  uv run scripts/fetch_edgar.py --forms 8-K --decks-only --per-company 3
  uv run scripts/fetch_edgar.py --ticker AAPL --ticker JPM
"""
from __future__ import annotations

import argparse
import csv
import json
import os
import re
import sys
import time
from dataclasses import dataclass, field

import requests

# Curated, table-heavy issuers (CIKs are stable). Banks/insurers have the
# gnarliest tables; large caps reliably post 8-K investor decks as PDFs.
DEFAULT_COMPANIES = {
    "JPMorgan_Chase": 19617,
    "Bank_of_America": 70858,
    "Berkshire_Hathaway": 1067983,
    "Prudential_Financial": 1137774,
    "Apple": 320193,
    "Microsoft": 789019,
    "NVIDIA": 1045810,
}

ARCHIVES = "https://www.sec.gov/Archives/edgar/data"
SUBMISSIONS = "https://data.sec.gov/submissions/CIK{cik:010d}.json"
TICKER_MAP = "https://www.sec.gov/files/company_tickers.json"

# Heuristic for spotting an investor-deck / exhibit PDF inside a filing folder.
DECK_HINTS = re.compile(r"(ex[\-_]?99|investor|present|slides|deck|earnings)", re.I)

MIN_INTERVAL = 0.15  # seconds between requests (~6-7/s, comfortably under 10/s)


@dataclass
class Downloaded:
    company: str
    cik: int
    form: str
    filing_date: str
    accession: str
    local_path: str
    source_url: str
    kind: str  # pdf | xlsx | html


class Edgar:
    def __init__(self, user_agent: str):
        self.s = requests.Session()
        self.s.headers.update({"User-Agent": user_agent, "Accept-Encoding": "gzip, deflate"})
        self._last = 0.0

    def get(self, url: str, *, want_json=False, binary=False, tries=4):
        for attempt in range(tries):
            gap = time.monotonic() - self._last
            if gap < MIN_INTERVAL:
                time.sleep(MIN_INTERVAL - gap)
            self._last = time.monotonic()
            r = self.s.get(url, timeout=30)
            if r.status_code == 429:  # rate-limited; back off
                time.sleep(1.5 * (attempt + 1))
                continue
            if r.status_code == 404:
                return None
            r.raise_for_status()
            return r.json() if want_json else (r.content if binary else r.text)
        return None

    def submissions(self, cik: int) -> dict | None:
        return self.get(SUBMISSIONS.format(cik=cik), want_json=True)

    def filing_index(self, cik: int, accession_nodash: str) -> dict | None:
        url = f"{ARCHIVES}/{cik}/{accession_nodash}/index.json"
        return self.get(url, want_json=True)

    def resolve_ticker(self, ticker: str) -> int | None:
        data = self.get(TICKER_MAP, want_json=True) or {}
        for row in data.values():
            if row.get("ticker", "").upper() == ticker.upper():
                return int(row["cik_str"])
        return None


def recent_filings(subs: dict, forms: set[str], limit_per_form: int):
    """Yield (form, date, accession) for the most recent filings of each form."""
    recent = subs.get("filings", {}).get("recent", {})
    accns = recent.get("accessionNumber", [])
    fms = recent.get("form", [])
    dates = recent.get("filingDate", [])
    counts: dict[str, int] = {}
    for accn, form, date in zip(accns, fms, dates):
        if form not in forms:
            continue
        if counts.get(form, 0) >= limit_per_form:
            continue
        counts[form] = counts.get(form, 0) + 1
        yield form, date, accn


def pick_documents(items: list[dict], form: str, decks_only: bool):
    """From a filing's directory items, choose what to download.

    Returns list of (filename, kind). For 8-Ks we look for deck-like PDFs;
    for 10-K/10-Q we grab Financial_Report.xlsx and (unless decks_only) the
    primary HTML.
    """
    names = [it.get("name", "") for it in items]
    chosen: list[tuple[str, str]] = []

    # Investor-deck / exhibit PDFs (the real born-digital PDF path).
    pdfs = [n for n in names if n.lower().endswith(".pdf")]
    deck_pdfs = [n for n in pdfs if DECK_HINTS.search(n)] or pdfs
    for n in deck_pdfs:
        chosen.append((n, "pdf"))

    if decks_only:
        return chosen

    # XLSX financial report (XBRL-derived).
    for n in names:
        if n.lower() == "financial_report.xlsx":
            chosen.append((n, "xlsx"))

    # Primary HTML doc for 10-K/10-Q (dense tables; not a PDF but useful).
    if form in {"10-K", "10-Q"}:
        htmls = [
            n for n in names
            if n.lower().endswith((".htm", ".html"))
            and not n.lower().startswith("r")          # skip XBRL R*.htm viewer pages
            and "index" not in n.lower()
        ]
        # Heuristic: the primary doc is usually the largest .htm; fall back to first.
        by_size = sorted(
            [it for it in items if it.get("name") in htmls],
            key=lambda it: int(it.get("size", 0) or 0),
            reverse=True,
        )
        if by_size:
            chosen.append((by_size[0]["name"], "html"))
    return chosen


def build_corpus(args) -> list[Downloaded]:
    ua = args.user_agent or os.environ.get("SEC_USER_AGENT", "")
    if not ua or "@" not in ua:
        sys.exit(
            "ERROR: SEC requires a User-Agent with contact info.\n"
            '  export SEC_USER_AGENT="Your Name you@example.com"\n'
            "  (or pass --user-agent). Requests without it are blocked by SEC."
        )

    edgar = Edgar(ua)

    companies = dict(DEFAULT_COMPANIES)
    for t in args.ticker or []:
        cik = edgar.resolve_ticker(t)
        if cik:
            companies[t.upper()] = cik
        else:
            print(f"  ! could not resolve ticker {t}", file=sys.stderr)

    forms = set(args.forms)
    got: list[Downloaded] = []

    for company, cik in companies.items():
        print(f"[{company}] CIK {cik}")
        subs = edgar.submissions(cik)
        if not subs:
            print("  ! no submissions", file=sys.stderr)
            continue

        for form, date, accn in recent_filings(subs, forms, args.per_company):
            accn_nodash = accn.replace("-", "")
            idx = edgar.filing_index(cik, accn_nodash)
            if not idx:
                continue
            items = idx.get("directory", {}).get("item", [])
            for fname, kind in pick_documents(items, form, args.decks_only):
                src = f"{ARCHIVES}/{cik}/{accn_nodash}/{fname}"
                blob = edgar.get(src, binary=True)
                if blob is None:
                    continue
                dest_dir = os.path.join(args.out, form, f"{company}_{date}_{accn}")
                os.makedirs(dest_dir, exist_ok=True)
                dest = os.path.join(dest_dir, fname)
                with open(dest, "wb") as fh:
                    fh.write(blob)
                got.append(Downloaded(company, cik, form, date, accn, dest, src, kind))
                print(f"  + {kind:4} {fname}  ({len(blob)//1024} KB)")

    return got


def write_manifest(rows: list[Downloaded], out: str):
    path = os.path.join(out, "manifest.csv")
    with open(path, "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["company", "cik", "form", "filing_date", "accession",
                    "kind", "local_path", "source_url"])
        for r in rows:
            w.writerow([r.company, r.cik, r.form, r.filing_date, r.accession,
                        r.kind, r.local_path, r.source_url])
    return path


def main():
    p = argparse.ArgumentParser(description="Build a real document corpus from SEC EDGAR.")
    p.add_argument("--out", default="./corpus", help="output directory")
    p.add_argument("--forms", nargs="+", default=["8-K", "10-K"],
                   help="filing forms to fetch (e.g. 8-K 10-K 10-Q)")
    p.add_argument("--per-company", type=int, default=1,
                   help="most-recent filings per form per company")
    p.add_argument("--ticker", action="append",
                   help="add a company by ticker (repeatable); resolved via EDGAR")
    p.add_argument("--decks-only", action="store_true",
                   help="only fetch exhibit/deck PDFs (skip xlsx and html)")
    p.add_argument("--user-agent", help='SEC User-Agent, e.g. "Name you@example.com"')
    args = p.parse_args()

    os.makedirs(args.out, exist_ok=True)
    rows = build_corpus(args)
    if not rows:
        print("\nNothing downloaded. Check your User-Agent and form selection.")
        return
    mpath = write_manifest(rows, args.out)
    by_kind: dict[str, int] = {}
    for r in rows:
        by_kind[r.kind] = by_kind.get(r.kind, 0) + 1
    print(f"\nDone: {len(rows)} files "
          f"({', '.join(f'{v} {k}' for k, v in sorted(by_kind.items()))})")
    print(f"Manifest: {mpath}")


if __name__ == "__main__":
    main()
