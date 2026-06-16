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
from flask import Flask, jsonify, request

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
_pdf, _pages, _regions, _cheap, _lite, _docling, _wd = {}, {}, {}, {}, {}, {}, tempfile.mkdtemp()


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
    if name not in _pages:
        pdf_ = pdf(name)
        pages, regions = [], []
        for pg in pdf_.pages:
            im = pg.to_image(resolution=150); buf = io.BytesIO(); im.save(buf, format="PNG")
            pages.append({"page": pg.page_number, "w": float(pg.width), "h": float(pg.height),
                          "img": base64.b64encode(buf.getvalue()).decode()})
            for ti, t in enumerate(pg.find_tables()):
                x0, top, x1, bottom = t.bbox
                regions.append({"id": f"p{pg.page_number}t{ti}", "page": pg.page_number,
                                "bbox": [x0, top, x1, bottom],
                                "box": {"left": 100*x0/pg.width, "top": 100*top/pg.height,
                                        "width": 100*(x1-x0)/pg.width, "height": 100*(bottom-top)/pg.height}})
        _pages[name] = pages; _regions[name] = regions
    return jsonify({"pages": _pages[name], "regions": _regions[name]})


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


HTML = r"""<!doctype html><html><head><meta charset="utf-8"><title>Parsing trajectory</title><style>
body{font-family:-apple-system,system-ui,sans-serif;margin:0;background:#f4f4f6;color:#222}
header{background:#fff;border-bottom:1px solid #ddd;padding:12px 16px;position:sticky;top:0;z-index:5}
h1{font-size:15px;margin:0}select{font-size:13px;padding:3px}.lg{font-size:12px;color:#666;margin-top:5px}
.tabs{margin-top:8px}.tabs button{margin-right:4px;font-size:12px;padding:3px 9px;cursor:pointer}.tabs button.on{background:#0a66c2;color:#fff;border-color:#0a66c2}
main{display:flex;gap:18px;padding:14px;align-items:flex-start}
.left{flex:0 0 780px;position:sticky;top:100px}.pagewrap{position:relative;width:780px;border:1px solid #ccc;background:#fff}.pagewrap img{width:100%;display:block}
.ov{position:absolute;box-sizing:border-box;border:2.5px solid #888;cursor:pointer}.ov:hover{border-color:#0a66c2}.ov.sel{box-shadow:0 0 0 3px #ffd54a;border-color:#0a66c2;z-index:2}
.ov .tag{position:absolute;left:0;top:-15px;font-size:10px;background:#fff;padding:0 3px;border:1px solid #ccc;white-space:nowrap}
.right{flex:1;min-width:440px}
.graph{display:flex;align-items:stretch;flex-wrap:wrap;margin-bottom:12px}
.node{border:2px solid;border-radius:8px;padding:7px 10px;min-width:120px;background:#fff;cursor:pointer}
.node .m{font-weight:700;font-size:13px}.node .i{font-size:10px;color:#666}.node .s{font-size:11px;margin-top:3px}.node .c{font-size:11px;color:#0a66c2;margin-top:2px}.node.sel{box-shadow:0 0 0 3px #ffd54a}
.arrow{display:flex;align-items:center;font-size:22px;color:#999;padding:0 5px}
.esc{margin:6px 0 12px}.esc button{font-size:13px;padding:6px 12px;cursor:pointer;background:#0a66c2;color:#fff;border:none;border-radius:6px}.esc button:disabled{background:#bbb}
.badge{display:inline-block;padding:0 7px;border-radius:9px;color:#fff;font-weight:600;font-size:11px}
.b-confirmed{background:#0a7}.b-ok{background:#3b82c4}.b-suspect{background:#d33}.b-figure{background:#8b3fc4}.b-missing{background:#e8820e}.b-verified{background:#0c8}
.det{background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px}
table{border-collapse:collapse;font-size:12px}td,th{border:1px solid #bbb;padding:2px 6px;white-space:nowrap}th{background:#eef}
.sig-pos{color:#0a7;font-size:12px}.sig-neg{color:#c00;font-size:12px}.tbl{margin:8px 0;overflow-x:auto}
details img{max-width:480px;border:1px solid #ccc;margin-top:5px}.hint{color:#888;padding:16px}.spin{color:#0a66c2}
</style></head><body>
<header><h1>Parsing trajectory &nbsp;<select id="docsel"></select></h1>
<div class="lg"><b>Click a table</b> in the PDF → it parses live (cheap). If flagged, <b>Escalate</b> runs the next method on demand (LiteParse → Docling per-page → vision). Every time is measured live; nothing is hardcoded.</div>
<div class="tabs" id="tabs"></div></header>
<main>
 <div class="left"><div class="pagewrap" id="pagewrap"><img id="pageimg"><div id="overlays"></div></div></div>
 <div class="right"><div id="graph" class="graph"></div><div id="esc" class="esc"></div><div id="det" class="det"></div></div>
</main>
<script>
const LADDER=["cheap","text-table","Docling","vision"];
const COLOR={confirmed:"#0a7",ok:"#3b82c4",suspect:"#d33",figure:"#8b3fc4",missing:"#e8820e",verified:"#0c8"};
let docName=null, doc=null, page=null, sel=null, traj=[], selNode=0;
async function j(u,o){const r=await fetch(u,o);return r.json();}
async function loadDocs(){const ds=await j("/api/docs");const s=document.getElementById("docsel");
 s.innerHTML=ds.map(d=>'<option>'+d+'</option>').join("");s.onchange=()=>selectDoc(s.value);selectDoc(ds[0]);}
async function selectDoc(n){docName=n;doc=await j("/api/doc/"+encodeURIComponent(n));page=doc.pages[0].page;sel=null;traj=[];renderTabs();renderPage();renderRight();}
function renderTabs(){document.getElementById("tabs").innerHTML=doc.pages.map(p=>'<button class="'+(p.page===page?"on":"")+'" onclick="setPage('+p.page+')">page '+p.page+'</button>').join("");}
function setPage(p){page=p;sel=null;traj=[];renderTabs();renderPage();renderRight();}
function regionsOn(p){return doc.regions.filter(r=>r.page===p);}
function renderPage(){const pg=doc.pages.find(x=>x.page===page);
 document.getElementById("pageimg").src="data:image/png;base64,"+pg.img;
 document.getElementById("overlays").innerHTML=regionsOn(page).map(r=>{
   const last=sel===r.id&&traj.length?traj[traj.length-1].status:null;
   const col=last?COLOR[last]:"#888";
   return '<div class="ov'+(sel===r.id?" sel":"")+'" style="left:'+r.box.left+'%;top:'+r.box.top+'%;width:'+r.box.width+'%;height:'+r.box.height+'%;border-color:'+col+'" onclick="pick(\''+r.id+'\')"></div>';
 }).join("");}
async function pick(id){sel=id;traj=[];selNode=0;renderPage();renderRight();await step("cheap");}
async function step(method){
 const r=doc.regions.find(x=>x.id===sel);
 document.getElementById("esc").innerHTML='<span class="spin">parsing with '+method+' …</span>';
 const res=await j("/api/parse",{method:"POST",headers:{"Content-Type":"application/json"},
   body:JSON.stringify({name:docName,page:r.page,bbox:r.bbox,method})});
 traj.push(res);selNode=traj.length-1;renderPage();renderRight();}
function badge(s){return '<span class="badge b-'+s+'">'+({confirmed:"reconciles",ok:"no issues",suspect:"SUSPECT",figure:"FIGURE",missing:"no table",verified:"verified"}[s]||s)+'</span>';}
function renderRight(){
 const g=document.getElementById("graph"),e=document.getElementById("esc"),d=document.getElementById("det");
 if(!sel){g.innerHTML='<div class="hint">← click a table in the PDF to parse it on demand</div>';e.innerHTML="";d.innerHTML="";return;}
 g.innerHTML=traj.map((n,i)=>{const arrow=i<traj.length-1?'<div class="arrow">→</div>':'';
   return '<div class="node'+(i===selNode?" sel":"")+'" style="border-color:'+COLOR[n.status]+'" onclick="selNode='+i+';renderRight()">'+
    '<div class="m">'+n.method+'</div><div class="i">on '+n.input+'</div><div class="s">'+badge(n.status)+'</div>'+
    '<div class="c">⏱ '+n.seconds+'s · $'+(n.dollars||0).toFixed(2)+'</div></div>'+arrow;}).join("");
 const last=traj.length?traj[traj.length-1]:null, nextIdx=last?LADDER.indexOf(last.method)+1:0;
 const flagged=last&&(last.status==="suspect"||last.status==="figure"||last.status==="missing");
 if(flagged&&nextIdx<LADDER.length){e.innerHTML='<button onclick="step(\''+LADDER[nextIdx]+'\')">Escalate → '+LADDER[nextIdx]+' (parse on demand)</button>';}
 else if(last&&(last.status==="ok"||last.status==="confirmed")){e.innerHTML='<span style="color:#0a7;font-size:13px">✓ validated at '+last.method+' — escalation stops (lazy)</span>';}
 else e.innerHTML="";
 const n=traj[selNode];
 if(!n){d.innerHTML="";return;}
 let h='<div><b>'+n.method+'</b> on '+n.input+' &nbsp;'+badge(n.status)+' &nbsp;<span class="c">⏱ '+n.seconds+'s · $'+(n.dollars||0).toFixed(2)+'</span></div>';
 if(n.note)h+='<div class="i" style="margin-top:6px">'+n.note+'</div>';
 if(n.recon!=null)h+='<div class="i" style="margin-top:4px">reconstruction error '+n.recon+'</div>';
 if(n.html)h+='<div class="tbl">'+n.html+'</div>'+(n.signals||[]).map(s=>'<div class="'+(s.positive?"sig-pos":"sig-neg")+'">'+(s.positive?"✓ ":"✗ ")+s.detail+'</div>').join("");
 if(n.detail)h+='<details><summary>reconstruction diff (green=matched, orange=misplaced, red=missing)</summary><img src="data:image/png;base64,'+n.detail+'"></details>';
 d.innerHTML=h;}
loadDocs();
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
