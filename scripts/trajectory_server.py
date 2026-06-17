#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["flask>=3", "docling", "pdfplumber>=0.11", "ultralytics", "huggingface_hub", "doclayout-yolo", "pypdf"]
# ///
"""
trajectory_server.py - Interactive, ON-DEMAND parsing-trajectory UI.

A local web app. Nothing is pre-computed or hardcoded: you click a table region in
the PDF and the server parses it live with the cheap method, validates it, and
reports the measured time. If it's flagged you click "escalate" and the NEXT
method runs on demand (LiteParse on its text, then Docling parsing just that page
via page_range, then vision) — each timed live. The lineage graph builds up node
by node as you escalate.

Methods are path-dependent (different representations):
  cheap geometric (PDF text-layer) · text-table (region text → a text-grid you
  then `structure` into columns by word geometry) · Docling (PDF, per-page) ·
  vision verify (image)

Run (deps declared inline via PEP 723, so plain uv run works):
  uv run scripts/trajectory_server.py            # -> http://127.0.0.1:5050
  PORT=8080 uv run scripts/trajectory_server.py  # pick another port
(Port 5000 is avoided: macOS AirPlay Receiver squats on it and returns 403.)
"""
from __future__ import annotations

import base64
import functools
import io
import json
import os
import re
import subprocess
import tempfile
import threading
import time
from dataclasses import asdict, dataclass, field
from enum import Enum
from typing import Literal, Optional

# Quiet the harmless "leaked semaphore" warning Docling's HF tokenizers emit at
# shutdown (multiprocessing parallelism not torn down cleanly). Must be set before
# tokenizers is imported (Docling loads lazily in _converter()).
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

import pdfplumber
from flask import Flask, Response, jsonify, request

import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import recon_validate as rv  # noqa: E402
import text_tables as tt  # noqa: E402
import typed_table as typ  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QUARRY = os.path.join(REPO, "target", "debug", "quarry")


def _load_env():
    """Load REDUCTO_API_KEY / LLAMAPARSE_API_KEY etc. from .env (gitignored)."""
    p = os.path.join(REPO, ".env")
    if os.path.exists(p):
        for line in open(p):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


_load_env()
DH = "0" * 64
VISION_RATE, VISION_TIME = 0.02, 1.2
BUILD = "multiindex-typed-36"  # bump on server changes; shown in the UI header to verify what's running

INPUT_DIR = os.path.join(REPO, "input")
# Friendlier display names for known files; any other PDF shows as its path under input/.
ALIASES = {
    "finance/disney/q2-fy26-financial-reconciliations.pdf": "Disney Q2 reconciliations",
    "arxiv/2604.08538v3.pdf": "ParseBench paper (arXiv)",
    "finance/jpm-2023-ar.pdf": "JPMorgan annual report",
}
DOCS: dict[str, str] = {}


def refresh_docs():
    """Discover every PDF under input/ (re-scanned on each /api/docs, so dropping
    a new PDF in input/ makes it loadable without restarting the server)."""
    DOCS.clear()
    for root, _, files in os.walk(INPUT_DIR):
        for f in sorted(files):
            if f.lower().endswith(".pdf"):
                full = os.path.join(root, f)
                rel = os.path.relpath(full, INPUT_DIR)
                # Display name must be slash-free: the Flask <name> route segment
                # doesn't match "/". Show subdirs with a visual separator instead.
                name = ALIASES.get(rel, rel[:-4].replace(os.sep, " › "))
                DOCS[name] = full
    return DOCS

app = Flask(__name__)
_pdf, _meta, _regions, _pageimg = {}, {}, {}, {}
_lite, _docling, _wd = {}, {}, tempfile.mkdtemp()
_layout = {}

# pypdfium2 / pdfplumber are NOT thread-safe, and the dev server is threaded — so
# concurrent renders (the viewer lazy-loading page images while a layout pass
# rasterizes) corrupt the shared document ("PDFium: Data format error"). Serialize
# every PDF-touching request through one lock.
_pdf_lock = threading.RLock()


def locked(fn):
    @functools.wraps(fn)
    def wrapper(*a, **k):
        with _pdf_lock:
            return fn(*a, **k)
    return wrapper


def pdf(name):
    if name not in _pdf:
        if name not in DOCS:
            refresh_docs()
        _pdf[name] = pdfplumber.open(DOCS[name])
    return _pdf[name]


def sh(cmd):
    return subprocess.run(cmd, capture_output=True, text=True)


def store_tables(store):
    man = json.load(open(os.path.join(store, "manifest.json")))
    ev = {e["id"]: e for e in json.loads(
        sh([QUARRY, "explain", store, "--json"]).stdout or "[]")}
    out = []
    for a in man["artifacts"]:
        if a.get("kind") != "HtmlTable":
            continue
        s = a["meta"]["provenance"].get("Source") or {}
        b = s["bbox"]; e = ev.get(a["meta"]["id"], {})
        out.append({"page": s["page"], "bbox": (b["x0"], b["y0"], b["x1"], b["y1"]),
                    "html": a["html"], "ev": e})
    return out


def status_of(ev):
    if any("figure" in g["detail"] for g in ev.get("signals", [])):
        return "figure"
    return {"confirmed": "confirmed", "no_issues": "ok", "suspect": "suspect"}.get(ev.get("impression"), "ok")


def iou(a, b):
    ix0, iy0, ix1, iy1 = max(a[0], b[0]), max(a[1], b[1]), min(a[2], b[2]), min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1-ix0)*(iy1-iy0)
    return inter / ((a[2]-a[0])*(a[3]-a[1]) + (b[2]-b[0])*(b[3]-b[1]) - inter + 1e-9)


# Region detection. pdfplumber's default find_tables keys off RULED LINES, so it
# only finds bordered tables (and collapses text-aligned columns to one column).
# Most finance/academic tables are borderless, which is why "it doesn't find much".
# The text strategy recovers them from word alignment; we gate it on numeric
# density so justified prose (which also aligns into pseudo-columns) isn't flagged.
_TEXT_TS = {"vertical_strategy": "text", "horizontal_strategy": "text",
            "min_words_vertical": 3, "min_words_horizontal": 2, "intersection_tolerance": 12}
_NUMRE = re.compile(r"^[\(\-]?\$?[\d,]*\.?\d+%?\)?$")


def detect_regions(pg):
    """Return [(bbox, ncols, source)] combining ruled-line and text-aligned
    detection, deduped by overlap. Text-aligned tables must be multi-column and
    numeric enough to be a data table, not prose. Each bbox is snapped out to the
    full extent of the words in its row band, so find_tables' often-tight box
    doesn't clip an overhanging cell (e.g. a 'May' month left of the date column)."""
    words = pg.extract_words()

    def numfrac(bbox):
        x0, top, x1, bottom = bbox
        ws = [w for w in words if x0 <= (w["x0"]+w["x1"])/2 <= x1 and top <= (w["top"]+w["bottom"])/2 <= bottom]
        return sum(1 for w in ws if _NUMRE.match(w["text"])) / len(ws) if ws else 0.0

    def snap(bbox, halo=55):
        x0, top, x1, bottom = bbox
        xs, ys = [x0, x1], [top, bottom]
        for w in words:
            cy, cx = (w["top"]+w["bottom"])/2, (w["x0"]+w["x1"])/2
            if top - 2 <= cy <= bottom + 2 and x0 - halo <= cx <= x1 + halo:
                xs += [w["x0"], w["x1"]]; ys += [w["top"], w["bottom"]]
        return (min(xs), max(0, min(ys)), max(xs), max(ys))

    out = []
    try:
        for t in pg.find_tables(_TEXT_TS):
            nc = len(t.rows[0].cells) if t.rows else 0
            if nc >= 2 and len(t.rows) >= 3 and numfrac(t.bbox) >= 0.08:
                out.append((snap(t.bbox), nc, "text"))
    except Exception:  # noqa: BLE001
        pass
    try:
        for t in pg.find_tables():  # ruled lines; add only if not already covered
            if not any(iou(t.bbox, b) > 0.5 for b, _, _ in out):
                nc = len(t.rows[0].cells) if t.rows else 0
                out.append((snap(t.bbox), nc, "lines"))
    except Exception:  # noqa: BLE001
        pass
    return out


def recon_for(name, page, bbox, html):
    res, det = rv.validate_detail(DOCS[name], page, bbox, html)
    if res.status != "ok":
        return None, None
    pg = pdf(name).pages[page-1]
    x0, y0, x1, y1 = bbox
    im = pg.crop((max(0, x0-4), max(0, y0-4), min(pg.width, x1+4), min(pg.height, y1+4))).to_image(resolution=130)
    colors = {"matched": (0, 160, 110), "misplaced": (235, 140, 0), "missing": (215, 0, 0)}
    for tok in det["obs"]:
        c = colors[tok["status"]]
        im.draw_rect(tok["bbox"], stroke=c, stroke_width=1 if tok["status"] == "matched" else 2, fill=c+(28,))
    buf = io.BytesIO(); im.save(buf, format="PNG")
    return round(res.error, 3), base64.b64encode(buf.getvalue()).decode()


# ---- on-demand parsing (each runs live, measures real time) ----------------


def ensure_lite(name):
    if name not in _lite:
        t = time.monotonic()
        js = os.path.join(_wd, "lp_" + str(abs(hash(name))) + ".json")
        err = None
        try:
            r = sh(["lit", "parse", DOCS[name], "--format", "json", "-o", js, "-q"])
            lj = json.load(open(js))
            text = {p["page"]: p["text"] for p in lj["pages"]}
            n_pages = len(lj["pages"])
        except FileNotFoundError:
            err = "the `lit` (LiteParse) CLI is not on the server's PATH"
            text, n_pages = {}, len(pdf(name).pages)
        except Exception as e:  # noqa: BLE001 — bad/empty lit output
            err = f"LiteParse failed: {(r.stderr if 'r' in dir() else str(e))[:120]}"
            text, n_pages = {}, len(pdf(name).pages)
        if err:
            print(f"[lite] {name}: {err}", flush=True)
        _lite[name] = {"text": text, "secs": time.monotonic()-t,
                       "n_pages": max(1, n_pages), "err": err}
    return _lite[name]


_conv = None


def _converter():
    global _conv
    if _conv is None:
        from docling.document_converter import DocumentConverter
        _conv = DocumentConverter()
    return _conv


# Docling label (snake_case) -> our display label (matches LAYCOLOR / YOLO labels).
_DL_LABEL = {"text": "Text", "paragraph": "Text", "title": "Title", "section_header": "Section-header",
             "table": "Table", "picture": "Picture", "figure": "Picture", "caption": "Caption",
             "list_item": "List-item", "formula": "Formula", "page_header": "Page-header",
             "page_footer": "Page-footer", "footnote": "Footnote", "code": "Formula"}


def _docling_layout(doc, page):
    """Layout elements from Docling's own document model (it runs a DocLayNet
    layout detector internally) — boxes in top-left PDF points, like pdfplumber."""
    els = []
    for item, _lvl in doc.iterate_items():
        prov = getattr(item, "prov", None)
        if not prov or prov[0].page_no != page:
            continue
        pg = doc.pages.get(page)
        ph = pg.size.height if pg else None
        bb = prov[0].bbox.to_top_left_origin(page_height=ph) if ph else prov[0].bbox
        raw = getattr(item.label, "value", item.label)
        els.append({"label": _DL_LABEL.get(str(raw), str(raw).replace("_", " ").title()),
                    "conf": 1.0, "bbox": [float(bb.l), float(bb.t), float(bb.r), float(bb.b)]})
    return els


def docling_page(name, page):
    key = (name, page)
    if key not in _docling:
        t = time.monotonic()
        res = _converter().convert(DOCS[name], page_range=(page, page))
        secs = time.monotonic()-t
        wd = os.path.join(_wd, f"dl_{abs(hash(name))}_{page}")
        os.makedirs(wd, exist_ok=True)
        js = os.path.join(wd, "d.json"); json.dump(res.document.export_to_dict(), open(js, "w"))
        st = os.path.join(wd, "d.art")
        sh([QUARRY, "import-docling", js, "--pdf", DOCS[name], "--out", st])
        try:
            layout = _docling_layout(res.document, page)
        except Exception as e:  # noqa: BLE001
            print(f"[docling-layout] {name} p{page}: {str(e)[:120]}", flush=True); layout = []
        _docling[key] = {"tables": store_tables(st), "secs": secs, "layout": layout}
    return _docling[key]


def explain_grid(grid, page, header_rows=1):
    cells = [{"row": r, "col": c, "text": t, "anchor": {"format": "pdf", "doc": DH, "page": page,
              "bbox": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}}, "is_header": r < header_rows}
             for r, row in enumerate(grid) for c, t in enumerate(row)]
    art = {"kind": "HtmlTable", "meta": {"id": "a", "content_hash": DH,
           "provenance": {"Source": {"format": "pdf", "doc": DH, "page": page, "bbox": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}}},
           "generation": 0, "risk": {"min_ocr_confidence": 1.0, "column_count_variance": 0.0,
           "merged_cell_rows": 0, "empty_cells": 0, "rotated_text": False, "figure_score": 0.0, "notes": []}},
           "n_rows": len(grid), "n_cols": max(len(r) for r in grid), "cells": cells, "html": tt.to_html(grid)}
    wd = tempfile.mkdtemp(); json.dump({"doc_hash": DH, "artifacts": [art]}, open(os.path.join(wd, "manifest.json"), "w"))
    e = json.loads(sh([QUARRY, "explain", wd, "--json"]).stdout or "[]")
    return e[0] if e else {"impression": "no_issues", "signals": []}


def tokens(s):
    return {"".join(c for c in w.lower() if c.isalnum()) for w in re.split(r"\s+", s)} - {""}


def lite_best_grid(name, page, bbox):
    """The grid LiteParse's text yields for the table under `bbox`, matched to the
    region's own PDF words. LiteParse splits a page into several grids while the
    region may be one big box, so we score each grid by max(precision, recall) of
    token overlap — precision (grid ⊆ region) catches a sub-table inside a big
    region; recall (region ⊆ grid) catches a region inside one big grid — then,
    among grids that clear the bar, pick the one covering the most region tokens.
    Returns (grid|None, secs, score). Shared by text-table and markdown."""
    lp = ensure_lite(name)
    pg = pdf(name).pages[page-1]
    x0, top, x1, bottom = bbox
    cr = pg.crop((max(0, x0), max(0, top), min(pg.width, x1), min(pg.height, bottom)))
    ref = tokens(" ".join(w["text"] for w in cr.extract_words()))
    grids = tt.detect_tables(lp["text"].get(page, ""))
    best, best_inter, best_score, max_seen = None, -1, 0.0, 0.0
    for g in grids:
        gt = tokens(" ".join(c2 for row in g for c2 in row))
        inter = len(ref & gt)
        score = max(inter / (len(gt) + 1e-9), inter / (len(ref) + 1e-9))
        max_seen = max(max_seen, score)
        if score >= 0.5 and inter > best_inter:
            best, best_inter, best_score = g, inter, score
    secs = lp["secs"] / max(1, lp["n_pages"])
    if best is None:
        if lp.get("err"):
            why = lp["err"]
        elif not grids:
            why = "LiteParse produced no detectable text grid on this page"
        else:
            why = f"{len(grids)} LiteParse grid(s) on this page, best overlap {max_seen:.2f} (<0.50) — none aligns with this region"
    else:
        why = None
    return best, secs, best_score, why


# ---- cloud page parsers (Reducto, LlamaParse) -------------------------------
# Both parse a page into structured content (Reducto -> HTML tables, LlamaParse ->
# markdown pipe tables); we pull the tables out, match the one over the region, and
# validate it the same way as every other extractor. Cost is real $ (per page).

def grids_from_content(content):
    """Tables in a parser's page output: HTML <table> blocks (Reducto) AND
    pipe/space-aligned tables (LlamaParse / text)."""
    grids = []
    for m in re.findall(r"<table.*?</table>", content or "", re.S | re.I):
        g, _ = rv.parse_html_grid(m)
        if g and len(g) >= 2:
            grids.append([[c.strip() for c in row] for row in g])
    grids += tt.detect_tables(content or "")
    return grids


_page_pdf, _provider = {}, {}


def single_page_pdf(name, page):
    """Extract page `page` as a 1-page PDF, so a cloud parse bills for one page."""
    key = (name, page)
    if key not in _page_pdf:
        from pypdf import PdfReader, PdfWriter
        w = PdfWriter(); w.add_page(PdfReader(DOCS[name]).pages[page-1])
        out = os.path.join(_wd, f"pg_{abs(hash(name))}_{page}.pdf")
        with open(out, "wb") as f:
            w.write(f)
        _page_pdf[key] = out
    return _page_pdf[key]


def _curl_json(args, timeout=180):
    r = subprocess.run(["curl", "-s", "--max-time", str(timeout)] + args,
                       capture_output=True, text=True, timeout=timeout + 10)
    if not r.stdout.strip():
        raise RuntimeError((r.stderr or "no response")[:200])
    return json.loads(r.stdout)


def reducto_parse(name, page):
    """Full Reducto result for one page (cached). Its blocks carry accurate,
    normalized bboxes — we use those rather than guessing from page text."""
    key = ("reducto", name, page)
    if key not in _provider:
        api = os.environ.get("REDUCTO_API_KEY")
        if not api:
            raise RuntimeError("REDUCTO_API_KEY not set (.env)")
        path = single_page_pdf(name, page)
        up = _curl_json(["-X", "POST", "https://platform.reducto.ai/upload",
                         "-H", f"Authorization: Bearer {api}", "-F", f"file=@{path}"])
        body = json.dumps({"document_url": up["file_id"], "options": {"chunking": {"chunk_mode": "page"}}})
        _provider[key] = _curl_json(["-X", "POST", "https://platform.reducto.ai/parse",
                                     "-H", f"Authorization: Bearer {api}", "-H", "Content-Type: application/json", "-d", body])
    return _provider[key]


def llamaparse_content(name, page):
    key = ("llamaparse", name, page)
    if key not in _provider:
        api = os.environ.get("LLAMAPARSE_API_KEY")
        if not api:
            raise RuntimeError("LLAMAPARSE_API_KEY not set (.env)")
        path = single_page_pdf(name, page)
        base = "https://api.cloud.llamaindex.ai/api/v1/parsing"
        auth = ["-H", f"Authorization: Bearer {api}"]
        jid = _curl_json(["-X", "POST", f"{base}/upload", *auth, "-F", f"file=@{path}"])["id"]
        for _ in range(60):
            st = _curl_json([*auth, f"{base}/job/{jid}"]).get("status")
            if st == "SUCCESS":
                break
            if st == "ERROR":
                raise RuntimeError("LlamaParse job failed")
            time.sleep(2)
        _provider[key] = _curl_json([*auth, f"{base}/job/{jid}/result/markdown"]).get("markdown", "")
    return _provider[key]


def _table_element(grid, bbox, conf, page):
    grid = [[c.strip() for c in row] for row in grid]
    ev = explain_grid(grid, page)
    try:
        conf = round(float(conf), 3)           # some providers return "high"/"low"
    except (TypeError, ValueError):
        conf = 1.0
    return {"label": "Table", "conf": conf, "bbox": bbox, "html": tt.to_html(grid),
            "status": status_of(ev), "impression": ev.get("impression"), "signals": ev.get("signals", [])}


def _bbox_from_words(grid, words, freq):
    """Derive a table's bbox from where its cell text sits on the page. Anchor on
    RARE tokens (≤2 occurrences) so common label words in surrounding prose don't
    inflate the box; fall back to all matches if too few rare anchors."""
    cells = tokens(" ".join(c for row in grid for c in row))
    def tok(w):
        return "".join(ch for ch in w["text"].lower() if ch.isalnum())
    pts = [w for w in words if tok(w) in cells and freq[tok(w)] <= 2]
    if len(pts) < 3:
        pts = [w for w in words if tok(w) in cells]
    if len(pts) < 3:
        return None
    return [min(w["x0"] for w in pts), min(w["top"] for w in pts),
            max(w["x1"] for w in pts), max(w["bottom"] for w in pts)]


def provider_layout(name, page, provider):
    """Page-level parse: surface each table as a Region carrying its already-parsed,
    validated HtmlTable (clicking it costs nothing more). Reducto gives accurate
    per-block bboxes; for LlamaParse we derive the bbox from page text."""
    pg = pdf(name).pages[page-1]
    if provider == "reducto":
        out = []
        for ch in reducto_parse(name, page).get("result", {}).get("chunks", []):
            for b in ch.get("blocks", []):
                if b.get("type") != "Table":
                    continue
                grid, _ = rv.parse_html_grid(b.get("content", "") or "")
                if not grid or len(grid) < 2:
                    continue
                bb = b["bbox"]
                bbox = [bb["left"]*pg.width, bb["top"]*pg.height,
                        (bb["left"]+bb["width"])*pg.width, (bb["top"]+bb["height"])*pg.height]
                out.append(_table_element(grid, bbox, b.get("confidence", 1.0) or 1.0, page))
        return out
    # LlamaParse: markdown tables, bbox derived from page words (rare-anchored).
    from collections import Counter
    words = pg.extract_words()
    freq = Counter("".join(ch for ch in w["text"].lower() if ch.isalnum()) for w in words)
    out = []
    for g in grids_from_content(llamaparse_content(name, page)):
        bbox = _bbox_from_words(g, words, freq)
        if bbox:
            out.append(_table_element(g, bbox, 1.0, page))
    return out


# ---- API -------------------------------------------------------------------

@app.get("/api/health")
def api_health():
    # The op registry is the authoritative scheme; the UI mirrors it from here.
    return jsonify({"build": BUILD,
                    "ops": [{"name": o.name, "kind": o.kind.value,
                             "consumes": o.consumes.value,
                             "produces": o.produces.value if o.produces else None,
                             "reads": o.reads, "label": o.label} for o in OPS.values()]})


@app.get("/api/docs")
def api_docs():
    return jsonify(list(refresh_docs()))


@app.get("/api/doc/<name>")
@locked
def api_doc(name):
    """Just page dimensions — instant for any doc size. Images and regions load
    lazily per page as the viewer scrolls."""
    if name not in _meta:
        pages = []
        for pg in pdf(name).pages:
            try:
                pages.append({"page": pg.page_number, "w": float(pg.width), "h": float(pg.height)})
            except Exception:  # noqa: BLE001 — a bad page still gets a slot (default dims)
                pages.append({"page": pg.page_number, "w": 612.0, "h": 792.0})
        _meta[name] = pages
    return jsonify({"pages": _meta[name]})


# Persistent Surya sidecar: a long-lived server process (isolated env) that loads
# the VLM once, so per-call cost is just inference rather than a model reload.
_surya = {"proc": None, "url": None}


def ensure_surya():
    if _surya["url"]:
        return _surya["url"]
    import socket
    import urllib.request
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "surya_layout.py")
    log = open(os.path.join(_wd, "surya_server.log"), "w")
    # New session so we can kill the whole group (surya server + its llama-server).
    _surya["proc"] = subprocess.Popen(["uv", "run", script, "--serve", str(port)],
                                      stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    url = f"http://127.0.0.1:{port}"
    for _ in range(900):  # first run downloads the model; wait generously
        if _surya["proc"].poll() is not None:
            raise RuntimeError("surya server exited (is llama.cpp installed?) — see surya_server.log")
        try:
            urllib.request.urlopen(url + "/health", timeout=2)
            _surya["url"] = url
            print(f"  surya server ready at {url}", flush=True)
            return url
        except Exception:  # noqa: BLE001 — not ready yet
            time.sleep(1)
    raise RuntimeError("surya server did not become ready in time")


import atexit
import signal


def _descendants(pid):
    """All transitive child pids of `pid` (llama-server is a grandchild of the
    `uv run` process, and may be in a different group, so kill the whole tree)."""
    out = subprocess.run(["ps", "-Ao", "pid=,ppid="], capture_output=True, text=True).stdout
    kids = {}
    for line in out.splitlines():
        try:
            c, p = (int(x) for x in line.split())
        except ValueError:
            continue
        kids.setdefault(p, []).append(c)
    seen, stack = [], [pid]
    while stack:
        for c in kids.get(stack.pop(), []):
            seen.append(c); stack.append(c)
    return seen


@atexit.register
def _stop_surya():
    p = _surya.get("proc")
    if not (p and p.poll() is None):
        return
    pids = _descendants(p.pid) + [p.pid]   # collect before killing (ppids change after)
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for pid in pids:
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass
        time.sleep(1.0)


def surya_layout(name, n):
    """Render page n, POST it to the warm Surya server, return its layout boxes."""
    import urllib.request
    url = ensure_surya()
    rr = 150
    tmp = os.path.join(_wd, f"surya_{abs(hash(name))}_{n}.png")
    pdf(name).pages[n-1].to_image(resolution=rr).save(tmp)
    req = urllib.request.Request(url + "/layout", data=json.dumps({"path": tmp}).encode(),
                                 headers={"Content-Type": "application/json"})
    boxes = json.loads(urllib.request.urlopen(req, timeout=300).read())
    scale = 72.0 / rr
    return [{"label": b["label"], "conf": b.get("conf", 1.0),
             "bbox": [v*scale for v in b["bbox"]]} for b in boxes]


def crop_table(name, page, bbox):
    """Region-scoped cheap extraction: build a grid from the text inside the
    region's bbox (text strategy first, ruled-line fallback). This makes the
    `Region —(cheap)→ table` edge honest — it parses the region, rather than
    matching against a separate whole-page detection."""
    pg = pdf(name).pages[page-1]
    x0, y0, x1, y1 = bbox
    crop = pg.crop((max(0, x0), max(0, y0), min(pg.width, x1), min(pg.height, y1)))
    for settings in ({"vertical_strategy": "text", "horizontal_strategy": "text",
                      "min_words_vertical": 2, "min_words_horizontal": 1, "intersection_tolerance": 12}, {}):
        try:
            grid = crop.extract_table(settings)
        except Exception:  # noqa: BLE001
            grid = None
        if grid:
            return [[(c or "").strip() for c in row] for row in grid]
    return None


def _placeholder_png(w, h, msg):
    from PIL import Image, ImageDraw
    img = Image.new("RGB", (max(2, int(w)), max(2, int(h))), (244, 245, 244))
    d = ImageDraw.Draw(img)
    d.text((24, 24), msg, fill=(150, 80, 80))
    buf = io.BytesIO(); img.save(buf, format="PNG"); return buf.getvalue()


# Normalize every detector's label vocabulary to one canonical set, so the
# "Table" filter and the color map work regardless of model.
_NORM = {"text": "Text", "plain text": "Text", "paragraph": "Text", "title": "Title",
         "section-header": "Section-header", "section_header": "Section-header",
         "table": "Table", "table_caption": "Caption", "table_footnote": "Footnote",
         "picture": "Picture", "figure": "Picture", "figure_caption": "Caption", "caption": "Caption",
         "list-item": "List-item", "list_item": "List-item", "list": "List-item",
         "formula": "Formula", "isolate_formula": "Formula", "formula_caption": "Caption",
         "page-header": "Page-header", "page_header": "Page-header",
         "page-footer": "Page-footer", "page_footer": "Page-footer",
         "footnote": "Footnote", "abandon": "Page-footer",
         # Surya's camelCase vocabulary
         "sectionheader": "Section-header", "pageheader": "Page-header", "pagefooter": "Page-footer",
         "listgroup": "List-item", "equation": "Formula", "figure": "Picture",
         "tableofcontents": "List-item", "bibliography": "Text", "form": "Text",
         "code": "Formula", "blankpage": "Page-footer", "chemicalblock": "Picture", "diagram": "Picture"}


def norm_label(lbl):
    return _NORM.get(str(lbl).strip().lower(), str(lbl).replace("_", " ").title())


@app.get("/api/layout/<name>/<int:n>")
@locked
def api_layout(name, n):
    """Learned layout detection: every document element (Table, Picture, Title,
    Text/paragraph, Section-header, …) on one page, as PDF-coordinate boxes.
    ?model = yolo26 (default) | doclayout (DocLayout-YOLO) | docling (Docling's own
    layout model). Lazy: model loads/downloads on first call; degrades gracefully."""
    model = request.args.get("model", "yolo26")
    key = (name, n, model)
    if key not in _layout:
        try:
            pg = pdf(name).pages[n-1]
            t0 = time.monotonic()
            if model in ("reducto", "llamaparse"):
                # Page-level cloud parse: each table region carries its parsed,
                # validated HtmlTable (+ next ops). Cost is one page ($/page).
                dollars = 0.015 if model == "reducto" else 0.0013
                els = []
                for i, el in enumerate(provider_layout(name, n, model)):
                    x0, y0, x1, y1 = el["bbox"]
                    nx = route(OpResult(method=model, kind="extract", input=model,
                                        status=el["status"], signals=el["signals"], html=el["html"]), [])
                    els.append({**el, "label": norm_label(el["label"]), "id": f"{model}_p{n}_{i}", "next": nx,
                                "box": {"left": 100*x0/pg.width, "top": 100*y0/pg.height,
                                        "width": 100*(x1-x0)/pg.width, "height": 100*(y1-y0)/pg.height}})
                _layout[key] = {"elements": els, "seconds": round(time.monotonic()-t0, 3), "dollars": dollars}
                return jsonify({**_layout[key], "model": model})
            if model == "find_tables":
                # Geometric detection (ruled + text-aligned) — just another model.
                raw = [{"label": "Table", "conf": 1.0, "bbox": list(b)}
                       for b, _nc, _src in detect_regions(pg)]
                secs = time.monotonic()-t0
            elif model == "surya":
                # Warm sidecar server (isolated env, model loaded once). First call
                # spawns it + downloads the model; failures -> "layout unavailable".
                raw = surya_layout(name, n)
                secs = time.monotonic()-t0
            elif model == "docling":
                # Docling's layout comes from the full conversion, so its cost IS
                # the conversion time (reused/cached by docling_page).
                dl = docling_page(name, n); raw = dl["layout"]; secs = dl["secs"]
            else:
                res = 150
                im = pdf(name).pages[n-1].to_image(resolution=res).original
                import yolo_layout as yl
                raw = yl.detect(im, res, key=model); secs = time.monotonic()-t0
            els = []
            for i, el in enumerate(raw):
                x0, y0, x1, y1 = el["bbox"]
                els.append({"label": norm_label(el["label"]), "conf": el.get("conf", 1.0),
                            "id": f"{model}_p{n}_{i}", "bbox": el["bbox"],
                            "box": {"left": 100*x0/pg.width, "top": 100*y0/pg.height,
                                    "width": 100*(x1-x0)/pg.width, "height": 100*(y1-y0)/pg.height}})
            _layout[key] = {"elements": els, "seconds": round(secs, 3)}
        except Exception as e:  # noqa: BLE001
            print(f"[layout:{model}] {name} p{n}: {str(e)[:160]}", flush=True)
            return jsonify({"error": str(e)[:200], "elements": []})
    return jsonify({**_layout[key], "model": model})


@app.get("/api/page/<name>/<int:n>")
@locked
def api_page(name, n):
    """Render one page on demand (lazy-loaded by the scrollable viewer). A page
    pdfium can't load (corrupt/encrypted) returns a placeholder instead of 500,
    so one bad page doesn't break the rest of the document."""
    key = (name, n)
    if key not in _pageimg:
        try:
            pg = pdf(name).pages[n-1]
            im = pg.to_image(resolution=150); buf = io.BytesIO(); im.save(buf, format="PNG")
            _pageimg[key] = buf.getvalue()
        except Exception as e:  # noqa: BLE001
            print(f"[page] {name} p{n}: {str(e)[:120]}", flush=True)
            try:
                pg = pdf(name).pages[n-1]; w, h = float(pg.width)*2, float(pg.height)*2
            except Exception:  # noqa: BLE001
                w, h = 1200, 1600
            # Don't cache the placeholder — allow a later retry to succeed.
            return Response(_placeholder_png(w, h, f"page {n} failed to render"),
                            mimetype="image/png")
    return Response(_pageimg[key], mimetype="image/png")


# ---- the artifact / operation scheme ----------------------------------------
# One invariant, two op shapes:
#   * extract / transform PRODUCE a new artifact (a new representation of content)
#   * validate ANNOTATES an artifact's evidence — it produces nothing new
# So validation is not an artifact; it enriches the table it judges (status +
# signals). That keeps every validator (reconcile, figure-guard, recon, vision)
# uniform — vision is no longer a special "verdict" node.

class ArtifactKind(str, Enum):
    PAGE = "page"          # the source page
    REGION = "region"      # a located area on the page (a layout detection)
    TEXTGRID = "text-grid"  # raw region text, columns not yet committed
    TABLE = "table"        # a parsed HtmlTable (+ its evidence)
    TYPED = "typed"        # a materialized, math-ready TypedTable


class OpKind(str, Enum):
    LAYOUT = "layout"        # Page      -> Region(s)        (segmentation; one op fans OUT)
    EXTRACT = "extract"      # Region    -> Table            (reparse a source repr, scoped to the region)
    TRANSFORM = "transform"  # Table     -> Table / Typed    (consume the artifact's own content)
    MERGE = "merge"          # [Region…] -> Region          (agree on a bbox; fans IN — the dual of LAYOUT)
    VALIDATE = "validate"    # Table     -> (no new artifact; attaches evidence to it)


# An evidence verdict — we can't prove correctness without ground truth, only
# gather signals; `status` is the strongest summary of them.
Status = Literal["confirmed", "ok", "suspect", "figure", "missing", "verified",
                 "typed", "located", "idle", "grid"]


@dataclass(frozen=True)
class Operation:
    """One edge in the artifact graph. `produces` is None for VALIDATE ops — they
    make no new artifact, they annotate the one they judge. `reads` names the
    source representation an EXTRACT reparses (the path-dependence that's the point
    of the project), or the image a VALIDATE looks at."""
    name: str
    kind: OpKind
    consumes: ArtifactKind
    produces: Optional[ArtifactKind] = None
    reads: Optional[str] = None
    label: str = ""

    @property
    def input(self) -> str:               # human label shown in the UI
        return self.reads or self.label


_PG, _RG, _TB, _TY = (ArtifactKind.PAGE, ArtifactKind.REGION, ArtifactKind.TABLE, ArtifactKind.TYPED)
OPS: dict[str, Operation] = {o.name: o for o in [
    # layout: Page -> Region(s)
    Operation("find_tables", OpKind.LAYOUT, _PG, _RG, label="geometric (ruled + text-aligned)"),
    Operation("yolo26",      OpKind.LAYOUT, _PG, _RG, label="YOLO26 learned layout"),
    Operation("doclayout",   OpKind.LAYOUT, _PG, _RG, label="DocLayout-YOLO"),
    Operation("docling",     OpKind.LAYOUT, _PG, _RG, label="Docling layout model"),
    Operation("surya",       OpKind.LAYOUT, _PG, _RG, label="Surya (VLM layout + reading order)"),
    # extract: Region -> Table  (reparse one source representation, scoped to the region)
    Operation("cheap",       OpKind.EXTRACT, _RG, _TB, reads="PDF text-layer (glyph boxes)"),
    Operation("Docling",     OpKind.EXTRACT, _RG, _TB, reads="PDF (direct, per page)"),
    # extract: Region -> TextGrid  (the raw region text; columns are committed later)
    Operation("text-table",  OpKind.EXTRACT, _RG, ArtifactKind.TEXTGRID, reads="region text (layout-preserving)"),
    # structure: TextGrid -> Table  (cluster the region's words into columns)
    Operation("structure",   OpKind.TRANSFORM, ArtifactKind.TEXTGRID, _TB, label="cluster words into columns → table"),
    # transform: Table -> Table / Typed  (consume the artifact's own content)
    Operation("markdown",    OpKind.TRANSFORM, _TB, _TB, label="grid → markdown → table"),
    Operation("sign-fix",    OpKind.TRANSFORM, _TB, _TB, label="reinterpret signs (parens / CR / DR → signed)"),
    Operation("materialize", OpKind.TRANSFORM, _TB, _TY, label="HtmlTable → typed columns"),
    # merge: [Region…] -> Region  (agree on a bbox across layout models; runs client-side)
    # validate: Table -> (no artifact; attaches a verdict to the table's evidence)
    Operation("vision",      OpKind.VALIDATE, _TB, None, reads="rendered region image"),
]}


@dataclass
class OpResult:
    """The artifact an operation produces — the invariant payload every /api/parse
    response carries. `kind` is the producing op's OpKind; the evidence lives here
    on the artifact (status/signals/recon), uniformly (validation is not a node)."""
    method: str
    kind: str                                   # OpKind value of the producing op
    input: str                                  # what it read / how it derived
    status: Status
    seconds: float = 0.0
    dollars: float = 0.0
    impression: Optional[str] = None
    signals: list[dict] = field(default_factory=list)
    html: Optional[str] = None
    recon: Optional[float] = None               # reconstruction error (evidence)
    detail: Optional[str] = None                # recon diff image (base64)
    markdown: Optional[str] = None
    text: Optional[str] = None                   # raw text-grid content (layout-preserving)
    produces: Optional[str] = None              # ArtifactKind this op made (table/text-grid/typed)
    note: Optional[str] = None
    next: list[dict] = field(default_factory=list)


def _artifact(op: Operation, ev: dict, html: str, secs: float,
              recon: Optional[float] = None, detail: Optional[str] = None,
              markdown: Optional[str] = None, note: Optional[str] = None) -> OpResult:
    """Build a produced TABLE artifact from a validator's evidence."""
    return OpResult(method=op.name, kind=op.kind.value, input=op.input,
                    status=status_of(ev), impression=ev.get("impression"),
                    signals=ev.get("signals", []), html=html, recon=recon, detail=detail,
                    markdown=markdown, note=note, seconds=round(secs, 3),
                    produces=op.produces.value if op.produces else "table")


def _missing(op: Operation, secs: float, note: Optional[str]) -> OpResult:
    return OpResult(method=op.name, kind=op.kind.value, input=op.input,
                    status="missing", seconds=round(secs, 3), note=note)


def grid_from_html(html):
    grid, _ = rv.parse_html_grid(html or "")
    return [[c for c in row] for row in grid] if grid else None


def sign_fix_grid(grid):
    """Transform: rewrite accounting sign conventions to signed numbers."""
    changed, out = 0, []
    for row in grid:
        r = []
        for c in row:
            t = c.strip(); new = c
            if t.startswith("(") and t.endswith(")") and any(ch.isdigit() for ch in t):
                new = "-" + t[1:-1].strip()
            elif t[-2:].lower() == "cr" and any(ch.isdigit() for ch in t):
                new = "-" + t[:-2].strip()
            elif t[-2:].lower() == "dr" and any(ch.isdigit() for ch in t):
                new = t[:-2].strip()
            elif t.endswith("-") and any(ch.isdigit() for ch in t[:-1]):
                new = "-" + t[:-1].strip()
            if new != c:
                changed += 1
            r.append(new)
        out.append(r)
    return out, changed


_RANK = {"confirmed": 3, "no_issues": 2, "suspect": 1, "figure": 0}
_RANKNAME = {3: "reconciles", 2: "clean", 1: "suspect", 0: "figure"}


def _grid_quality(grid, page):
    """Score a grid's evidence so a transform can prove it helped. Higher is better,
    compared lexicographically: (reconciliation, negatives captured, numbers parsed,
    -violations). 'Negatives captured' is the signal that catches the split-paren
    fix — rejoining '( 902 )' flips a phantom +902 into -902, which is the whole
    point — and reconciliation catches whether the totals then add up."""
    ev = explain_grid(grid, page)
    rank = _RANK.get(ev.get("impression"), 1)
    negs = ncells = viol = 0
    try:
        t = typ.materialize([[(c or "") for c in row] for row in grid], 1)
        viol = len(t.violations)
        for col in t.columns:
            if col.dtype == "label":
                continue
            for v in col.values:
                if v is not None:
                    ncells += 1
                    if isinstance(v, (int, float)) and v < 0:
                        negs += 1
    except Exception:  # noqa: BLE001
        viol = 999
    return (rank, negs, ncells, -viol), ev


def _delta_note(base, new):
    parts = []
    if new[0] != base[0]:
        parts.append(f"reconciliation {_RANKNAME[base[0]]}→{_RANKNAME[new[0]]}")
    if new[1] != base[1]:
        parts.append(f"{new[1]-base[1]:+d} negative(s) captured")
    if new[3] != base[3]:
        parts.append(f"violations {-base[3]}→{-new[3]}")
    return ", ".join(parts)


def route(r: OpResult, tried: list[str]) -> list[dict]:
    """Diagnostic-driven next operations: ops whose precondition matches the
    failure, in priority order (a cheap targeted transform before a costly
    re-parse). This policy replaces the linear ladder — escalation is best-first
    over the artifact graph. `tried` is the ancestor op lineage (no repeats)."""
    sigs = " ".join(s.get("detail", "") for s in r.signals)
    status, html, method = r.status, (r.html or ""), r.method
    cands: list[dict] = []

    def add(op: str, reason: str) -> None:
        if op != method and op not in tried and all(c["op"] != op for c in cands):
            cands.append({"op": op, "reason": reason, "kind": OPS[op].kind.value})

    if status in ("ok", "confirmed", "verified"):
        add("materialize", "validated — materialize to typed, math-ready columns")
        return cands
    if "figure" in sigs:
        add("vision", "looks like a chart/figure — confirm with vision")
    if "no column reconciles" in sigs or "rows sum to" in sigs:
        if "CR" in html or "DR" in html:
            add("sign-fix", "totals fail and CR/DR markers present — reinterpret signs")
        add("Docling", "totals don't reconcile — re-parse structure from the PDF")
    if "no column headers" in sigs:
        add("Docling", "header row looks like data — re-parse from the PDF")
    if "non-numeric" in sigs:
        add("Docling", "stray text in a numeric column — re-parse from the PDF")
    if status == "missing":
        for op in ("text-table", "markdown", "Docling"):
            add(op, "not found by this method — try " + OPS[op].input)
    # generic fallbacks — always leave a way forward, cheapest last-resort first
    add("Docling", "escalate to a structure-aware parser")
    add("vision", "escalate to vision verification")
    add("markdown", "re-express the grid as markdown and re-detect")
    return cands[:4]


@app.post("/api/parse")
@locked
def api_parse():
    """Apply one operation to a Region/Table, returning the produced artifact.
    Every branch builds the same typed OpResult (the scheme's invariant)."""
    d = request.get_json()
    name, page, bbox, method = d["name"], d["page"], tuple(d["bbox"]), d["method"]
    parent_html = d.get("parent_html")
    op = OPS[method]

    if method == "cheap":  # EXTRACT: parse a grid from the text inside the region's bbox
        t0 = time.monotonic()
        grid = crop_table(name, page, bbox)
        if grid and len(grid) >= 2:
            html = tt.to_html(grid)
            err, png = recon_for(name, page, bbox, html)
            r = _artifact(op, explain_grid(grid, page), html, time.monotonic()-t0, recon=err, detail=png)
        else:
            r = _missing(op, time.monotonic()-t0, "no table grid found in this region")

    elif method == "text-table":  # EXTRACT: the region's raw text as a text-grid (no columns yet)
        t0 = time.monotonic()
        pg = pdf(name).pages[page-1]
        x0, top, x1, bottom = bbox
        cr = pg.crop((max(0, x0), max(0, top), min(pg.width, x1), min(pg.height, bottom)))
        text = cr.extract_text(layout=True) or ""
        if text.strip():
            r = OpResult(method=op.name, kind=op.kind.value, input=op.input, produces="text-grid",
                         status="grid", text=text, seconds=round(time.monotonic()-t0, 3))
        else:
            r = _missing(op, time.monotonic()-t0, "no text found in this region")

    elif method == "structure":  # TRANSFORM: cluster the text-grid's words into columns -> table
        t0 = time.monotonic()
        pg = pdf(name).pages[page-1]
        x0, top, x1, bottom = bbox
        cr = pg.crop((max(0, x0), max(0, top), min(pg.width, x1), min(pg.height, bottom)))
        grid, hdr = tt.structure_words(cr.extract_words())
        if grid and len(grid) >= 2:
            html = tt.to_html_headed(grid, hdr)
            err, png = recon_for(name, page, bbox, html)
            note = f"{hdr} header row(s) collapsed into the column index" if hdr > 1 else None
            r = _artifact(op, explain_grid(grid, page, hdr), html, time.monotonic()-t0, recon=err, detail=png, note=note)
        else:
            r = _missing(op, time.monotonic()-t0, "could not cluster the text grid into columns")

    elif method == "markdown":  # TRANSFORM: parent grid -> markdown -> re-parse -> validate
        t0 = time.monotonic()
        src = grid_from_html(parent_html) if parent_html else lite_best_grid(name, page, bbox)[0]
        if src:
            md = tt.to_markdown(src)
            reparsed = tt.detect_tables(md)
            grid = reparsed[0] if reparsed else src
            r = _artifact(op, explain_grid(grid, page), tt.to_html(grid), time.monotonic()-t0, markdown=md)
        else:
            r = _missing(op, time.monotonic()-t0, "no parent grid to re-express as markdown")

    elif method == "sign-fix":  # TRANSFORM: rewrite the parent grid's accounting signs
        t0 = time.monotonic()
        src = grid_from_html(parent_html)
        if src:
            grid, changed = sign_fix_grid(src)
            base_q, _ = _grid_quality(src, page)
            new_q, ev = _grid_quality(grid, page)
            delta = _delta_note(base_q, new_q)
            ev = dict(ev)
            ev["signals"] = list(ev.get("signals", [])) + [
                {"positive": new_q > base_q, "detail": "sign-fix " + (delta if delta else "did not change the evidence")}]
            note = f"rewrote {changed} parenthesised/CR/DR value(s) to signed numbers" + (" — " + delta if delta else "")
            r = _artifact(op, ev, tt.to_html(grid), time.monotonic()-t0, note=note)
        else:
            r = _missing(op, time.monotonic()-t0, "no parent grid to reinterpret")

    elif method == "Docling":  # EXTRACT: match the region to Docling's per-page tables
        dl = docling_page(name, page)
        t = max((t for t in dl["tables"] if t["page"] == page), key=lambda t: iou(bbox, t["bbox"]), default=None)
        if t is None:
            r = _missing(op, dl["secs"], "Docling found no table at this region")
        else:
            err, png = recon_for(name, page, bbox, t["html"])
            r = _artifact(op, t["ev"], t["html"], dl["secs"], recon=err, detail=png)

    elif method == "vision":  # VALIDATE: an evidence patch merged onto the table (no new artifact)
        r = OpResult(method=op.name, kind=op.kind.value, input=op.input, status="verified",
                     seconds=VISION_TIME, dollars=VISION_RATE,
                     signals=[{"positive": True, "detail": "vision-verified the parse (modeled)"}],
                     note="LLM vision-verifies the parse / confirms it is a figure (modeled)")

    else:
        return jsonify({"error": f"unknown operation {method!r}"}), 400

    if r.status == "grid":  # a text-grid's only next step is to commit columns
        r.next = [{"op": "structure", "reason": OPS["structure"].label, "kind": "transform"}]
    else:
        r.next = route(r, d.get("tried", []))
    return jsonify(asdict(r))


_SQL = {"int": "BIGINT", "float": "DOUBLE", "percent": "DOUBLE",
        "currency": "DOUBLE", "label": "VARCHAR"}


def _typed_payload(t):
    """Serialize a TypedTable into the column/row/DDL preview the UI renders."""
    from collections import Counter
    cols = []
    for c in t.columns:
        tf = Counter(x for cell in c.cells for x in cell.transforms)
        cols.append({"name": c.name, "dtype": c.dtype, "sql": _SQL[c.dtype],
                     "levels": c.levels, "nulls": sum(1 for v in c.values if v is None),
                     "transforms": dict(tf)})
    rows = [[(c.values[r] if r < len(c.values) else None) for c in t.columns]
            for r in range(min(t.n_rows, 60))]
    ddl = ("CREATE TABLE t (\n  "
           + ",\n  ".join(f'"{c["name"]}" {c["sql"]}' for c in cols) + "\n);")
    return {"columns": cols, "rows": rows, "violations": t.violations,
            "n_rows": t.n_rows, "ddl": ddl}


@app.post("/api/materialize")
def api_materialize():
    """The non-reversible HtmlTable -> TypedTable step: materialize the parsed
    HTML into typed, math-ready columns (negatives, scale, %, currency resolved)
    with per-cell provenance. Auto-promotes nested section headers to a column, and
    also returns the tidy/long (melted) shape. Includes the DuckDB DDL each implies."""
    d = request.get_json()
    html = d.get("html") or ""
    try:
        grid, hdr = rv.parse_html_grid(html)
        hr = (max(hdr) + 1) if hdr else 1
        grid, hr, sectioned = typ.section_to_column(grid, hr)
        t = typ.materialize(grid, hr)
    except Exception as e:  # noqa: BLE001
        return jsonify({"error": str(e)})
    out = _typed_payload(t)
    out["sections"] = sectioned
    out["tidy"] = _typed_payload(t.melt())
    return jsonify(out)


STATIC = os.path.join(os.path.dirname(os.path.abspath(__file__)), "static")


@app.get("/")
def index():
    return Response(open(os.path.join(STATIC, "app.html"), encoding="utf-8").read(), mimetype="text/html")


def warmup():
    """Load the Docling model at startup so the first escalation is fast. We
    convert one page and DISCARD it (don't cache), so real on-demand clicks still
    measure the per-page time with the model already warm."""
    refresh_docs()
    if not DOCS:
        print("  no PDFs found under input/ — drop some in and reload.", flush=True)
        return
    print("Loading Docling model (one-time)...", flush=True)
    t = time.monotonic()
    try:
        _converter().convert(next(iter(DOCS.values())), page_range=(1, 1))
        print(f"  Docling ready in {time.monotonic()-t:.0f}s.", flush=True)
    except Exception as e:  # noqa: BLE001
        print(f"  Docling warmup skipped: {str(e)[:80]}", flush=True)
    # Pre-download/load the YOLO layout models so a first-use download doesn't
    # stall the (now-serialized) request pipeline.
    try:
        import yolo_layout as yl
        for key in ("yolo26", "doclayout"):
            t = time.monotonic(); yl._load(key)
            print(f"  layout '{key}' ready in {time.monotonic()-t:.0f}s.", flush=True)
    except Exception as e:  # noqa: BLE001
        print(f"  YOLO layout warmup skipped: {str(e)[:80]}", flush=True)


if __name__ == "__main__":
    import socket
    port = int(os.environ.get("PORT", 5050))  # avoid 5000 (macOS AirPlay -> 403)
    # Fail loudly if the port is already taken — a stale old server still bound to
    # it is exactly how you end up hitting outdated code after a "restart".
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    if probe.connect_ex(("127.0.0.1", port)) == 0:
        probe.close()
        print(f"\n*** PORT {port} IS ALREADY IN USE ***", flush=True)
        print(f"An old trajectory_server is probably still running and serving STALE code.", flush=True)
        print(f"Kill it first:  pkill -f trajectory_server   (or use a different PORT=…)\n", flush=True)
        sys.exit(1)
    probe.close()
    # pkill sends SIGTERM, which atexit doesn't catch — so stop the surya sidecar
    # (and its llama-server) explicitly on signals too.
    for _sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(_sig, lambda *_: (_stop_surya(), sys.exit(0)))
    print(f"trajectory_server build={BUILD}", flush=True)
    warmup()
    print(f"open http://127.0.0.1:{port}   (build {BUILD})", flush=True)
    app.run(host="127.0.0.1", port=port, debug=False)
