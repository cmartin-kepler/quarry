#!/usr/bin/env python3
"""
trajectory.py - Visualize the lazy, iterative parsing trajectory of a document.

This is the brief's core model made visible: a cheap parse runs first; validators
flag the regions that look wrong; only those get re-parsed with a better parser;
re-validate; repeat. The output is a single self-contained HTML with a SLIDER:

  Stage 0  uploaded — pages shown, regions detected, nothing parsed yet
  Stage 1  cheap parse + validate — every region parsed; detectors color them
  Stage 2  escalate the flagged ones to Docling + re-validate — lazy: regions
           that already looked fine are NOT re-parsed

The whole document is on the left (rendered pages with region overlays colored by
their CURRENT-stage validation status); the selected region's extracted table +
validation evidence is on the right. Move the slider to watch a flagged region
get escalated and turn green.

Usage:
  uv run scripts/trajectory.py \
      --pdf input/finance/disney/q2-fy26-financial-reconciliations.pdf \
      --cheap corpus/label/q2/cheap2.artifacts \
      --better corpus/q2-recon.docling.artifacts \
      -o corpus/trajectory-q2.html
"""
from __future__ import annotations

import argparse
import base64
import html as H
import io
import json
import os
import subprocess
import sys

import pdfplumber

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import recon_validate as rv  # noqa: E402

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
QUARRY = os.path.join(REPO, "target", "debug", "quarry")


def load(store: str) -> list[dict]:
    man = json.load(open(os.path.join(store, "manifest.json")))
    ev = {e["id"]: e for e in json.loads(
        subprocess.run([QUARRY, "explain", store, "--json"], capture_output=True, text=True).stdout or "[]")}
    out = []
    for a in man["artifacts"]:
        if a.get("kind") != "HtmlTable":
            continue
        s = a["meta"]["provenance"].get("Source") or {}
        if s.get("format") != "pdf":
            continue
        b = s["bbox"]
        out.append({
            "id": a["meta"]["id"], "page": s["page"],
            "bbox": (b["x0"], b["y0"], b["x1"], b["y1"]),
            "rows": a["n_rows"], "cols": a["n_cols"], "html": a["html"],
            "ev": ev.get(a["meta"]["id"], {}),
        })
    return out


def iou(a, b):
    ix0, iy0 = max(a[0], b[0]), max(a[1], b[1])
    ix1, iy1 = min(a[2], b[2]), min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1 - ix0) * (iy1 - iy0)
    return inter / ((a[2]-a[0])*(a[3]-a[1]) + (b[2]-b[0])*(b[3]-b[1]) - inter + 1e-9)


def status_of(t: dict) -> str:
    """confirmed | ok | suspect — drives color and the escalation decision."""
    return {"confirmed": "confirmed", "no_issues": "ok", "suspect": "suspect"}.get(
        t["ev"].get("impression", ""), "ok")


def validation(t: dict, pdf_path: str, parser: str) -> dict:
    res = rv.validate_table(pdf_path, t["page"], t["bbox"], t["html"])
    return {
        "parser": parser,
        "status": status_of(t),
        "impression": t["ev"].get("impression"),
        "signals": t["ev"].get("signals", []),
        "recon": None if res.error is None else round(res.error, 3),
        "rows": t["rows"], "cols": t["cols"],
        "html": t["html"],
    }


def build(pdf_path, cheap, better):
    pdf = pdfplumber.open(pdf_path)
    pages = []
    for pg in pdf.pages:
        im = pg.to_image(resolution=130)
        buf = io.BytesIO(); im.save(buf, format="PNG")
        pages.append({"page": pg.page_number, "w": float(pg.width), "h": float(pg.height),
                      "img": base64.b64encode(buf.getvalue()).decode()})

    regions = []
    for ct in cheap:
        v1 = validation(ct, pdf_path, "cheap")
        s0 = {"parser": None, "status": "unparsed"}
        # Stage 2: escalate ONLY if stage 1 flagged it (lazy).
        if v1["status"] == "suspect":
            match = max((bt for bt in better if bt["page"] == ct["page"]),
                        key=lambda bt: iou(ct["bbox"], bt["bbox"]), default=None)
            if match and iou(ct["bbox"], match["bbox"]) > 0.3:
                v2 = validation(match, pdf_path, "docling (escalated)")
            else:
                v2 = dict(v1); v2["parser"] = "cheap (no better parse available)"
        else:
            v2 = v1  # unchanged — not re-parsed
        pw = next(p for p in pages if p["page"] == ct["page"])
        x0, y0, x1, y1 = ct["bbox"]
        regions.append({
            "id": ct["id"], "page": ct["page"],
            "box": {"left": 100*x0/pw["w"], "top": 100*y0/pw["h"],
                    "width": 100*(x1-x0)/pw["w"], "height": 100*(y1-y0)/pw["h"]},
            "stages": [s0, v1, v2],
        })
    return {"doc": os.path.basename(pdf_path),
            "stages": ["Uploaded (detected, unparsed)", "Cheap parse + validate",
                       "Escalate flagged → better parser"],
            "pages": pages, "regions": regions}


PAGE = r"""<!doctype html><html><head><meta charset="utf-8"><title>Parsing trajectory</title>
<style>
 body{font-family:-apple-system,system-ui,sans-serif;margin:0;background:#f4f4f6;color:#222}
 header{background:#fff;border-bottom:1px solid #ddd;padding:12px 18px;position:sticky;top:0;z-index:5}
 h1{font-size:16px;margin:0 0 8px} .sub{color:#666;font-size:12px}
 .slider{display:flex;align-items:center;gap:12px;margin-top:8px}
 input[type=range]{flex:1;max-width:480px} .stagelbl{font-weight:600;font-size:13px;min-width:230px}
 main{display:flex;gap:16px;padding:16px;align-items:flex-start}
 .left{flex:0 0 auto} .tabs{margin-bottom:8px} .tabs button{margin-right:4px;font-size:12px;padding:3px 8px;cursor:pointer}
 .tabs button.on{background:#0a66c2;color:#fff;border-color:#0a66c2}
 .pagewrap{position:relative;width:540px;border:1px solid #ccc;background:#fff}
 .pagewrap img{width:100%;display:block}
 .ov{position:absolute;box-sizing:border-box;border:2px solid;cursor:pointer;opacity:.85}
 .ov.sel{box-shadow:0 0 0 3px #ffd54a}
 .right{flex:1;min-width:380px}
 .card{background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px}
 .badge{display:inline-block;padding:1px 8px;border-radius:10px;color:#fff;font-weight:600;font-size:12px}
 .b-confirmed{background:#0a7} .b-ok{background:#3b82c4} .b-suspect{background:#d33} .b-unparsed{background:#999}
 table{border-collapse:collapse;font-size:12px} td,th{border:1px solid #bbb;padding:2px 6px;white-space:nowrap}
 th{background:#eef} .sig-pos{color:#0a7;font-size:12px} .sig-neg{color:#c00;font-size:12px}
 .meta2{color:#666;font-size:12px;margin:6px 0} .parser{font-size:12px;color:#444}
 .legend{font-size:11px;color:#555;margin-top:6px}
 .legend b{padding:0 4px;border-radius:3px;color:#fff}
</style></head><body>
<header>
 <h1>Parsing trajectory — <span id="doc"></span></h1>
 <div class="sub">Lazy &amp; iterative: cheap parse first, validate, then escalate only the flagged regions. Drag the slider.</div>
 <div class="slider">
   <input type="range" id="stage" min="0" max="2" value="1" step="1">
   <span class="stagelbl" id="stagelbl"></span>
 </div>
 <div class="legend">regions:
   <b class="b-unparsed">unparsed</b> <b class="b-suspect">suspect (flagged)</b>
   <b class="b-ok">no issues</b> <b class="b-confirmed">arithmetic reconciles</b></div>
</header>
<main>
 <div class="left"><div class="tabs" id="tabs"></div><div class="pagewrap" id="pagewrap"><img id="pageimg"><div id="overlays"></div></div></div>
 <div class="right" id="right"></div>
</main>
<script>
const DATA = __DATA__;
const COLOR = {confirmed:"#0a7", ok:"#3b82c4", suspect:"#d33", unparsed:"#999"};
let stage = 1, page = DATA.pages[0].page, sel = null;

function regionsOn(p){ return DATA.regions.filter(r=>r.page===p); }
function stageOf(r){ return r.stages[stage]; }

function renderTabs(){
  const t = document.getElementById("tabs"); t.innerHTML="";
  DATA.pages.forEach(p=>{ const b=document.createElement("button");
    b.textContent="p"+p.page; b.className=p.page===page?"on":"";
    b.onclick=()=>{page=p.page; sel=null; renderAll();}; t.appendChild(b); });
}
function renderPage(){
  const p = DATA.pages.find(x=>x.page===page);
  document.getElementById("pageimg").src = "data:image/png;base64,"+p.img;
  const ov = document.getElementById("overlays"); ov.innerHTML="";
  regionsOn(page).forEach(r=>{
    const st = stageOf(r); const d=document.createElement("div");
    d.className="ov"+(sel===r.id?" sel":"");
    d.style.left=r.box.left+"%"; d.style.top=r.box.top+"%";
    d.style.width=r.box.width+"%"; d.style.height=r.box.height+"%";
    d.style.borderColor=COLOR[st.status]; d.style.background=COLOR[st.status]+"22";
    d.title=(st.parser||"unparsed")+" — "+st.status;
    d.onclick=()=>{sel=r.id; renderAll();}; ov.appendChild(d);
  });
}
function badge(s){ return '<span class="badge b-'+s+'">'+
  ({confirmed:"LIKELY CORRECT",ok:"no issues",suspect:"SUSPECT",unparsed:"not parsed yet"}[s])+'</span>'; }
function renderRight(){
  const right=document.getElementById("right");
  let regs = regionsOn(page);
  if(sel) regs = regs.filter(r=>r.id===sel).concat(regs.filter(r=>r.id!==sel));
  right.innerHTML = regs.map(r=>{
    const st=stageOf(r);
    if(st.status==="unparsed") return '<div class="card"><div class="parser">region on p'+r.page+
      '</div>'+badge("unparsed")+'<div class="meta2">not parsed yet — stage 0</div></div>';
    const sigs=(st.signals||[]).map(s=>'<div class="'+(s.positive?"sig-pos":"sig-neg")+'">'+
      (s.positive?"✓ ":"✗ ")+s.detail+'</div>').join("");
    const recon = st.recon===null? "" : ' &middot; reconstruction error '+st.recon;
    return '<div class="card'+(sel===r.id?'" style="box-shadow:0 0 0 2px #ffd54a':"")+'">'+
      '<div class="parser"><b>'+st.parser+'</b> &middot; '+st.rows+'×'+st.cols+recon+'</div>'+
      badge(st.status)+'<div style="margin-top:8px">'+st.html+'</div>'+
      '<div style="margin-top:8px">'+sigs+'</div></div>';
  }).join("");
}
function renderAll(){ renderTabs(); renderPage(); renderRight();
  document.getElementById("stagelbl").textContent = "Stage "+stage+": "+DATA.stages[stage]; }
document.getElementById("doc").textContent = DATA.doc;
document.getElementById("stage").oninput = e=>{ stage=+e.target.value; renderAll(); };
renderAll();
</script></body></html>"""


def main():
    ap = argparse.ArgumentParser(description="Lazy/iterative parsing trajectory viewer.")
    ap.add_argument("--pdf", required=True)
    ap.add_argument("--cheap", required=True, help="cheap-parser artifact store")
    ap.add_argument("--better", required=True, help="better-parser artifact store (escalation target)")
    ap.add_argument("-o", "--out", required=True)
    args = ap.parse_args()

    data = build(args.pdf, load(args.cheap), load(args.better))
    out = PAGE.replace("__DATA__", json.dumps(data))
    with open(args.out, "w") as fh:
        fh.write(out)
    print(f"wrote {args.out} ({os.path.getsize(args.out)//1024} KB) — open in a browser", file=sys.stderr)


if __name__ == "__main__":
    main()
