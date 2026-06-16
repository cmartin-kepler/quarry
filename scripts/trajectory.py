#!/usr/bin/env python3
"""
trajectory.py - Visualize the lazy, iterative parsing trajectory of documents.

The brief's core model, made interactive. A SLIDER steps through the escalation
lineage; only regions that fail validation escalate to the next (more expensive)
tier:

  0 uploaded            regions detected, nothing parsed
  1 cheap parse         geometric reconstruction (fast, ~free)
  2 LiteParse           OCR-aware tokens -> reconstruction (LiteParse CLI emits a
                        positioned text layer, NOT markdown tables, so we detect
                        tables from its tokens)
  3 Docling             ML table-structure recognition (slow)
  4 vision verify       LLM checks the still-ambiguous ones ($ per region)

The whole document is on the left (pages with region overlays colored by current
validation status, plus orange "coverage gap" overlays where a real text block
was NOT captured as a table). The selected region's extracted table, validation
evidence, COST (time + $), and reconstruction diff (annotated source crop:
green=matched, orange=misplaced, red=missing) are on the right.

Usage:  uv run scripts/trajectory.py -o corpus/trajectory.html
"""
from __future__ import annotations

import base64
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
VISION_RATE = 0.02   # $ per region for an LLM vision verify (reference)
VISION_TIME = 1.2    # s per region

# Featured documents: (name, pdf, cheap_qdoc, [(tier, store, s/page)])
SPECS = [
    {"name": "Disney Q2 reconciliations", "pdf": "input/finance/disney/q2-fy26-financial-reconciliations.pdf",
     "qdoc": "corpus/label/q2/q2b.qdoc",
     "tiers": [("cheap", "corpus/label/q2/cheap2.artifacts", 0.076),
               ("LiteParse", "corpus/label/q2/lp.artifacts", 0.064),
               ("Docling", "corpus/q2-recon.docling.artifacts", 2.03)]},
    {"name": "ParseBench paper (arXiv)", "pdf": "input/arxiv/2604.08538v3.pdf",
     "qdoc": "corpus/input/2604.08538v3.qdoc", "max_pages": 8,
     "tiers": [("cheap", "corpus/input/2604.08538v3.artifacts", 0.049),
               ("LiteParse", "corpus/2604.lp_ocr.artifacts", 0.13),
               ("Docling", "corpus/2604.docling.artifacts", 5.57)]},
    {"name": "JPMorgan annual report (charts)", "pdf": "input/finance/jpm-2023-ar.pdf",
     "qdoc": "corpus/label/jpm/jpm.qdoc", "only_pages": [14, 15, 16],
     "tiers": [("cheap", "corpus/label/jpm/cheap2.artifacts", 0.05)]},
]


def load(store):
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
        e = ev.get(a["meta"]["id"], {})
        out.append({"page": s["page"], "bbox": (b["x0"], b["y0"], b["x1"], b["y1"]),
                    "rows": a["n_rows"], "cols": a["n_cols"], "html": a["html"],
                    "impression": e.get("impression", "no_issues"), "signals": e.get("signals", []),
                    "figure": any("figure" in g["detail"] for g in e.get("signals", []))})
    return out


def iou(a, b):
    ix0, iy0, ix1, iy1 = max(a[0], b[0]), max(a[1], b[1]), min(a[2], b[2]), min(a[3], b[3])
    if ix1 <= ix0 or iy1 <= iy0:
        return 0.0
    inter = (ix1 - ix0) * (iy1 - iy0)
    return inter / ((a[2]-a[0])*(a[3]-a[1]) + (b[2]-b[0])*(b[3]-b[1]) - inter + 1e-9)


def status_of(t):
    if t.get("figure"):
        return "figure"
    return {"confirmed": "confirmed", "no_issues": "ok", "suspect": "suspect"}.get(t["impression"], "ok")


def recon_detail(pdf, pdf_path, page, bbox, html):
    res, det = rv.validate_detail(pdf_path, page, bbox, html)
    if res.status != "ok":
        return None, None
    miss = sum(1 for o in det["obs"] if o["status"] == "missing")
    mis = sum(1 for o in det["obs"] if o["status"] == "misplaced")
    pg = pdf.pages[page - 1]
    x0, y0, x1, y1 = bbox
    box = (max(0, x0-4), max(0, y0-4), min(pg.width, x1+4), min(pg.height, y1+4))
    im = pg.crop(box).to_image(resolution=130)
    colors = {"matched": (0, 160, 110), "misplaced": (235, 140, 0), "missing": (215, 0, 0)}
    for tok in det["obs"]:
        c = colors[tok["status"]]
        im.draw_rect(tok["bbox"], stroke=c, stroke_width=1 if tok["status"] == "matched" else 2, fill=c+(28,))
    buf = io.BytesIO(); im.save(buf, format="PNG")
    return round(res.error, 3), {"png": base64.b64encode(buf.getvalue()).decode(),
                                 "miss": miss, "mis": mis}


def coverage_gaps(qdoc_path, region_boxes_by_page, restrict):
    """Table-like text clusters NOT covered by any parsed region — possible missed
    tables. Filtered to be number-dense and column-structured so prose (titles,
    footnotes) isn't mistaken for a table."""
    doc = json.load(open(qdoc_path))
    gaps = []
    for p in doc["pages"]:
        if restrict and p["page"] not in restrict:
            continue
        regs = region_boxes_by_page.get(p["page"], [])
        unc, unc_num = [], []
        for s in p["spans"]:
            x0, y0, x1, y1 = s["bbox"]
            cx, cy = (x0+x1)/2, (y0+y1)/2
            if not any(b[0] <= cx <= b[2] and b[1] <= cy <= b[3] for b in regs):
                unc.append((x0, y0, x1, y1))
                unc_num.append(any(ch.isdigit() for ch in s["text"]))
        if len(unc) < 25:
            continue
        order = sorted(range(len(unc)), key=lambda i: unc[i][1])
        cluster, last_y, clusters = [], None, []
        for i in order:
            if last_y is not None and unc[i][1] - last_y > 40:
                clusters.append(cluster); cluster = []
            cluster.append(i); last_y = unc[i][1]
        clusters.append(cluster)
        for cl in clusters:
            if len(cl) < 20:
                continue
            spans = [unc[i] for i in cl]
            num_frac = sum(unc_num[i] for i in cl) / len(cl)
            if num_frac < 0.2:  # prose / footnotes are not number-dense
                continue
            # >=3 distinct column-start positions each used by several spans.
            xs = sorted(s[0] for s in spans)
            xclusters, cur = [], [xs[0]]
            for v in xs[1:]:
                if v - cur[-1] <= 8:
                    cur.append(v)
                else:
                    xclusters.append(cur); cur = [v]
            xclusters.append(cur)
            cols = sum(1 for g in xclusters if len(g) >= 3)
            ys = sorted({round((s[1] + s[3]) / 2) for s in spans})
            rows = 1 + sum(1 for a, b in zip(ys, ys[1:]) if b - a > 6)
            if cols < 3 or rows < 3:
                continue
            gaps.append({"page": p["page"], "bbox": (min(s[0] for s in spans), min(s[1] for s in spans),
                         max(s[2] for s in spans), max(s[3] for s in spans)), "spans": len(cl)})
    return gaps


def build_doc(spec):
    pdf = pdfplumber.open(spec["pdf"])
    tiers = [(n, load(s), spp) for (n, s, spp) in spec["tiers"]]
    tier_names = [n for n, _, _ in tiers]

    # Union of regions across tiers, anchored to the first tier that has each.
    regions = []
    for ti, (tname, tables, _) in enumerate(tiers):
        for t in tables:
            m = next((r for r in regions if r["page"] == t["page"] and iou(r["bbox"], t["bbox"]) > 0.4), None)
            if m is None:
                regions.append({"page": t["page"], "bbox": t["bbox"], "byTier": {tname: t}})
            else:
                m["byTier"].setdefault(tname, t)

    # Coverage gaps: table-like text that NO tier captured (truly missed by
    # everyone). Measured against the union of every tier's regions, padded a
    # little so text at a detected table's edge isn't counted as a gap.
    all_boxes = {}
    for r in regions:
        x0, y0, x1, y1 = r["bbox"]
        all_boxes.setdefault(r["page"], []).append((x0 - 8, y0 - 8, x1 + 8, y1 + 8))
    only = spec.get("only_pages")
    gaps = coverage_gaps(spec["qdoc"], all_boxes, only)

    # Which pages to render: those with a region or a gap (+ cap).
    cand_pages = sorted({r["page"] for r in regions} | {g["page"] for g in gaps})
    if only:
        cand_pages = [p for p in cand_pages if p in only]
    if spec.get("max_pages"):
        cand_pages = cand_pages[: spec["max_pages"]]
    cand = set(cand_pages)

    pages = []
    for pno in cand_pages:
        pg = pdf.pages[pno - 1]
        im = pg.to_image(resolution=120); buf = io.BytesIO(); im.save(buf, format="PNG")
        pages.append({"page": pno, "w": float(pg.width), "h": float(pg.height),
                      "img": base64.b64encode(buf.getvalue()).decode()})

    def pct(box, pno):
        pw = next(p for p in pages if p["page"] == pno)
        x0, y0, x1, y1 = box
        return {"left": 100*x0/pw["w"], "top": 100*y0/pw["h"],
                "width": 100*(x1-x0)/pw["w"], "height": 100*(y1-y0)/pw["h"]}

    # Per-region trajectory (lazy escalation through the tiers, then vision).
    tier_spp = {n: spp for n, _, spp in tiers}
    out_regions, idx = [], 0
    for r in regions:
        if r["page"] not in cand:
            continue
        idx += 1
        x0, y0, x1, y1 = r["bbox"]
        regs_on_page = sum(1 for q in regions if q["page"] == r["page"])
        stages = [{"status": "unparsed", "tier": None, "time": 0.0, "cost": 0.0}]
        resolved = False
        cum_time, cum_cost = 0.0, 0.0
        for tname in tier_names:
            if resolved:
                stages.append(dict(stages[-1]))  # carry — not re-parsed (lazy)
                continue
            t = r["byTier"].get(tname)
            cum_time += tier_spp[tname] / max(1, regs_on_page)
            if t is None:
                stages.append({"status": "missing", "tier": tname, "time": round(cum_time, 2),
                               "cost": cum_cost, "note": "no table detected at this tier"})
                continue
            st = status_of(t)
            err, detail = recon_detail(pdf, spec["pdf"], r["page"], r["bbox"], t["html"]) \
                if tname in ("cheap", "Docling") else (None, None)
            stages.append({"status": st, "tier": tname, "rows": t["rows"], "cols": t["cols"],
                           "html": t["html"], "signals": t["signals"], "recon": err,
                           "detail": detail, "time": round(cum_time, 2), "cost": round(cum_cost, 3)})
            if st in ("confirmed", "ok"):
                resolved = True
        # Vision tier (stage = len(tiers)+1): verify if still unresolved.
        last = stages[-1]
        if last["status"] in ("suspect", "figure", "missing"):
            verdict = "figure — excluded from tables" if last["status"] == "figure" else "vision-verified"
            stages.append({"status": "verified", "tier": "vision verify",
                           "time": round(cum_time + VISION_TIME, 2), "cost": round(cum_cost + VISION_RATE, 3),
                           "note": verdict, "html": last.get("html"), "signals": last.get("signals", []),
                           "recon": last.get("recon"), "detail": last.get("detail"),
                           "rows": last.get("rows"), "cols": last.get("cols")})
        else:
            stages.append(dict(stages[-1]))
        out_regions.append({"id": f"r{idx}", "page": r["page"], "box": pct(r["bbox"], r["page"]),
                            "stages": stages})

    gap_overlays = [{"page": g["page"], "box": pct(g["bbox"], g["page"]), "spans": g["spans"]}
                    for g in gaps if g["page"] in cand]

    # Cost summary per stage (doc level): lazy vs eager.
    n_pages_total = len(pdf.pages)
    cheap_total = tier_spp[tier_names[0]] * n_pages_total
    docling_spp = tier_spp.get("Docling", 5.0)
    stage_names = ["Uploaded", "Cheap parse + validate"] + \
                  [f"Escalate flagged → {n}" for n in tier_names[1:]] + ["Vision verify ambiguous"]
    return {"name": spec["name"], "stages": stage_names, "pages": pages,
            "regions": out_regions, "gaps": gap_overlays,
            "cost": {"cheap_total": round(cheap_total, 2), "docling_spp": round(docling_spp, 2),
                     "n_pages": n_pages_total, "n_regions": len(out_regions),
                     "vision_rate": VISION_RATE,
                     "eager_docling_time": round(docling_spp * n_pages_total, 1),
                     "eager_vision_cost": round(VISION_RATE * len(out_regions), 2)}}


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", default="corpus/trajectory.html")
    args = ap.parse_args()
    docs = []
    for spec in SPECS:
        if not all(os.path.exists(s) for _, s, _ in spec["tiers"]) or not os.path.exists(spec["qdoc"]):
            print(f"skip {spec['name']} (missing inputs)", file=sys.stderr); continue
        print(f"building {spec['name']} ...", file=sys.stderr)
        docs.append(build_doc(spec))
    html = TEMPLATE.replace("__DATA__", json.dumps({"docs": docs}))
    open(args.out, "w").write(html)
    print(f"wrote {args.out} ({os.path.getsize(args.out)//1024} KB)", file=sys.stderr)


TEMPLATE = r"""<!doctype html><html><head><meta charset="utf-8"><title>Parsing trajectory</title><style>
body{font-family:-apple-system,system-ui,sans-serif;margin:0;background:#f4f4f6;color:#222}
header{background:#fff;border-bottom:1px solid #ddd;padding:10px 16px;position:sticky;top:0;z-index:5}
h1{font-size:15px;margin:0 0 6px} select{font-size:13px;padding:2px}
.slider{display:flex;align-items:center;gap:12px;margin-top:8px}
input[type=range]{flex:1;max-width:520px} .stagelbl{font-weight:600;font-size:13px;min-width:230px}
.cost{font-size:12px;color:#333;margin-top:6px;background:#f0f4f8;padding:5px 8px;border-radius:5px}
.legend{font-size:11px;color:#555;margin-top:6px} .legend b{padding:0 4px;border-radius:3px;color:#fff}
main{display:flex;gap:16px;padding:14px;align-items:flex-start}
.tabs button{margin-right:4px;font-size:12px;padding:3px 8px;cursor:pointer}
.tabs button.on{background:#0a66c2;color:#fff;border-color:#0a66c2}
.pagewrap{position:relative;width:520px;border:1px solid #ccc;background:#fff;margin-top:6px}
.pagewrap img{width:100%;display:block}
.ov{position:absolute;box-sizing:border-box;border:2px solid;cursor:pointer}
.ov.sel{box-shadow:0 0 0 3px #ffd54a;z-index:2}
.gap{position:absolute;box-sizing:border-box;border:2px dashed #e8820e;background:#e8820e18;cursor:pointer}
.right{flex:1;min-width:400px}
.card{background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px;margin-bottom:12px}
.badge{display:inline-block;padding:1px 8px;border-radius:10px;color:#fff;font-weight:600;font-size:12px}
.b-confirmed{background:#0a7}.b-ok{background:#3b82c4}.b-suspect{background:#d33}.b-figure{background:#8b3fc4}
.b-unparsed{background:#999}.b-missing{background:#e8820e}.b-verified{background:#0c8}
table{border-collapse:collapse;font-size:12px}td,th{border:1px solid #bbb;padding:2px 6px;white-space:nowrap}th{background:#eef}
.sig-pos{color:#0a7;font-size:12px}.sig-neg{color:#c00;font-size:12px}
.parser{font-size:12px;color:#444}.costline{font-size:12px;color:#0a66c2;margin:5px 0;font-weight:600}
details{margin-top:6px}summary{cursor:pointer;font-size:12px}details img{max-width:480px;border:1px solid #ccc;margin-top:5px}
.lg{font-size:11px}
</style></head><body>
<header>
 <h1>Parsing trajectory &nbsp; <select id="docsel"></select></h1>
 <div class="lg">Lazy &amp; iterative: cheap parse first, validate, then escalate ONLY the flagged regions up the lineage (cheap → LiteParse → Docling → vision). Drag the slider.</div>
 <div class="slider"><input type="range" id="stage" min="0" value="1" step="1"><span class="stagelbl" id="stagelbl"></span></div>
 <div class="cost" id="cost"></div>
 <div class="legend">regions: <b class="b-unparsed">unparsed</b> <b class="b-suspect">suspect</b>
  <b class="b-figure">figure</b> <b class="b-missing">missing</b> <b class="b-ok">no issues</b>
  <b class="b-confirmed">reconciles</b> <b class="b-verified">vision-verified</b> &nbsp;
  <b style="background:#e8820e">▢ coverage gap (possible missed table)</b></div>
</header>
<main>
 <div class="left"><div class="tabs" id="tabs"></div><div class="pagewrap" id="pagewrap"><img id="pageimg"><div id="overlays"></div></div></div>
 <div class="right" id="right"></div>
</main>
<script>
const ALL = __DATA__;
const COLOR={confirmed:"#0a7",ok:"#3b82c4",suspect:"#d33",figure:"#8b3fc4",unparsed:"#999",missing:"#e8820e",verified:"#0c8"};
let di=0, stage=1, page=0, sel=null;
function D(){return ALL.docs[di];}
function regionsOn(p){return D().regions.filter(r=>r.page===p);}
function gapsOn(p){return D().gaps.filter(g=>g.page===p);}
function st(r){return r.stages[Math.min(stage, r.stages.length-1)];}
function initDoc(){ page=D().pages[0].page; sel=null;
  document.getElementById("stage").max=D().stages.length-1;
  if(stage>D().stages.length-1) stage=1; }
function renderSel(){
  const s=document.getElementById("docsel"); s.innerHTML="";
  ALL.docs.forEach((d,i)=>{const o=document.createElement("option");o.value=i;o.textContent=d.name;s.appendChild(o);});
  s.value=di; s.onchange=e=>{di=+e.target.value; initDoc(); renderAll();};
}
function renderTabs(){const t=document.getElementById("tabs");t.innerHTML="";
  D().pages.forEach(p=>{const b=document.createElement("button");b.textContent="p"+p.page;
    b.className=p.page===page?"on":"";b.onclick=()=>{page=p.page;sel=null;renderAll();};t.appendChild(b);});}
function renderPage(){const p=D().pages.find(x=>x.page===page);
  document.getElementById("pageimg").src="data:image/png;base64,"+p.img;
  const ov=document.getElementById("overlays");ov.innerHTML="";
  if(stage>=1) gapsOn(page).forEach(g=>{const d=document.createElement("div");d.className="gap";
    d.style.left=g.box.left+"%";d.style.top=g.box.top+"%";d.style.width=g.box.width+"%";d.style.height=g.box.height+"%";
    d.title="coverage gap: "+g.spans+" text tokens not captured as a table";ov.appendChild(d);});
  regionsOn(page).forEach(r=>{const s=st(r);const d=document.createElement("div");
    d.className="ov"+(sel===r.id?" sel":"");d.style.left=r.box.left+"%";d.style.top=r.box.top+"%";
    d.style.width=r.box.width+"%";d.style.height=r.box.height+"%";d.style.borderColor=COLOR[s.status];
    d.style.background=COLOR[s.status]+"22";d.title=(s.tier||"unparsed")+" — "+s.status;
    d.onclick=()=>{sel=r.id;renderAll();};ov.appendChild(d);});}
function badge(s){return '<span class="badge b-'+s+'">'+({confirmed:"LIKELY CORRECT",ok:"no issues",
  suspect:"SUSPECT",figure:"FIGURE (not a table)",unparsed:"not parsed",missing:"not detected",
  verified:"vision-verified"}[s])+'</span>';}
function renderRight(){const right=document.getElementById("right");let regs=regionsOn(page);
  if(sel)regs=regs.filter(r=>r.id===sel).concat(regs.filter(r=>r.id!==sel));
  right.innerHTML=regs.map(r=>{const s=st(r);
    if(s.status==="unparsed")return '<div class="card">'+badge("unparsed")+'<div class="parser">not parsed yet</div></div>';
    const sigs=(s.signals||[]).map(g=>'<div class="'+(g.positive?"sig-pos":"sig-neg")+'">'+(g.positive?"✓ ":"✗ ")+g.detail+'</div>').join("");
    const recon=s.recon==null?"":' &middot; reconstruction error '+s.recon;
    let det="";
    if(s.detail)det='<details><summary>reconstruction diff ('+s.detail.miss+' missing, '+s.detail.mis+' misplaced) — '+
      '<span style="color:#0a0">■matched</span> <span style="color:#e8820e">■misplaced</span> <span style="color:#d00">■missing</span></summary>'+
      '<img src="data:image/png;base64,'+s.detail.png+'"></details>';
    const tbl=s.html?'<div style="margin-top:8px;overflow-x:auto">'+s.html+'</div>':
      '<div class="parser" style="margin-top:8px">'+(s.note||"")+'</div>';
    const cost='<div class="costline">cost so far: '+s.time+'s &middot; $'+(s.cost||0).toFixed(2)+'</div>';
    return '<div class="card'+(sel===r.id?'" style="box-shadow:0 0 0 2px #ffd54a':"")+'">'+
      '<div class="parser"><b>'+(s.tier||"")+'</b>'+(s.rows?' &middot; '+s.rows+'×'+s.cols:"")+recon+'</div>'+
      badge(s.status)+cost+tbl+'<div style="margin-top:6px">'+sigs+'</div>'+det+'</div>';
  }).join("");}
function renderCost(){const c=D().cost;const flagged=D().regions.filter(r=>{
    const s1=r.stages[1];return s1 && (s1.status==="suspect"||s1.status==="figure"||s1.status==="missing");}).length;
  const lazyVision=D().regions.filter(r=>r.stages[r.stages.length-1].status==="verified").length;
  document.getElementById("cost").innerHTML=
    '<b>cost @ this stage</b> &middot; cheap whole doc '+c.cheap_total+'s, $0 &nbsp;|&nbsp; '+
    flagged+' of '+c.n_regions+' regions flagged → escalated lazily &nbsp;|&nbsp; '+
    'vision on '+lazyVision+' ambiguous = $'+(lazyVision*c.vision_rate).toFixed(2)+
    ' &nbsp;||&nbsp; <i>eager baseline:</i> Docling-everything '+c.eager_docling_time+'s, vision-everything $'+c.eager_vision_cost;}
function renderAll(){renderSel();renderTabs();renderPage();renderRight();renderCost();
  document.getElementById("stagelbl").textContent="Stage "+stage+": "+D().stages[Math.min(stage,D().stages.length-1)];}
document.getElementById("stage").oninput=e=>{stage=+e.target.value;renderAll();};
initDoc();renderAll();
</script></body></html>"""


if __name__ == "__main__":
    main()
