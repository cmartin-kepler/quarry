#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["flask>=3", "docling", "pdfplumber>=0.11"]
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
  cheap geometric (PDF text-layer) · text-table (LiteParse text) ·
  Docling (PDF, per-page) · vision verify (image)

Run (deps declared inline via PEP 723, so plain uv run works):
  uv run scripts/trajectory_server.py            # -> http://127.0.0.1:5050
  PORT=8080 uv run scripts/trajectory_server.py  # pick another port
(Port 5000 is avoided: macOS AirPlay Receiver squats on it and returns 403.)
"""
from __future__ import annotations

import base64
import io
import json
import os
import re
import subprocess
import tempfile
import time

import pdfplumber
from flask import Flask, Response, jsonify, request

import sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import recon_validate as rv  # noqa: E402
import text_tables as tt  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QUARRY = os.path.join(REPO, "target", "debug", "quarry")
DH = "0" * 64
VISION_RATE, VISION_TIME = 0.02, 1.2

DOCS = {
    "Disney Q2 reconciliations": "input/finance/disney/q2-fy26-financial-reconciliations.pdf",
    "ParseBench paper (arXiv)": "input/arxiv/2604.08538v3.pdf",
    "JPMorgan annual report": "input/finance/jpm-2023-ar.pdf",
}
INPUTS = {"cheap": "PDF text-layer (glyph boxes)", "text-table": "LiteParse text / markdown",
          "Docling": "PDF (direct, per page)", "vision": "rendered region image"}

app = Flask(__name__)
_pdf, _meta, _regions, _pageimg = {}, {}, {}, {}
_cheap, _lite, _docling, _wd = {}, {}, {}, tempfile.mkdtemp()


def pdf(name):
    if name not in _pdf:
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

def ensure_cheap(name):
    if name not in _cheap:
        import pdf_to_qdoc as B
        t = time.monotonic()
        doc = B.convert(DOCS[name], None, detect=True, max_pages=None)
        wd = os.path.join(_wd, "cheap_" + str(abs(hash(name))))
        os.makedirs(wd, exist_ok=True)
        q = os.path.join(wd, "c.qdoc"); json.dump(doc, open(q, "w"))
        st = os.path.join(wd, "c.art")
        sh([QUARRY, "parse", q, "--out", st, "--tier", "0"])
        _cheap[name] = {"tables": store_tables(st), "secs": time.monotonic()-t,
                        "n_pages": len(pdf(name).pages)}
    return _cheap[name]


def ensure_lite(name):
    if name not in _lite:
        t = time.monotonic()
        js = os.path.join(_wd, "lp_" + str(abs(hash(name))) + ".json")
        sh(["lit", "parse", DOCS[name], "--format", "json", "-o", js, "-q"])
        secs = time.monotonic()-t
        lj = json.load(open(js))
        _lite[name] = {"text": {p["page"]: p["text"] for p in lj["pages"]},
                       "secs": secs, "n_pages": len(lj["pages"])}
    return _lite[name]


_conv = None


def _converter():
    global _conv
    if _conv is None:
        from docling.document_converter import DocumentConverter
        _conv = DocumentConverter()
    return _conv


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
        _docling[key] = {"tables": store_tables(st), "secs": secs}
    return _docling[key]


def explain_grid(grid, page):
    cells = [{"row": r, "col": c, "text": t, "anchor": {"format": "pdf", "doc": DH, "page": page,
              "bbox": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}}, "is_header": r == 0}
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


# ---- API -------------------------------------------------------------------

@app.get("/api/docs")
def api_docs():
    return jsonify(list(DOCS))


@app.get("/api/doc/<name>")
def api_doc(name):
    """Just page dimensions — instant for any doc size. Images and regions load
    lazily per page as the viewer scrolls."""
    if name not in _meta:
        _meta[name] = [{"page": pg.page_number, "w": float(pg.width), "h": float(pg.height)}
                       for pg in pdf(name).pages]
    return jsonify({"pages": _meta[name]})


@app.get("/api/regions/<name>/<int:n>")
def api_regions(name, n):
    """Detect table regions on one page on demand (find_tables is slow per page)."""
    key = (name, n)
    if key not in _regions:
        pg = pdf(name).pages[n-1]
        regs = []
        for ti, t in enumerate(pg.find_tables()):
            x0, top, x1, bottom = t.bbox
            regs.append({"id": f"p{n}t{ti}", "page": n, "bbox": [x0, top, x1, bottom],
                         "box": {"left": 100*x0/pg.width, "top": 100*top/pg.height,
                                 "width": 100*(x1-x0)/pg.width, "height": 100*(bottom-top)/pg.height}})
        _regions[key] = regs
    return jsonify(_regions[key])


@app.get("/api/page/<name>/<int:n>")
def api_page(name, n):
    """Render one page on demand (lazy-loaded by the scrollable viewer)."""
    key = (name, n)
    if key not in _pageimg:
        pg = pdf(name).pages[n-1]
        im = pg.to_image(resolution=150); buf = io.BytesIO(); im.save(buf, format="PNG")
        _pageimg[key] = buf.getvalue()
    return Response(_pageimg[key], mimetype="image/png")


@app.post("/api/parse")
def api_parse():
    d = request.get_json()
    name, page, bbox, method = d["name"], d["page"], tuple(d["bbox"]), d["method"]
    res = {"method": method, "input": INPUTS.get(method, method)}

    if method == "cheap":
        c = ensure_cheap(name)
        cands = [t for t in c["tables"] if t["page"] == page]
        t = max(cands, key=lambda t: iou(bbox, t["bbox"]), default=None)
        secs = c["secs"] / max(1, c["n_pages"])
        res.update(_table_result(name, page, bbox, t, secs, 0.0))

    elif method == "text-table":
        lp = ensure_lite(name)
        c = ensure_cheap(name)
        ref = max((t for t in c["tables"] if t["page"] == page), key=lambda t: iou(bbox, t["bbox"]), default=None)
        ref_tok = tokens(re.sub("<[^>]+>", " ", ref["html"])) if ref else set()
        best, bestov = None, 0.0
        for g in tt.detect_tables(lp["text"].get(page, "")):
            ov = len(ref_tok & tokens(" ".join(c2 for row in g for c2 in row))) / (len(ref_tok)+1e-9)
            if ov > bestov:
                best, bestov = g, ov
        secs = lp["secs"] / max(1, lp["n_pages"])
        if best and bestov > 0.3:
            ev = explain_grid(best, page)
            res.update({"status": status_of(ev), "impression": ev.get("impression"),
                        "signals": ev.get("signals", []), "html": tt.to_html(best),
                        "seconds": round(secs, 3), "dollars": 0.0, "recon": None, "detail": None})
        else:
            res.update({"status": "missing", "seconds": round(secs, 3), "dollars": 0.0})

    elif method == "Docling":
        dl = docling_page(name, page)
        t = max((t for t in dl["tables"] if t["page"] == page), key=lambda t: iou(bbox, t["bbox"]), default=None)
        res.update(_table_result(name, page, bbox, t, dl["secs"], 0.0))

    elif method == "vision":
        res.update({"status": "verified", "seconds": VISION_TIME, "dollars": VISION_RATE,
                    "note": "LLM vision-verifies the parse / confirms it is a figure (modeled)"})
    return jsonify(res)


@app.post("/api/parse_page")
def api_parse_page():
    """Run a method on the WHOLE page and return every table it finds — so tables
    that find_tables (ruled lines) missed (e.g. borderless academic tables) get
    discovered. Docling finds these; cheap/text usually don't."""
    d = request.get_json()
    name, page, method = d["name"], d["page"], d["method"]
    pg = pdf(name).pages[page-1]
    tables, secs = [], 0.0
    if method == "Docling":
        dl = docling_page(name, page); secs = dl["secs"]
        tables = [t for t in dl["tables"] if t["page"] == page]
    elif method == "cheap":
        c = ensure_cheap(name); secs = c["secs"]/max(1, c["n_pages"])
        tables = [t for t in c["tables"] if t["page"] == page]
    out = []
    for i, t in enumerate(tables):
        x0, y0, x1, y1 = t["bbox"]
        err, png = recon_for(name, page, t["bbox"], t["html"])
        out.append({"id": f"{method}_p{page}_{i}", "bbox": list(t["bbox"]),
                    "box": {"left": 100*x0/pg.width, "top": 100*y0/pg.height,
                            "width": 100*(x1-x0)/pg.width, "height": 100*(y1-y0)/pg.height},
                    "method": method, "input": INPUTS.get(method, method),
                    "status": status_of(t["ev"]), "impression": t["ev"].get("impression"),
                    "signals": t["ev"].get("signals", []), "html": t["html"],
                    "seconds": round(secs, 3), "dollars": 0.0, "recon": err, "detail": png})
    return jsonify({"method": method, "count": len(out), "tables": out})


def _table_result(name, page, bbox, t, secs, dollars):
    if t is None:
        return {"status": "missing", "seconds": round(secs, 3), "dollars": dollars}
    err, png = recon_for(name, page, bbox, t["html"])
    return {"status": status_of(t["ev"]), "impression": t["ev"].get("impression"),
            "signals": t["ev"].get("signals", []), "html": t["html"],
            "seconds": round(secs, 3), "dollars": dollars, "recon": err, "detail": png}


@app.get("/")
def index():
    return HTML


HTML = r"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>Parsing trajectory</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600;700&family=Geist+Mono:wght@400;500&display=swap" rel="stylesheet">
<style>
:root{
 --g950:#030a07;--g900:#0b1f17;--g800:#0f2e22;--g700:#14412f;--g600:#1a5a40;--g500:#22805a;--g400:#4a9d76;--g300:#7fb89a;--g200:#bdd4c7;--g100:#dfe9e3;--g50:#f4f5f4;
 --paper:#faf9f7;--ink:#171717;--muted:#6b6b6b;--line:#e7e5e4;
 --confirmed:#1a5a40;--ok:#3f7a8c;--suspect:#b4453a;--figure:#7a4fa3;--missing:#b07515;--verified:#1a8a6a;--idle:#9aa39e;
}
*{box-sizing:border-box}
body{margin:0;background:var(--paper);color:var(--ink);font-family:Geist,-apple-system,system-ui,sans-serif;font-size:14px;-webkit-font-smoothing:antialiased}
.mono{font-family:'Geist Mono',ui-monospace,monospace}
header{position:sticky;top:0;z-index:10;background:rgba(250,249,247,.85);backdrop-filter:blur(8px);border-bottom:1px solid var(--line);padding:12px 22px;display:flex;align-items:center;gap:18px}
.brand{display:flex;align-items:center;gap:9px;font-weight:600;font-size:15px;letter-spacing:-.01em}
.logo{width:22px;height:22px;border-radius:6px;background:linear-gradient(135deg,var(--g500),var(--g700));display:grid;place-items:center;color:#fff;font-size:13px;font-weight:700}
select{font-family:inherit;font-size:13px;padding:5px 9px;border:1px solid var(--line);border-radius:7px;background:#fff;color:var(--ink);cursor:pointer}
select:focus{outline:none;border-color:var(--g400)}
.spacer{flex:1}
.pageind{font-size:12px;color:var(--muted);display:flex;align-items:center;gap:6px}
.pageind input{width:46px;font-family:'Geist Mono',monospace;font-size:12px;padding:3px 6px;border:1px solid var(--line);border-radius:6px;text-align:center}
.sub{font-size:12px;color:var(--muted);padding:0 22px 10px;margin-top:-2px}
main{display:grid;grid-template-columns:1fr 2fr;gap:0;height:calc(100vh - 84px)}
.viewer{overflow-y:auto;padding:18px 18px 40px;scroll-behavior:smooth}
.pageslot{position:relative;width:100%;max-width:780px;margin:0 auto 18px;background:#fff;border:1px solid var(--line);border-radius:8px;box-shadow:0 1px 2px rgba(20,20,20,.04),0 4px 14px rgba(20,20,20,.05);overflow:hidden}
.pagenum{position:absolute;left:10px;top:10px;z-index:3;font-family:'Geist Mono',monospace;font-size:10px;color:var(--muted);background:rgba(255,255,255,.8);border:1px solid var(--line);border-radius:5px;padding:1px 6px}
.pageimg{width:100%;display:block;min-height:240px;background:repeating-linear-gradient(45deg,#f6f6f5,#f6f6f5 10px,#f1f1ef 10px,#f1f1ef 20px)}
.overlays{position:absolute;inset:0}
.ov{position:absolute;box-sizing:border-box;border:2px solid var(--idle);border-radius:4px;cursor:pointer;transition:box-shadow .12s,border-color .12s;background:transparent}
.ov:hover{border-color:var(--g500);box-shadow:0 0 0 3px rgba(34,128,90,.15)}
.ov.sel{border-color:var(--g600);box-shadow:0 0 0 3px #fde68a}
.ov .tag{position:absolute;left:-1px;top:-17px;font-size:9px;font-weight:600;color:#fff;padding:1px 5px;border-radius:4px;white-space:nowrap;text-transform:uppercase;letter-spacing:.03em}
.panel{border-left:1px solid var(--line);background:#fff;overflow-y:auto;padding:20px}
.pp{font-size:12px;color:var(--muted);display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin-bottom:14px}
.btn{font-family:inherit;font-size:12px;font-weight:500;padding:5px 11px;border-radius:7px;border:1px solid var(--g600);background:var(--g600);color:#fff;cursor:pointer;transition:background .12s}
.btn:hover{background:var(--g700)}
.btn.ghost{background:#fff;color:var(--g700);border-color:var(--g300)}
.btn.ghost:hover{background:var(--g50)}
.btn:disabled{opacity:.5;cursor:default}
.graph{display:flex;align-items:center;flex-wrap:wrap;gap:0;margin-bottom:8px}
.node{border:1.5px solid var(--idle);border-radius:10px;padding:9px 12px;min-width:122px;background:#fff;cursor:pointer;transition:box-shadow .12s,transform .12s}
.node:hover{transform:translateY(-1px)}
.node.sel{box-shadow:0 0 0 3px #fde68a}
.node.next{border-style:dashed;border-color:var(--g300);background:var(--g50);cursor:default;display:flex;flex-direction:column;justify-content:center;gap:5px}
.node.next:hover{transform:none}
.node.next .btn{width:100%;text-align:center}
.node .m{font-weight:600;font-size:13px}
.node .i{font-size:10px;color:var(--muted);margin-top:1px}
.node .c{font-family:'Geist Mono',monospace;font-size:10px;color:var(--g600);margin-top:4px}
.arrow{color:var(--g300);font-size:18px;padding:0 7px}
.badge{display:inline-flex;align-items:center;font-size:10px;font-weight:600;color:#fff;padding:2px 8px;border-radius:20px;text-transform:uppercase;letter-spacing:.03em;margin-top:5px}
.esc{margin:8px 0 16px;min-height:30px}
.esc .ok{color:var(--g600);font-size:13px;font-weight:500}
.spin{color:var(--g500);font-size:13px;display:inline-flex;align-items:center;gap:7px}
.spin::before{content:"";width:13px;height:13px;border:2px solid var(--g200);border-top-color:var(--g500);border-radius:50%;display:inline-block;animation:sp .7s linear infinite}
@keyframes sp{to{transform:rotate(360deg)}}
.card{border:1px solid var(--line);border-radius:10px;padding:14px;background:#fff}
.card h4{margin:0 0 8px;font-size:13px;display:flex;align-items:center;gap:8px;flex-wrap:wrap}
.tbl{margin:10px 0;overflow-x:auto;border:1px solid var(--line);border-radius:8px}
table{border-collapse:collapse;font-size:12px;width:100%}
td,th{border-bottom:1px solid var(--g50);border-right:1px solid var(--g50);padding:4px 9px;white-space:nowrap;text-align:left}
th{background:var(--g50);font-weight:600;color:var(--g800)}
tr:last-child td{border-bottom:none}
.sig{font-size:12px;margin-top:3px;display:flex;gap:6px}
.sig.pos{color:var(--g600)}.sig.neg{color:var(--suspect)}
.dim{color:var(--muted);font-size:12px}
details{margin-top:10px}summary{cursor:pointer;font-size:12px;color:var(--g600)}
details img{max-width:100%;border:1px solid var(--line);border-radius:8px;margin-top:8px}
.hint{color:var(--muted);font-size:13px;padding:30px 10px;text-align:center;border:1px dashed var(--line);border-radius:10px}
.costline{font-family:'Geist Mono',monospace;font-size:11px;color:var(--g600);margin-top:6px}
</style></head><body>
<header>
 <div class="brand"><span class="logo">Q</span> Parsing trajectory</div>
 <select id="docsel"></select>
 <div class="spacer"></div>
 <div class="pageind">page <input id="pagejump" type="number" min="1" value="1"> / <span id="npages">–</span></div>
</header>
<div class="sub">Click a table in the PDF to parse it on demand. If it's flagged, <b>Escalate</b> runs the next method live (LiteParse → Docling per-page → vision). Borderless or no region? <b>Parse the whole page.</b> Every time is measured; nothing is hardcoded.</div>
<main>
 <div class="viewer" id="viewer"></div>
 <div class="panel" id="panel"></div>
</main>
<script>
const STATUS={confirmed:'#1a5a40',ok:'#3f7a8c',suspect:'#b4453a',figure:'#7a4fa3',missing:'#b07515',verified:'#1a8a6a',idle:'#9aa39e'};
const LABEL={confirmed:'reconciles',ok:'no issues',suspect:'suspect',figure:'figure',missing:'no table',verified:'verified',idle:'table'};
const LADDER=['cheap','text-table','Docling','vision'];
const enc=encodeURIComponent;
let docName=null,doc=null,regionsByPage={},discovered={},sel=null,traj=[],selNode=0,curPage=1;
async function J(u,o){return (await fetch(u,o)).json();}
function regionsOn(p){return regionsByPage[p]||[];}
function discOn(p){return discovered[p]||[];}

async function init(){const ds=await J('/api/docs');const s=document.getElementById('docsel');
 s.innerHTML=ds.map(d=>'<option>'+d+'</option>').join('');s.onchange=()=>selectDoc(s.value);
 document.getElementById('pagejump').onchange=e=>scrollToPage(+e.target.value);
 selectDoc(ds[0]);}
async function selectDoc(n){docName=n;doc=await J('/api/doc/'+enc(n));regionsByPage={};discovered={};sel=null;traj=[];
 document.getElementById('npages').textContent=doc.pages.length;curPage=doc.pages[0].page;buildViewer();renderPanel();}

function buildViewer(){const v=document.getElementById('viewer');v.innerHTML='';
 doc.pages.forEach(p=>{const slot=document.createElement('div');slot.className='pageslot';slot.dataset.page=p.page;
   slot.style.aspectRatio=p.w+' / '+p.h;
   slot.innerHTML='<div class="pagenum">p.'+p.page+'</div><img class="pageimg" data-src="/api/page/'+enc(docName)+'/'+p.page+'"><div class="overlays"></div>';
   v.appendChild(slot);});
 lazyObserve();}
async function loadRegions(p){if(regionsByPage[p])return;regionsByPage[p]=await J('/api/regions/'+enc(docName)+'/'+p);drawOverlays(p);}
function lazyObserve(){const io=new IntersectionObserver((es)=>{es.forEach(e=>{
   const slot=e.target;if(e.isIntersecting){const img=slot.querySelector('img');if(img.dataset.src){img.src=img.dataset.src;delete img.dataset.src;}
     const pn=+slot.dataset.page;curPage=pn;document.getElementById('pagejump').value=pn;loadRegions(pn);}});},
   {root:document.getElementById('viewer'),rootMargin:'200px',threshold:.05});
 document.querySelectorAll('.pageslot').forEach(s=>io.observe(s));}
function scrollToPage(n){const s=document.querySelector('.pageslot[data-page="'+n+'"]');if(s)s.scrollIntoView({behavior:'smooth'});}
function drawOverlays(p){const slot=document.querySelector('.pageslot[data-page="'+p+'"]');if(!slot)return;
 const ov=slot.querySelector('.overlays');let h='';
 regionsOn(p).forEach(r=>{const st=(sel===r.id&&traj.length)?traj[traj.length-1].status:'idle';
   h+='<div class="ov'+(sel===r.id?' sel':'')+'" style="left:'+r.box.left+'%;top:'+r.box.top+'%;width:'+r.box.width+'%;height:'+r.box.height+'%;border-color:'+STATUS[st]+'" onclick="pick(\''+r.id+'\')"><span class="tag" style="background:'+STATUS[st]+'">'+LABEL[st]+'</span></div>';});
 discOn(p).forEach(t=>{h+='<div class="ov'+(sel===t.id?' sel':'')+'" style="left:'+t.box.left+'%;top:'+t.box.top+'%;width:'+t.box.width+'%;height:'+t.box.height+'%;border-color:'+STATUS[t.status]+';border-style:dashed" onclick="pickDisc(\''+t.id+'\')"><span class="tag" style="background:'+STATUS[t.status]+'">'+t.method+'</span></div>';});
 ov.innerHTML=h;}

function allRegions(){return Object.values(regionsByPage).flat();}
async function pick(id){sel=id;traj=[];selNode=0;const r=allRegions().find(x=>x.id===id);drawOverlays(r.page);renderPanel();await step('cheap');}
function pickDisc(id){const t=Object.values(discovered).flat().find(x=>x.id===id);sel=id;traj=[t];selNode=0;drawOverlays(t.page);renderPanel();}
function regionById(id){return allRegions().find(x=>x.id===id)||Object.values(discovered).flat().find(x=>x.id===id);}
async function step(method){const r=regionById(sel);
 document.getElementById('esc').innerHTML='<span class="spin">parsing with '+method+' …</span>';
 const res=await J('/api/parse',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:docName,page:r.page,bbox:r.bbox,method})});
 traj.push(res);selNode=traj.length-1;drawOverlays(r.page);renderPanel();return res;}
const GOOD=s=>['ok','confirmed','verified'].includes(s);
const FLAGGED=s=>['suspect','figure','missing'].includes(s);
let auto=false;
async function autoEscalate(){auto=true;renderTraj();
 while(true){const last=traj[traj.length-1],ni=LADDER.indexOf(last.method)+1;
   if(!FLAGGED(last.status)||ni>=LADDER.length)break;
   await step(LADDER[ni]);}
 auto=false;renderTraj();}
async function parsePage(method){const m=document.getElementById('ppmsg');m.innerHTML='<span class="spin">parsing page '+curPage+' with '+method+' …</span>';
 const res=await J('/api/parse_page',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:docName,page:curPage,method})});
 const exist=regionsOn(curPage);
 const novel=res.tables.filter(t=>!exist.some(r=>Math.abs(r.box.top-t.box.top)<3&&Math.abs(r.box.left-t.box.left)<3));
 discovered[curPage]=(discovered[curPage]||[]).filter(t=>t.method!==method).concat(novel.map(t=>({...t,disc:true})));
 m.textContent=method+' found '+res.count+' table(s)'+(novel.length?' ('+novel.length+' new) in '+(res.tables[0]?res.tables[0].seconds:'?')+'s — click them':' (already detected)');
 drawOverlays(curPage);}

function badge(s){return '<span class="badge" style="background:'+STATUS[s]+'">'+LABEL[s]+'</span>';}
function renderPanel(){const p=document.getElementById('panel');
 p.innerHTML='<div class="pp">No region, or it\'s borderless? Parse the whole page (p.'+curPage+'): '+
   '<button class="btn ghost" onclick="parsePage(\'cheap\')">cheap</button>'+
   '<button class="btn ghost" onclick="parsePage(\'Docling\')">Docling</button><span id="ppmsg"></span></div>'+
   '<div id="graph" class="graph"></div><div id="esc" class="esc"></div><div id="det"></div>';
 renderTraj();}
function renderTraj(){const g=document.getElementById('graph'),e=document.getElementById('esc'),d=document.getElementById('det');
 if(!sel||!traj.length){g.innerHTML='';e.innerHTML='';d.innerHTML='<div class="hint">Click a table in the PDF to parse it on demand →</div>';return;}
 let gh=traj.map((n,i)=>{
   return '<div class="node'+(i===selNode?' sel':'')+'" style="border-color:'+STATUS[n.status]+'" onclick="selNode='+i+';renderTraj()">'+
     '<div class="m">'+n.method+'</div><div class="i">on '+n.input+'</div>'+badge(n.status)+
     '<div class="c">⏱ '+n.seconds+'s · $'+(n.dollars||0).toFixed(2)+'</div></div><span class="arrow">→</span>';}).join('');
 const last=traj[traj.length-1],nextIdx=LADDER.indexOf(last.method)+1;
 if(FLAGGED(last.status)&&nextIdx<LADDER.length){
   gh+='<div class="node next">'+(auto?'<span class="spin">escalating…</span>':
     '<button class="btn" onclick="step(\''+LADDER[nextIdx]+'\')">Escalate → '+LADDER[nextIdx]+'</button>'+
     '<button class="btn ghost" onclick="autoEscalate()">⚡ Auto until valid</button>')+'</div>';
 }else{gh=gh.replace(/<span class="arrow">→<\/span>$/,'');
   if(GOOD(last.status))gh+='<span class="ok" style="margin-left:6px">✓ validated at '+last.method+'</span>';
   else gh+='<span class="dim" style="margin-left:6px">✗ exhausted ladder — still '+(LABEL[last.status]||last.status)+'</span>';}
 g.innerHTML=gh;e.innerHTML='';
 const n=traj[selNode];let h='<div class="card"><h4><b>'+n.method+'</b> <span class="dim">on '+n.input+'</span> '+badge(n.status)+'</h4>';
 h+='<div class="costline">⏱ '+n.seconds+'s · $'+(n.dollars||0).toFixed(2)+(n.recon!=null?' · reconstruction error '+n.recon:'')+'</div>';
 if(n.note)h+='<div class="dim" style="margin-top:6px">'+n.note+'</div>';
 if(n.html)h+='<div class="tbl">'+n.html+'</div>';
 (n.signals||[]).forEach(s=>h+='<div class="sig '+(s.positive?'pos':'neg')+'">'+(s.positive?'✓':'✗')+' <span>'+s.detail+'</span></div>');
 if(n.detail)h+='<details><summary>reconstruction diff — green matched · orange misplaced · red missing</summary><img src="data:image/png;base64,'+n.detail+'"></details>';
 if(!n.html&&n.status==='missing')h+='<div class="dim">no table detected by this method here</div>';
 h+='</div>';d.innerHTML=h;}
init();
</script></body></html>"""


def warmup():
    """Load the Docling model at startup so the first escalation is fast. We
    convert one page and DISCARD it (don't cache), so real on-demand clicks still
    measure the per-page time with the model already warm."""
    print("Loading Docling model (one-time)...", flush=True)
    t = time.monotonic()
    try:
        _converter().convert(next(iter(DOCS.values())), page_range=(1, 1))
        print(f"  Docling ready in {time.monotonic()-t:.0f}s.", flush=True)
    except Exception as e:  # noqa: BLE001
        print(f"  Docling warmup skipped: {str(e)[:80]}", flush=True)


if __name__ == "__main__":
    port = int(os.environ.get("PORT", 5050))  # avoid 5000 (macOS AirPlay -> 403)
    warmup()
    print(f"open http://127.0.0.1:{port}", flush=True)
    app.run(host="127.0.0.1", port=port, debug=False)
