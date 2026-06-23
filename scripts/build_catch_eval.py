#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pypdfium2", "Pillow", "pdfplumber"]
# ///
"""Build the silent-failure catch-rate experiment (brief §2) — WITH SOURCE.

A table can only be labeled correct/wrong against its ground truth. So every
example is shown as **SOURCE ‖ EXTRACTION**:

  - synthetic: source = the intended-correct table; extraction = the (maybe
    corrupted) version. You diff them by eye.
  - corpus:    source = a rendered crop of the actual PDF region; extraction =
    what the cheap reconstructor pulled out. You check it against the page.

The SAME detectors (`quarry judge`) flag the extraction. The page hides each
verdict until you label, then computes the catch rate (of the tables YOU call
wrong, what fraction a detector flagged), false-alarm rate, and silent misses.

    uv run scripts/build_catch_eval.py
    open catch_eval/label.html
"""
import base64
import io
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "catch_eval")


def render_grid_html(grid, header_rows=1):
    import html as _h
    out = ["<table>"]
    for r, row in enumerate(grid):
        tag = "th" if r < header_rows else "td"
        out.append("<tr>" + "".join(f"<{tag}>{_h.escape(str(c))}</{tag}>" for c in row) + "</tr>")
    out.append("</table>")
    return "".join(out)


def _numbers(grid):
    import re
    from collections import Counter
    c = Counter()
    for row in grid:
        for cell in row:
            for tok in re.findall(r"\(?\d[\d,]*\)?", str(cell)):
                d = re.sub(r"[^\d]", "", tok)
                if d:
                    c[d] += 1
    return c


def failure_hint(cheap_grid, docling_grid):
    """A NEUTRAL annotation from the independent parse — helps the labeler look in
    the right place without dictating the answer (they still judge against source)."""
    if not docling_grid:
        return "no independent tier for this region"
    a, b = _numbers(cheap_grid), _numbers(docling_grid)
    if not a and not b:
        return "no numbers to compare"
    diff = sum((a - b).values()) + sum((b - a).values())
    if diff == 0:
        return f"docling agrees on all {sum(a.values())} numbers (values look right)"
    if sum(a.values()) * 2 < sum(b.values()):
        return f"cheap parsed {sum(a.values())} numbers, docling found {sum(b.values())} — possible fragment / missed rows"
    return f"{diff} number(s) differ between cheap parse and docling"


def cargo_judge(grids):
    """Run `quarry judge` on [{id, grid, header_rows, source}] → judged results."""
    gp = os.path.join(OUT, "_grids.json")
    json.dump(grids, open(gp, "w"))
    res = subprocess.run(["cargo", "run", "--quiet", "--", "judge", gp],
                         cwd=ROOT, capture_output=True, text=True)
    if res.returncode != 0:
        sys.exit(f"quarry judge failed:\n{res.stderr}")
    return {j["id"]: j for j in json.loads(res.stdout)}


# --------------------------------------------------------------------------- #
# Synthetic: each is (correct source, possibly-corrupted extraction). truth and
# kind are the DESIGNED answer, hidden from the labeler.
# --------------------------------------------------------------------------- #

def lattice_boxes(grid):
    """Cell bboxes on the shared lattice (matches lattice_geo's coordinate system),
    so two same-shape grids' cells overlap cell-for-cell for the geometry-keyed
    cross-tier comparator."""
    CW, RH = 100, 20
    nc = max(len(r) for r in grid)
    return [[[c * CW + 5, r * RH + 5, c * CW + 95, r * RH + 15] for c in range(nc)] for r in range(len(grid))]


def lattice_geo(correct, ext):
    """Lay the CORRECT grid's words on a coordinate lattice (the 'source') and the
    EXTRACTION grid's cells on the same lattice — so the reconstruction detector
    can see, against the detected region, any word the extraction dropped."""
    CW, RH = 100, 20
    words = []
    for r, row in enumerate(correct):
        for c, cell in enumerate(row):
            x0, y0 = c * CW + 5, r * RH + 5
            for i, w in enumerate(cell.split()):
                wx = x0 + i * 14
                words.append({"text": w, "bbox": [wx, y0, wx + 12, y0 + 10]})
    nr, nc = len(ext), max(len(r) for r in ext)
    boxes = [[[c * CW + 5, r * RH + 5, c * CW + 95, r * RH + 15] for c in range(nc)] for r in range(nr)]
    rr = max(nr, len(correct)) * RH
    cc = max(nc, max(len(r) for r in correct)) * CW
    return {"page": 1, "cell_boxes": boxes, "source_words": words, "region": [0, 0, cc, rr]}


def synthetic():
    S = []
    def add(id, src, ext, truth, kind, header_rows=1, geo=False, alt=False):
        item = {"id": id, "src": src, "ext": ext, "header_rows": header_rows,
                "source": "synthetic", "truth": truth, "kind": kind}
        if geo:
            item["geo"] = lattice_geo(src, ext)
        if alt:
            # An IDEALIZED independent tier (the correct parse) — measures the
            # UPPER BOUND of cross-tier agreement: what it recovers when the second
            # parser gets it right. The realistic bound is the corpus (pdfplumber).
            # Same-shape grids on the shared lattice so cells overlap cell-for-cell.
            item["alt_grid"] = src
            item["_ext_boxes"] = lattice_boxes(ext)
            item["_alt_boxes"] = lattice_boxes(src)
        S.append(item)
    def same(id, grid, kind, header_rows=1):
        add(id, grid, grid, "correct", kind, header_rows)

    # GOOD — source and extraction identical
    same("syn-good-total", [["Segment", "Revenue"], ["Parks", "100"], ["Studios", "220"], ["Total", "320"]], "good")
    same("syn-good-sections",
         [["Item", "Q1", "Q2"], ["Product", "10", "12"], ["Service", "5", "8"],
          ["Subtotal", "15", "20"], ["Other", "3", "4"], ["Total", "18", "24"]], "good-multisection")
    same("syn-good-percent",
         [["Segment", "Revenue", "Margin"], ["Parks", "100", "18%"], ["Studios", "220", "25%"], ["Total", "320", "22%"]],
         "good-ratio-col")
    same("syn-good-nototal", [["City", "Pop"], ["Austin", "961"], ["Dallas", "1304"], ["Houston", "2304"]], "good-no-total")

    # FALSE-ALARM TRAPS — correct, but tempt the detector
    same("syn-trap-placeholder",
         [["Segment", "2024", "2023"], ["Parks", "100", "—"], ["Studios", "220", "200"], ["Total", "320", "200"]],
         "trap-legit-placeholder")
    same("syn-trap-nonadditive",
         [["Segment", "Revenue", "Customers"], ["Parks", "100", "40"], ["Studios", "220", "35"], ["Total", "320", "60"]],
         "trap-nonadditive-total")
    same("syn-trap-section-rows",
         [["Item", "Amount"], ["Revenues:", ""], ["Product", "100"], ["Service", "50"],
          ["Costs:", ""], ["COGS", "80"], ["Total", "230"]], "trap-section-blanks")

    # WRONG, CATCHABLE — extraction breaks arithmetic or structure
    add("syn-bad-total",
        [["Segment", "Revenue"], ["Parks", "100"], ["Studios", "220"], ["Total", "320"]],
        [["Segment", "Revenue"], ["Parks", "100"], ["Studios", "220"], ["Total", "999"]], "wrong", "bad-total")
    add("syn-bad-straytext",
        [["Segment", "Revenue"], ["Parks", "100"], ["Studios", "220"], ["Other", "30"]],
        [["Segment", "Revenue"], ["Parks", "100"], ["Studios", "n/m oops"], ["Other", "30"]], "wrong", "stray-text")
    add("syn-bad-ragged",
        [["Item", "Q1", "Q2"], ["Product", "10", "12"], ["Service", "5", "8"], ["Other", "3", "4"]],
        [["Item", "Q1", "Q2"], ["Product", "10", "12"], ["Service", "5"], ["Other", "3", "4"]], "wrong", "ragged-cell")
    add("syn-bad-shifted",
        [["Segment", "Revenue", "Income"], ["Parks", "100", "20"], ["Studios", "220", "44"], ["Total", "320", "64"]],
        [["Segment", "Revenue", "Income"], ["Parks", "100", "20"], ["Studios", "", "220"], ["Total", "100", "240"]],
        "wrong", "shifted-row")

    # WRONG, SILENT — extraction stays internally consistent; only the source reveals it
    add("syn-silent-swaplabels",
        [["Segment", "Revenue"], ["Parks", "100"], ["Studios", "220"], ["Total", "320"]],
        [["Segment", "Revenue"], ["Parks", "220"], ["Studios", "100"], ["Total", "320"]], "wrong", "values-swapped", alt=True)
    add("syn-silent-nototal-typo",
        [["City", "Population"], ["Austin", "961"], ["Dallas", "1304"], ["Houston", "2304"]],
        [["City", "Population"], ["Austin", "961"], ["Dallas", "1384"], ["Houston", "2304"]], "wrong", "typo-no-total", alt=True)
    add("syn-silent-headerswap",
        [["Segment", "2023", "2024"], ["Parks", "100", "120"], ["Studios", "220", "240"], ["Total", "320", "360"]],
        [["Segment", "2024", "2023"], ["Parks", "100", "120"], ["Studios", "220", "240"], ["Total", "320", "360"]],
        "wrong", "headers-swapped", alt=True)
    add("syn-silent-coltranspose",
        [["Segment", "Revenue", "Income"], ["Parks", "100", "20"], ["Studios", "220", "44"], ["Total", "320", "64"]],
        [["Segment", "Revenue", "Income"], ["Parks", "20", "100"], ["Studios", "44", "220"], ["Total", "64", "320"]],
        "wrong", "columns-transposed", alt=True)
    add("syn-silent-dup-nototal",
        [["City", "Pop"], ["Austin", "961"], ["Dallas", "1304"], ["Houston", "2304"]],
        [["City", "Pop"], ["Austin", "961"], ["Austin", "961"], ["Houston", "2304"]], "wrong", "duplicated-row", alt=True)

    # WRONG, but a LOSS the reconstruction detector can see (it has source geometry):
    # arithmetic has no total to check and the grid looks structurally clean, yet a
    # word/row is gone from the source. These exercise the third detector's niche.
    add("syn-recon-droprow",
        [["City", "Population"], ["Austin", "961"], ["Dallas", "1304"], ["Houston", "2304"]],
        [["City", "Population"], ["Austin", "961"], ["Houston", "2304"]],
        "wrong", "dropped-row", geo=True)  # the whole Dallas row vanished
    add("syn-recon-dropunits",
        [["Metric", "Value"], ["Cash", "1,234 mn"], ["Debt", "987 mn"], ["Net", "247 mn"]],
        [["Metric", "Value"], ["Cash", "1,234"], ["Debt", "987"], ["Net", "247"]],
        "wrong", "dropped-units", geo=True)  # the "mn" unit dropped from every value
    return S


# --------------------------------------------------------------------------- #
# Corpus: parse real PDFs, render a crop of each table's region as the source.
# --------------------------------------------------------------------------- #

def render_crop(pdf, page_no, bbox, scale=3.0, pad=6):
    """Render PDF region (points, top-left) → base64 PNG data-uri."""
    page = pdf[page_no - 1]  # anchors are 1-based (pdfplumber page_number)
    w, h = page.get_size()
    bitmap = page.render(scale=scale)
    img = bitmap.to_pil()
    x0, y0, x1, y1 = bbox
    box = (max(0, (x0 - pad) * scale), max(0, (y0 - pad) * scale),
           min(w * scale, (x1 + pad) * scale), min(h * scale, (y1 + pad) * scale))
    crop = img.crop([int(v) for v in box])
    buf = io.BytesIO()
    crop.save(buf, format="PNG")
    return "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode()


def parser_b(plumber_page, region):
    """An INDEPENDENT second parse of the region — pdfplumber's table finder, a
    different algorithm from the Rust whitespace-gap reconstructor. Returns
    (grid, cell_boxes) in absolute page points (top-left), or (None, None) if it
    can't form a comparable table. cell_boxes is parallel to grid so the
    geometry-keyed cross-tier comparator can match cells by position."""
    w, h = plumber_page.width, plumber_page.height
    bbox = (max(0, region[0]), max(0, region[1]), min(w, region[2]), min(h, region[3]))
    if bbox[2] <= bbox[0] or bbox[3] <= bbox[1]:
        return None, None
    crop = plumber_page.crop(bbox, strict=False)
    for settings in ({"vertical_strategy": "text", "horizontal_strategy": "text"}, None):
        try:
            tables = crop.find_tables(table_settings=settings) if settings else crop.find_tables()
        except Exception:
            tables = []
        if not tables:
            continue
        t = max(tables, key=lambda T: (T.bbox[2] - T.bbox[0]) * (T.bbox[3] - T.bbox[1]))
        text_grid = t.extract()
        grid, boxes = [], []
        for i, rowobj in enumerate(t.rows):
            grow, brow = [], []
            for j, cb in enumerate(rowobj.cells):
                txt = ""
                if i < len(text_grid) and j < len(text_grid[i]) and text_grid[i][j]:
                    txt = str(text_grid[i][j]).replace("\n", " ").strip()
                grow.append(txt)
                brow.append([cb[0], cb[1], cb[2], cb[3]] if cb else [0.0, 0.0, 0.0, 0.0])
            grid.append(grow)
            boxes.append(brow)
        keep = [k for k, row in enumerate(grid) if any(c for c in row)]
        grid, boxes = [grid[k] for k in keep], [boxes[k] for k in keep]
        if len(grid) >= 2 and max(len(r) for r in grid) >= 2:
            return grid, boxes
    return None, None


def docling_b(pdf_path, page_no, region, _cache={}):
    """A GENUINELY independent second parse: Docling's ML layout/table model. Its
    failure modes don't correlate with the whitespace-gap reconstructor, so its
    agreement is real evidence and its disagreement surfaces real silent failures
    (unlike pdfplumber, which shares our method). We isolate the single page into
    its own PDF (Docling on a 300-page report is infeasible), run the existing
    sidecar, and reuse the crate's `import-docling` adapter for the bottom-left→
    top-left coordinate conversion. Returns (grid, cell_boxes) or (None, None)."""
    import pypdfium2 as pdfium
    key = (pdf_path, page_no)
    if key not in _cache:
        tmp_pdf = os.path.join(OUT, "_dl_page.pdf")
        src = pdfium.PdfDocument(pdf_path)
        dst = pdfium.PdfDocument.new()
        dst.import_pages(src, [page_no - 1])
        dst.save(tmp_pdf)
        tables = []
        r = subprocess.run(["uv", "run", "scripts/run_docling.py", tmp_pdf],
                           cwd=ROOT, capture_output=True, text=True)
        if r.returncode == 0 and r.stdout.strip():
            jf = os.path.join(OUT, "_dl.json")
            open(jf, "w").write(r.stdout)
            store = os.path.join(OUT, "_dl_store")
            subprocess.run(["rm", "-rf", store])
            ri = subprocess.run(["cargo", "run", "--quiet", "--", "import-docling", jf, "--out", store],
                                cwd=ROOT, capture_output=True, text=True)
            if ri.returncode == 0:
                man = json.load(open(os.path.join(store, "manifest.json")))
                for a in man["artifacts"]:
                    if a.get("kind") != "HtmlTable":
                        continue
                    nr, nc = a["n_rows"], a["n_cols"]
                    grid = [["" for _ in range(nc)] for _ in range(nr)]
                    boxes = [[[0.0, 0.0, 0.0, 0.0] for _ in range(nc)] for _ in range(nr)]
                    for c in a["cells"]:
                        if c["row"] < nr and c["col"] < nc:
                            grid[c["row"]][c["col"]] = c["text"]
                            an = c["anchor"]
                            if isinstance(an, dict) and an.get("format") == "pdf":
                                b = an["bbox"]
                                boxes[c["row"]][c["col"]] = [b["x0"], b["y0"], b["x1"], b["y1"]]
                    prov = a["meta"]["provenance"].get("Source", {}).get("bbox")
                    tables.append((prov, grid, boxes))
            else:
                print(f"  import-docling failed p{page_no}: {ri.stderr[:160]}", file=sys.stderr)
        else:
            print(f"  docling failed p{page_no}: {r.stderr[-160:]}", file=sys.stderr)
        _cache[key] = tables

    def overlap(b):
        if not b:
            return 0.0
        x0, y0 = max(b["x0"], region[0]), max(b["y0"], region[1])
        x1, y1 = min(b["x1"], region[2]), min(b["y1"], region[3])
        return max(0.0, x1 - x0) * max(0.0, y1 - y0)

    best, ba = (None, None), 0.0
    for prov, grid, boxes in _cache[key]:
        a = overlap(prov)
        if a > ba:
            ba, best = a, (grid, boxes)
    return best if ba > 0 else (None, None)


def corpus_examples(pdfs, per_pdf=4, total=10):
    import pdfplumber
    import pypdfium2 as pdfium
    out = []
    for rel in pdfs:
        if len(out) >= total:
            break
        pdf_path = os.path.join(ROOT, rel)
        if not os.path.exists(pdf_path):
            continue
        qd = os.path.join(OUT, "_corpus.qdoc")
        if subprocess.run(["uv", "run", "scripts/pdf_to_qdoc.py", pdf_path, "-o", qd],
                          cwd=ROOT, capture_output=True, text=True).returncode != 0:
            continue
        store = os.path.join(OUT, "_corpus_store")
        subprocess.run(["rm", "-rf", store])
        if subprocess.run(["cargo", "run", "--quiet", "--", "parse", qd, "--out", store],
                          cwd=ROOT, capture_output=True, text=True).returncode != 0:
            continue
        man = json.load(open(os.path.join(store, "manifest.json")))
        qdoc = json.load(open(qd))
        pages_by_no = {p["page"]: p for p in qdoc["pages"]}
        pdf = pdfium.PdfDocument(pdf_path)
        plumber = pdfplumber.open(pdf_path)
        name = os.path.splitext(os.path.basename(pdf_path))[0]
        picked = 0
        for a in man["artifacts"]:
            if a.get("kind") != "HtmlTable" or picked >= per_pdf or len(out) >= total:
                continue
            nr, nc = a["n_rows"], a["n_cols"]
            if not (3 <= nr <= 12 and 2 <= nc <= 8):
                continue
            grid = [["" for _ in range(nc)] for _ in range(nr)]
            boxes = [[[0.0, 0.0, 0.0, 0.0] for _ in range(nc)] for _ in range(nr)]
            hdr, page_no = set(), None
            xs, ys, xe, ye = [], [], [], []
            for c in a["cells"]:
                an = c["anchor"]
                if c["row"] < nr and c["col"] < nc:
                    grid[c["row"]][c["col"]] = c["text"]
                    if c.get("is_header"):
                        hdr.add(c["row"])
                    if isinstance(an, dict) and an.get("format") == "pdf":
                        b = an["bbox"]
                        boxes[c["row"]][c["col"]] = [b["x0"], b["y0"], b["x1"], b["y1"]]
                if isinstance(an, dict) and an.get("format") == "pdf":
                    page_no = an["page"]; b = an["bbox"]
                    xs.append(b["x0"]); ys.append(b["y0"]); xe.append(b["x1"]); ye.append(b["y1"])
            if page_no is None or not xs:
                continue
            header_rows = 0
            while header_rows in hdr:
                header_rows += 1
            # The DETECTED region (provenance bbox = the area handed to the parser).
            # Scoping the reconstruction comparison to this — not the captured-cell
            # union — is what lets dropped words show up as residual.
            prov = a["meta"]["provenance"].get("Source", {})
            rb = prov.get("bbox")
            if not rb:
                continue
            region = (rb["x0"], rb["y0"], rb["x1"], rb["y1"])
            words = []
            for s in pages_by_no.get(page_no, {}).get("spans", []):
                b = s["bbox"]
                cx, cy = (b[0] + b[2]) / 2, (b[1] + b[3]) / 2
                if region[0] <= cx <= region[2] and region[1] <= cy <= region[3]:
                    words.append({"text": s["text"], "bbox": b})
            # tier-B = Docling (genuinely independent ML parse); pdfplumber only as
            # a last-resort fallback when Docling yields nothing for a region.
            try:
                alt_grid, alt_boxes = docling_b(pdf_path, page_no, region)
            except Exception as e:
                print(f"  docling_b failed {name} p{page_no}: {e}", file=sys.stderr)
                alt_grid, alt_boxes = None, None
            if not alt_grid:
                try:
                    alt_grid, alt_boxes = parser_b(plumber.pages[page_no - 1], region)
                except Exception as e:
                    print(f"  parser_b fallback failed {name} p{page_no}: {e}", file=sys.stderr)
                    alt_grid, alt_boxes = None, None
            # Crop the UNION of both parsers' table extents, so the source pane shows
            # the FULL table even when the cheap parse under-scoped its region (the
            # dominant real failure). The cheap `region` still scopes recon/words.
            crop_region = list(region)
            for row in (alt_boxes or []):
                for b in row:
                    if b[2] > b[0] and b[3] > b[1]:
                        crop_region = [min(crop_region[0], b[0]), min(crop_region[1], b[1]),
                                       max(crop_region[2], b[2]), max(crop_region[3], b[3])]
            try:
                img = render_crop(pdf, page_no, crop_region)
            except Exception as e:
                print(f"  crop failed {name} p{page_no}: {e}", file=sys.stderr)
                continue
            out.append({"id": f"corpus-{name}-{picked}", "grid": grid,
                        "header_rows": max(1, header_rows), "source": f"corpus:{name}",
                        "src_img": img, "truth": None, "kind": "corpus",
                        "page": page_no, "cell_boxes": boxes,
                        "source_words": words, "region": list(region),
                        "alt_grid": alt_grid or [], "alt_cell_boxes": alt_boxes or [],
                        "alt_page": page_no})
            picked += 1
    return out


# --------------------------------------------------------------------------- #

CORPUS_PDFS = [
    "input/finance/jpm-2023-ar.pdf",
    "input/finance/brk-2023-ar.pdf",
    "input/finance/gpmr2026-private-equity-clearer-view-tougher-terrain_final_v10.pdf",
    "input/finance/RBRTEd.pdf",
    "input/finance/2025ltr.pdf",
]


def main():
    os.makedirs(OUT, exist_ok=True)
    syn = synthetic()
    cor = corpus_examples(CORPUS_PDFS, per_pdf=6, total=24)

    # Judge synthetic source + extraction (render both with the same renderer) and
    # corpus extraction. Source HTML for synthetic comes back labeled "::src".
    gj = []
    for s in syn:
        gj.append({"id": s["id"] + "::src", "grid": s["src"], "header_rows": s["header_rows"], "source": "syn"})
        ext = {"id": s["id"] + "::ext", "grid": s["ext"], "header_rows": s["header_rows"], "source": "syn"}
        ext.update(s.get("geo", {}))  # source geometry → reconstruction detector runs
        if s.get("alt_grid"):  # idealized independent tier → cross-tier runs
            ext["alt_grid"] = s["alt_grid"]
            ext["alt_header_rows"] = s["header_rows"]
            ext["page"] = 1
            ext["cell_boxes"] = s["_ext_boxes"]
            ext["alt_cell_boxes"] = s["_alt_boxes"]
            ext["alt_page"] = 1
        gj.append(ext)
    for c in cor:
        gj.append({"id": c["id"], "grid": c["grid"], "header_rows": c["header_rows"], "source": c["source"],
                   "page": c["page"], "cell_boxes": c["cell_boxes"], "source_words": c["source_words"],
                   "region": c["region"], "alt_grid": c.get("alt_grid", []),
                   "alt_cell_boxes": c.get("alt_cell_boxes", []), "alt_page": c.get("alt_page", 0)})
    judged = cargo_judge(gj)

    dataset = []
    for s in syn:
        ext = judged[s["id"] + "::ext"]
        dataset.append({
            "id": s["id"], "source": s["source"], "kind": s["kind"], "truth": s["truth"],
            "src_kind": "table", "src_html": judged[s["id"] + "::src"]["html"],
            "ext_html": ext["html"], "flagged": ext["flagged"], "signals": ext["signals"],
        })
    for c in cor:
        ext = judged[c["id"]]
        dataset.append({
            "id": c["id"], "source": c["source"], "kind": c["kind"], "truth": c["truth"],
            "src_kind": "image", "src_img": c["src_img"],
            "ext_html": ext["html"], "flagged": ext["flagged"], "signals": ext["signals"],
            # The genuinely-independent parse, shown as a third pane so the real
            # failure (value? structure? region scope?) is judgeable, not inferred.
            "docling_html": render_grid_html(c["alt_grid"]) if c.get("alt_grid") else "",
        })

    json.dump(dataset, open(os.path.join(OUT, "dataset.json"), "w"))
    open(os.path.join(OUT, "label.html"), "w").write(PAGE.replace("/*DATA*/", json.dumps(dataset)))
    nflag = sum(1 for d in dataset if d["flagged"])
    print(f"built {len(dataset)} examples ({len(syn)} synthetic, {len(cor)} corpus); {nflag} flagged by detectors")
    print(f"open file://{os.path.join(OUT, 'label.html')}")

    # Re-grounding page: REAL corpus tables, classified by what actually breaks,
    # with detector coverage measured per real-failure class.
    classify = []
    for c in cor:
        ext = judged[c["id"]]
        classify.append({
            "id": c["id"], "source": c["source"],
            "src_img": c["src_img"],
            "ext_html": ext["html"],
            "docling_html": render_grid_html(c["alt_grid"]) if c.get("alt_grid") else "",
            "flagged": ext["flagged"],
            "signals": ext["signals"],
            "hint": failure_hint(c["grid"], c.get("alt_grid", [])),
        })
    json.dump(classify, open(os.path.join(OUT, "corpus_dataset.json"), "w"))
    open(os.path.join(OUT, "corpus_label.html"), "w").write(CLASSIFY_PAGE.replace("/*CDATA*/", json.dumps(classify)))
    print(f"re-grounding: {len(classify)} real corpus tables → open file://{os.path.join(OUT, 'corpus_label.html')}")


PAGE = r"""<!doctype html><html><head><meta charset="utf-8"><title>Catch-rate (with source)</title>
<style>
:root{--ink:#171717;--line:#e7e5e4;--g:#1a7a4f;--r:#b4453a;--amber:#b07515}
*{box-sizing:border-box}
body{margin:0;font-family:Geist,-apple-system,system-ui,sans-serif;color:var(--ink);background:#faf9f7}
header{position:sticky;top:0;background:rgba(250,249,247,.96);backdrop-filter:blur(6px);border-bottom:1px solid var(--line);padding:12px 20px;z-index:5}
h1{font-size:15px;margin:0 0 8px}
.metrics{display:flex;gap:18px;flex-wrap:wrap;font-size:13px;align-items:baseline}
.metric b{font-size:18px}.big{font-size:22px!important}.muted{color:#6b6b6b}
main{max-width:1080px;margin:0 auto;padding:18px}
.card{border:1px solid var(--line);border-radius:12px;background:#fff;margin:0 0 16px;padding:14px;box-shadow:0 1px 2px rgba(0,0,0,.03)}
.cardhd{display:flex;gap:10px;margin-bottom:10px;font-size:12px;color:#6b6b6b}
.cols{display:grid;grid-template-columns:1fr 1fr;gap:14px}
.col h4{margin:0 0 6px;font-size:11px;letter-spacing:.04em;text-transform:uppercase;color:#9a9a9a}
.pane{border:1px solid var(--line);border-radius:8px;overflow:auto;background:#fff}
.pane img{display:block;max-width:100%}
table{border-collapse:collapse;font-size:13px;width:100%}
td,th{border-bottom:1px solid #f0efed;border-right:1px solid #f0efed;padding:5px 10px;text-align:left;white-space:nowrap}
th{background:#f6f6f5;font-weight:600}
.btns{display:flex;gap:8px;margin:12px 0 0;align-items:center}
button{font:inherit;font-size:13px;padding:6px 14px;border-radius:8px;border:1px solid var(--line);background:#fff;cursor:pointer}
.lab.correct.on{background:var(--g);color:#fff;border-color:var(--g)}
.lab.wrong.on{background:var(--r);color:#fff;border-color:var(--r)}
.right{margin-left:auto}
.verdict{margin-top:10px;font-size:13px;border-top:1px dashed var(--line);padding-top:8px}
.flag{color:var(--r)}.pass{color:var(--g)}
.sig{font-family:ui-monospace,monospace;font-size:11px;color:var(--amber);margin-top:2px}
.hidden{display:none}small{color:#9a9a9a}
</style></head><body>
<header>
 <h1>Catch-rate — does the <b>EXTRACTION</b> (right) faithfully represent the <b>SOURCE</b> (left)? Label correct / wrong.</h1>
 <div class="metrics" id="metrics"></div>
</header>
<main id="main"></main>
<script>
const DATA = /*DATA*/;
const KEY = "catch_eval_src_v1";
let labels = JSON.parse(localStorage.getItem(KEY) || "{}");
const order = DATA.map(d=>d).sort((a,b)=>{
  const h=s=>{let x=0;for(const c of s)x=(x*31+c.charCodeAt(0))>>>0;return x;};
  return h(a.id)-h(b.id);
});
function setLabel(id,v){ labels[id]= labels[id]===v? null : v; localStorage.setItem(KEY,JSON.stringify(labels)); render(); }
function reveal(id){ document.getElementById("v-"+id).classList.remove("hidden"); }
function metrics(){
  let caught=0,missed=0,fa=0,clean=0,labeled=0;
  for(const d of DATA){ const l=labels[d.id]; if(!l)continue; labeled++;
    if(l==="wrong"){ d.flagged?caught++:missed++; } else { d.flagged?fa++:clean++; } }
  const wrong=caught+missed, correct=fa+clean;
  return {caught,missed,fa,clean,labeled,wrong,correct,
    cr: wrong?Math.round(100*caught/wrong):null, far: correct?Math.round(100*fa/correct):null};
}
function renderMetrics(){
  const m=metrics();
  document.getElementById("metrics").innerHTML =
   `<div class="metric">labeled <b>${m.labeled}</b><span class="muted">/${DATA.length}</span></div>`+
   `<div class="metric">CATCH RATE <b class="big" style="color:var(--g)">${m.cr==null?"–":m.cr+"%"}</b> <span class="muted">of ${m.wrong} wrong, flagged</span></div>`+
   `<div class="metric">silent misses <b style="color:var(--r)">${m.missed}</b></div>`+
   `<div class="metric">false alarms <b style="color:var(--amber)">${m.fa}</b> <span class="muted">(${m.far==null?"–":m.far+"%"} of ${m.correct} good)</span></div>`+
   `<div class="metric right"><button onclick="exportLabels()">export labels</button></div>`;
}
function exportLabels(){
  const rows = DATA.map(d=>({id:d.id,source:d.source,kind:d.kind,truth:d.truth,flagged:d.flagged,label:labels[d.id]||null}));
  const a=document.createElement("a");a.href=URL.createObjectURL(new Blob([JSON.stringify(rows,null,2)],{type:"application/json"}));
  a.download="catch_eval_labels.json";a.click();
}
function srcPane(d){ return d.src_kind==="image" ? `<div class="pane"><img src="${d.src_img}"></div>` : `<div class="pane">${d.src_html}</div>`; }
function render(){
  renderMetrics();
  document.getElementById("main").innerHTML = order.map(d=>{
    const l=labels[d.id];
    const verdict = `<div id="v-${d.id}" class="verdict ${l?'':'hidden'}">`+
      (d.flagged?`<span class="flag">⚑ detector FLAGGED the extraction</span>`:`<span class="pass">✓ detector passed the extraction</span>`)+
      d.signals.map(s=>`<div class="sig">${s.check}: ${s.reason}</div>`).join("")+`</div>`;
    let panes = `<div class="col"><h4>source</h4>${srcPane(d)}</div>`+
      `<div class="col"><h4>cheap parse (under test)</h4><div class="pane">${d.ext_html}</div></div>`;
    if (d.docling_html) panes += `<div class="col"><h4>docling (independent)</h4><div class="pane">${d.docling_html}</div></div>`;
    const ncol = d.docling_html ? 3 : 2;
    return `<div class="card"><div class="cardhd"><span>${d.source}</span><small>${d.id}</small></div>`+
      `<div class="cols" style="grid-template-columns:repeat(${ncol},1fr)">${panes}</div>`+
      `<div class="btns">`+
        `<button class="lab correct ${l==='correct'?'on':''}" onclick="setLabel('${d.id}','correct')">✓ correct</button>`+
        `<button class="lab wrong ${l==='wrong'?'on':''}" onclick="setLabel('${d.id}','wrong')">✗ wrong</button>`+
        (l?``:`<button onclick="reveal('${d.id}')" class="right">peek verdict</button>`)+
      `</div>${verdict}</div>`;
  }).join("");
}
render();
</script></body></html>"""


CLASSIFY_PAGE = r"""<!doctype html><html><head><meta charset="utf-8"><title>Real corpus failures</title>
<style>
:root{--ink:#171717;--line:#e7e5e4;--g:#1a7a4f;--r:#b4453a;--amber:#b07515;--blue:#2b6cb0}
*{box-sizing:border-box}
body{margin:0;font-family:Geist,-apple-system,system-ui,sans-serif;color:var(--ink);background:#faf9f7}
header{position:sticky;top:0;background:rgba(250,249,247,.97);backdrop-filter:blur(6px);border-bottom:1px solid var(--line);padding:12px 20px;z-index:5}
h1{font-size:15px;margin:0 0 8px}
.metrics{font-size:13px;line-height:1.5}
.metrics table{border-collapse:collapse;margin-top:4px}
.metrics td,.metrics th{border:1px solid var(--line);padding:3px 10px;text-align:left;font-size:12px}
.muted{color:#6b6b6b}
main{max-width:96vw;margin:0 auto;padding:18px}
.card{border:1px solid var(--line);border-radius:12px;background:#fff;margin:0 0 16px;padding:14px;box-shadow:0 1px 2px rgba(0,0,0,.03)}
.cardhd{display:flex;gap:10px;margin-bottom:6px;font-size:12px;color:#6b6b6b;align-items:center}
.hint{font-size:12px;color:var(--blue);margin-bottom:8px}
.cols{display:grid;grid-template-columns:1fr 1fr 1fr;gap:12px}
.col h4{margin:0 0 6px;font-size:11px;letter-spacing:.04em;text-transform:uppercase;color:#9a9a9a}
.pane{border:1px solid var(--line);border-radius:8px;overflow:auto;background:#fff;max-height:620px}
.srcimg{display:block;width:100%;cursor:zoom-in;background:#fff}
table.t{border-collapse:collapse;font-size:12px;width:100%}
.t td,.t th{border-bottom:1px solid #f0efed;border-right:1px solid #f0efed;padding:4px 8px;text-align:left;white-space:nowrap}
.t th{background:#f6f6f5;font-weight:600}
.btns{display:flex;gap:8px;margin:12px 0 0;flex-wrap:wrap;align-items:center}
button{font:inherit;font-size:13px;padding:6px 12px;border-radius:8px;border:1px solid var(--line);background:#fff;cursor:pointer}
.cls.on[data-k$="-ok"]{background:var(--g);color:#fff;border-color:var(--g)}
.cls.on[data-k$="-bad"]{background:var(--r);color:#fff;border-color:var(--r)}
.cls.on[data-k$="-na"]{background:#777;color:#fff;border-color:#777}
.qlabel{font-size:12px;color:#6b6b6b;margin-right:6px;min-width:120px;display:inline-block}
input[type=range]{width:180px;vertical-align:middle;cursor:pointer}
.rv{font-size:13px;font-weight:600;margin-left:10px;font-variant-numeric:tabular-nums;min-width:34px;display:inline-block}
.scale{font-size:11px;color:#9a9a9a;margin:2px 0 8px 126px}
#zoom{position:fixed;inset:0;background:rgba(0,0,0,.88);display:none;z-index:99;overflow:auto;padding:24px;cursor:zoom-out;text-align:center}
#zoom img{max-width:none;background:#fff;box-shadow:0 4px 24px rgba(0,0,0,.5)}
.flagchip{margin-left:auto;font-size:11px;font-family:ui-monospace,monospace}
.flag{color:var(--r)}.pass{color:var(--g)}
.right{margin-left:auto}
</style></head><body>
<header>
 <h1>Real corpus failures — classify what the <b>cheap parse</b> got wrong vs the <b>source</b> (docling shown as a second opinion).</h1>
 <div class="metrics" id="metrics"></div>
</header>
<main id="main"></main>
<div id="zoom" onclick="this.style.display='none'"><img id="zoomimg"></div>
<script>
const DATA = /*CDATA*/;
const KEY = "corpus_rate_v1";
function zoomImg(s){ document.getElementById('zoomimg').src=s; document.getElementById('zoom').style.display='block'; }
let L = JSON.parse(localStorage.getItem(KEY) || "{}");
const get = id => L[id] || {};
function setv(id,axis,v){ v=+v; const o=get(id); o[axis]=v; L[id]=o; localStorage.setItem(KEY,JSON.stringify(L));
  const el=document.getElementById('rv-'+id+'-'+axis); if(el) el.textContent=v+'/5';
  renderMetrics(); }
function metrics(){
  let nC=0,sumC=0, joint=0,sumCj=0,sumDj=0, win=0,reg=0;
  const bk={}; for(let i=0;i<=5;i++) bk[i]={n:0,flagged:0};
  let badN=0,badF=0,goodN=0,goodF=0;
  for(const d of DATA){ const o=get(d.id); const c=o.cheap, dl=o.docling;
    if(c!==undefined){ nC++; sumC+=c; bk[c].n++; if(d.flagged)bk[c].flagged++;
      if(c<=2){ badN++; if(d.flagged)badF++; } if(c>=4){ goodN++; if(d.flagged)goodF++; } }
    if(c!==undefined && dl!==undefined){ joint++; sumCj+=c; sumDj+=dl; const delta=dl-c;
      if(delta>=2)win++; else if(delta<0)reg++; } }
  return {nC,sumC,joint,sumCj,sumDj,win,reg,bk,badN,badF,goodN,goodF};
}
function renderMetrics(){
  const m=metrics();
  const mcj=m.joint?m.sumCj/m.joint:null, mdj=m.joint?m.sumDj/m.joint:null;
  const lift=(mcj==null)?null:mdj-mcj;
  const recall=m.badN?Math.round(100*m.badF/m.badN):null, fa=m.goodN?Math.round(100*m.goodF/m.goodN):null;
  let brows=""; for(let i=5;i>=0;i--){ const b=m.bk[i]; if(!b.n)continue;
    brows+=`<tr><td>cheap = ${i}</td><td>${b.n}</td><td>${b.flagged}/${b.n} (${Math.round(100*b.flagged/b.n)}%)</td></tr>`; }
  document.getElementById("metrics").innerHTML =
    `<b>${m.nC}</b>/${DATA.length} cheap-rated`+
    `<div class="right" style="float:right"><button onclick="exportLabels()">export</button></div>`+
    `<table><tr><th colspan=2>1. Is docling better? (${m.joint} jointly rated, 0–5)</th></tr>`+
      `<tr><td>mean quality — cheap</td><td><b>${mcj==null?'–':mcj.toFixed(2)}</b></td></tr>`+
      `<tr><td>mean quality — docling</td><td><b>${mdj==null?'–':mdj.toFixed(2)}</b></td></tr>`+
      `<tr><td><b>quality lift</b> (docling − cheap)</td><td><b style="color:var(--g)">${lift==null?'–':(lift>=0?'+':'')+lift.toFixed(2)}</b></td></tr>`+
      `<tr><td>docling clearly better (≥ +2 pts)</td><td>${m.win}</td></tr>`+
      `<tr><td>docling regressed (worse than cheap)</td><td style="color:var(--r)">${m.reg}</td></tr>`+
    `</table>`+
    `<table><tr><th>2. Do detector flags track LOW cheap quality?</th><th>tables</th><th>flagged</th></tr>`+
      brows+
      `<tr><td>recall — cheap ≤2, % flagged</td><td colspan=2><b style="color:var(--g)">${recall==null?'–':recall+'%'}</b> (${m.badF}/${m.badN})</td></tr>`+
      `<tr><td>false alarm — cheap ≥4, % flagged</td><td colspan=2><b style="color:var(--amber)">${fa==null?'–':fa+'%'}</b> (${m.goodF}/${m.goodN})</td></tr>`+
    `</table>`+
    `<div class="muted">Flags concentrating at low cheap ratings (high recall, low false alarm) ⇒ a free check can gate escalation.</div>`;
}
function exportLabels(){
  const rows = DATA.map(d=>({id:d.id,source:d.source,flagged:d.flagged,signals:d.signals.map(s=>s.check),hint:d.hint,...get(d.id)}));
  const a=document.createElement("a");a.href=URL.createObjectURL(new Blob([JSON.stringify(rows,null,2)],{type:"application/json"}));
  a.download="corpus_rate_labels.json";a.click();
}
function rrow(d,axis,label){ const o=get(d.id); const v=o[axis];
  return `<div class="btns"><span class="qlabel">${label}</span>`+
    `<input type=range min=0 max=5 step=1 value="${v===undefined?0:v}" oninput="setv('${d.id}','${axis}',this.value)" onclick="setv('${d.id}','${axis}',this.value)">`+
    `<span class="rv" id="rv-${d.id}-${axis}">${v===undefined?'–':v+'/5'}</span></div>`; }
function render(){
  renderMetrics();
  document.getElementById("main").innerHTML = DATA.map(d=>{
    const chip = d.flagged ? `<span class="flagchip flag">⚑ ${d.signals.map(s=>s.check.split('_')[0]).join(',')}</span>` : `<span class="flagchip pass">✓ no detector fired</span>`;
    const dl = d.docling_html ? `<div class="col"><h4>docling (independent)</h4><div class="pane">${d.docling_html.replace('<table>','<table class=t>')}</div></div>` : `<div class="col"><h4>docling</h4><div class="pane muted" style="padding:8px">none</div></div>`;
    const src = d.src_img ? `<div class="pane"><img class="srcimg" src="${d.src_img}" onclick="zoomImg(this.src)"></div>` : '<div class="pane muted" style="padding:8px">no crop</div>';
    return `<div class="card"><div class="cardhd"><span>${d.source}</span><small>${d.id}</small>${chip}</div>`+
      `<div class="hint">↳ ${d.hint}</div>`+
      `<div class="cols">`+
        `<div class="col"><h4>source (click to enlarge)</h4>${src}</div>`+
        `<div class="col"><h4>cheap parse (under test)</h4><div class="pane">${d.ext_html.replace('<table>','<table class=t>')}</div></div>`+
        dl+
      `</div>`+
      rrow(d,'cheap','cheap quality:')+
      rrow(d,'docling','docling quality:')+
      `<div class="scale">0 unusable · 1 · 2 major errors · 3 usable w/ errors · 4 minor issues · 5 faithful</div>`+
      `</div>`;
  }).join("");
}
render();
</script></body></html>"""


if __name__ == "__main__":
    main()
