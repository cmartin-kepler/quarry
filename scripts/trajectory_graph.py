#!/usr/bin/env python3
"""
trajectory_graph.py - Per-chunk, path-dependent parsing-trajectory GRAPH.

Each chunk of the PDF (here: a page) has its OWN escalation trajectory through a
set of METHODS, and the methods are path-dependent because they operate on
different representations:

  cheap geometric   -> on the PDF text-layer (.qdoc glyph boxes)
  text-table        -> on a parser's TEXT/markdown (LiteParse), via column channels
  Docling           -> on the PDF directly (ML table-structure recognition)
  vision verify     -> on the rendered page image ($ per region)

A chunk escalates lazily: run the cheapest method, validate; if flagged, try the
next method; stop at the first that validates. The result is a GRAPH per chunk —
the path taken (solid) plus the methods not needed (greyed). Click a node to see
that method's extracted tables, validation, and cost.

Usage:  uv run scripts/trajectory_graph.py -o corpus/trajectory-graph.html
"""
from __future__ import annotations

import base64
import io
import json
import os
import subprocess
import sys
import tempfile

import pdfplumber

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import text_tables as tt  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QUARRY = os.path.join(REPO, "target", "debug", "quarry")
DH = "0" * 64

# Featured doc with all methods available.
DOC = {
    "name": "Disney Q2 reconciliations",
    "pdf": "input/finance/disney/q2-fy26-financial-reconciliations.pdf",
    "cheap_store": "corpus/label/q2/cheap2.artifacts",
    "liteparse_json": "corpus/label/q2/q2.liteparse.json",
    "docling_store": "corpus/q2-recon.docling.artifacts",
    "rates": {"cheap": 0.076, "text-table": 0.064, "Docling": 2.03},
}
VISION_RATE, VISION_TIME = 0.02, 1.2


def store_tables(store):
    man = json.load(open(os.path.join(store, "manifest.json")))
    ev = {e["id"]: e for e in json.loads(
        subprocess.run([QUARRY, "explain", store, "--json"], capture_output=True, text=True).stdout or "[]")}
    by_page = {}
    for a in man["artifacts"]:
        if a.get("kind") != "HtmlTable":
            continue
        s = a["meta"]["provenance"].get("Source") or {}
        e = ev.get(a["meta"]["id"], {})
        by_page.setdefault(s["page"], []).append({
            "html": a["html"], "impression": e.get("impression", "no_issues"),
            "signals": e.get("signals", []),
            "figure": any("figure" in g["detail"] for g in e.get("signals", []))})
    return by_page


def explain_grid(grid, page):
    """Validate a coordinate-free text grid through quarry's detectors."""
    cells = [{"row": r, "col": c, "text": t,
              "anchor": {"format": "pdf", "doc": DH, "page": page, "bbox": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}},
              "is_header": r == 0}
             for r, row in enumerate(grid) for c, t in enumerate(row)]
    art = {"kind": "HtmlTable", "meta": {"id": "art_text_0", "content_hash": DH,
           "provenance": {"Source": {"format": "pdf", "doc": DH, "page": page, "bbox": {"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}}},
           "generation": 0, "risk": {"min_ocr_confidence": 1.0, "column_count_variance": 0.0,
           "merged_cell_rows": 0, "empty_cells": 0, "rotated_text": False, "figure_score": 0.0, "notes": []}},
           "n_rows": len(grid), "n_cols": max(len(r) for r in grid), "cells": cells, "html": tt.to_html(grid)}
    td = tempfile.mkdtemp()
    json.dump({"doc_hash": DH, "artifacts": [art]}, open(os.path.join(td, "manifest.json"), "w"))
    out = subprocess.run([QUARRY, "explain", td, "--json"], capture_output=True, text=True).stdout
    e = json.loads(out or "[]")
    return e[0] if e else {"impression": "no_issues", "signals": []}


def page_status(tables):
    if not tables:
        return "missing"
    if any(t.get("figure") for t in tables):
        return "figure"
    if any(t["impression"] == "suspect" for t in tables):
        return "suspect"
    if any(t["impression"] == "confirmed" for t in tables):
        return "confirmed"
    return "ok"


def method_tables(method, page, cheap, lite_text, docling):
    if method == "cheap":
        return cheap.get(page, [])
    if method == "Docling":
        return docling.get(page, [])
    if method == "text-table":
        out = []
        for g in tt.detect_tables(lite_text.get(page, "")):
            e = explain_grid(g, page)
            out.append({"html": tt.to_html(g), "impression": e.get("impression", "no_issues"),
                        "signals": e.get("signals", []),
                        "figure": any("figure" in s["detail"] for s in e.get("signals", []))})
        return out
    return []


def build():
    pdf = pdfplumber.open(DOC["pdf"])
    cheap = store_tables(DOC["cheap_store"])
    docling = store_tables(DOC["docling_store"])
    lj = json.load(open(DOC["liteparse_json"]))
    lite_text = {p["page"]: p["text"] for p in lj["pages"]}

    method_order = ["cheap", "text-table", "Docling"]
    inputs = {"cheap": "PDF text-layer (glyph boxes)", "text-table": "LiteParse text / markdown",
              "Docling": "PDF (direct, ML structure)", "vision verify": "rendered page image"}

    pages = sorted(set(cheap) | set(docling) | set(lite_text))
    out_pages = []
    for pno in pages:
        pg = pdf.pages[pno - 1]
        im = pg.to_image(resolution=110); buf = io.BytesIO(); im.save(buf, format="PNG")
        # Build the path-dependent escalation across methods.
        nodes, resolved, cum_t, cum_c = [], False, 0.0, 0.0
        for m in method_order:
            tried = not resolved
            tabs = method_tables(m, pno, cheap, lite_text, docling) if tried else []
            st = page_status(tabs) if tried else "skipped"
            if tried:
                cum_t += DOC["rates"][m]
            nodes.append({"method": m, "input": inputs[m], "tried": tried, "status": st,
                          "tables": tabs, "time": round(cum_t, 2), "cost": round(cum_c, 2)})
            if tried and st in ("ok", "confirmed"):
                resolved = True
        # vision tier if still unresolved
        last = nodes[-1]
        vtried = not resolved
        if vtried:
            cum_t += VISION_TIME; cum_c += VISION_RATE
        nodes.append({"method": "vision verify", "input": inputs["vision verify"], "tried": vtried,
                      "status": "verified" if vtried else "skipped", "tables": [],
                      "time": round(cum_t, 2), "cost": round(cum_c, 2),
                      "note": "LLM confirms the parse / flags it as a figure"})
        out_pages.append({"page": pno, "img": base64.b64encode(buf.getvalue()).decode(), "nodes": nodes})
    return {"name": DOC["name"], "pages": out_pages}


TEMPLATE = r"""<!doctype html><html><head><meta charset="utf-8"><title>Trajectory graph</title><style>
body{font-family:-apple-system,system-ui,sans-serif;margin:0;background:#f4f4f6;color:#222}
header{background:#fff;border-bottom:1px solid #ddd;padding:12px 16px}
h1{font-size:15px;margin:0} .lg{font-size:12px;color:#666;margin-top:5px}
main{display:flex;gap:16px;padding:14px;align-items:flex-start}
.left{flex:0 0 auto}.pagebtns button{display:block;width:100%;margin-bottom:4px;text-align:left;font-size:12px;padding:4px 8px;cursor:pointer}
.pagebtns button.on{background:#0a66c2;color:#fff;border-color:#0a66c2}
.pageimg{width:240px;border:1px solid #ccc;margin-top:8px}
.right{flex:1;min-width:520px}
.graph{display:flex;align-items:stretch;gap:0;flex-wrap:wrap;margin-bottom:14px}
.node{border:2px solid;border-radius:8px;padding:8px 10px;min-width:130px;background:#fff;cursor:pointer;position:relative}
.node .m{font-weight:700;font-size:13px}.node .i{font-size:10px;color:#666}.node .s{font-size:11px;margin-top:3px}
.node .c{font-size:11px;color:#0a66c2;margin-top:3px}
.node.skip{opacity:.4;border-style:dashed} .node.sel{box-shadow:0 0 0 3px #ffd54a}
.arrow{display:flex;align-items:center;font-size:22px;color:#999;padding:0 6px}
.badge{display:inline-block;padding:0 7px;border-radius:9px;color:#fff;font-weight:600;font-size:11px}
.b-confirmed{background:#0a7}.b-ok{background:#3b82c4}.b-suspect{background:#d33}.b-figure{background:#8b3fc4}
.b-missing{background:#e8820e}.b-verified{background:#0c8}.b-skipped{background:#bbb}
.det{background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px}
table{border-collapse:collapse;font-size:12px}td,th{border:1px solid #bbb;padding:2px 6px;white-space:nowrap}th{background:#eef}
.sig-pos{color:#0a7;font-size:12px}.sig-neg{color:#c00;font-size:12px}.tbl{margin:8px 0;overflow-x:auto}
</style></head><body>
<header><h1>Parsing trajectory graph — <span id="doc"></span></h1>
<div class="lg">Each PAGE is a chunk with its own path-dependent escalation. Methods run on different representations (text-layer / LiteParse text / PDF / image); a chunk escalates only until a method validates. Click a node.</div></header>
<main>
 <div class="left"><div class="pagebtns" id="pagebtns"></div><img class="pageimg" id="pageimg"></div>
 <div class="right"><div class="graph" id="graph"></div><div class="det" id="det"></div></div>
</main>
<script>
const DATA=__DATA__;
const COLOR={confirmed:"#0a7",ok:"#3b82c4",suspect:"#d33",figure:"#8b3fc4",missing:"#e8820e",verified:"#0c8",skipped:"#bbb"};
let page=DATA.pages[0].page, selNode=0;
function P(){return DATA.pages.find(p=>p.page===page);}
function badge(s){return '<span class="badge b-'+s+'">'+({confirmed:"reconciles",ok:"no issues",suspect:"SUSPECT",figure:"FIGURE",missing:"no table",verified:"verified",skipped:"not needed"}[s])+'</span>';}
function renderBtns(){const b=document.getElementById("pagebtns");b.innerHTML="";
 DATA.pages.forEach(p=>{const x=document.createElement("button");
  const final=p.nodes.filter(n=>n.tried).slice(-1)[0];
  x.innerHTML="page "+p.page+" &nbsp;"+badge(final.status);x.className=p.page===page?"on":"";
  x.onclick=()=>{page=p.page;selNode=P_first_tried(p);render();};b.appendChild(x);});}
function P_first_tried(p){return p.nodes.findIndex(n=>n.tried);}
function renderGraph(){const g=document.getElementById("graph");g.innerHTML="";const p=P();
 p.nodes.forEach((n,i)=>{
  const d=document.createElement("div");d.className="node"+(n.tried?"":" skip")+(i===selNode?" sel":"");
  d.style.borderColor=COLOR[n.status]||"#bbb";
  d.innerHTML='<div class="m">'+n.method+'</div><div class="i">on '+n.input+'</div>'+
    '<div class="s">'+badge(n.status)+(n.tables&&n.tables.length?' '+n.tables.length+' tbl':'')+'</div>'+
    (n.tried?'<div class="c">⏱ '+n.time+'s · $'+n.cost.toFixed(2)+'</div>':'');
  d.onclick=()=>{selNode=i;render();};g.appendChild(d);
  if(i<p.nodes.length-1){const a=document.createElement("div");a.className="arrow";a.textContent="→";g.appendChild(a);}
 });}
function renderDet(){const p=P(),n=p.nodes[selNode],det=document.getElementById("det");
 let h='<div><b>'+n.method+'</b> &nbsp; on '+n.input+' &nbsp; '+badge(n.status)+
   (n.tried?' &nbsp; <span class="c">cumulative ⏱ '+n.time+'s · $'+n.cost.toFixed(2)+'</span>':' &nbsp;<i>(not run — an earlier method already validated)</i>')+'</div>';
 if(n.note)h+='<div class="i" style="margin-top:6px">'+n.note+'</div>';
 (n.tables||[]).forEach(t=>{const sigs=(t.signals||[]).map(s=>'<div class="'+(s.positive?"sig-pos":"sig-neg")+'">'+(s.positive?"✓ ":"✗ ")+s.detail+'</div>').join("");
   h+='<div style="margin-top:10px">'+badge(t.impression||"ok")+'<div class="tbl">'+t.html+'</div>'+sigs+'</div>';});
 if(n.tried && !(n.tables||[]).length && n.status==="missing")h+='<div class="i" style="margin-top:8px">no table detected by this method on this chunk</div>';
 det.innerHTML=h;}
function render(){document.getElementById("doc").textContent=DATA.name;
 document.getElementById("pageimg").src="data:image/png;base64,"+P().img;renderBtns();renderGraph();renderDet();}
selNode=P_first_tried(P());render();
</script></body></html>"""


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", default="corpus/trajectory-graph.html")
    args = ap.parse_args()
    data = build()
    open(args.out, "w").write(TEMPLATE.replace("__DATA__", json.dumps(data)))
    print(f"wrote {args.out} ({os.path.getsize(args.out)//1024} KB)", file=sys.stderr)


if __name__ == "__main__":
    main()
