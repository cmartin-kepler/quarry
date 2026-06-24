//! Adapter: Docling JSON → Quarry `Artifact`s.
//!
//! This is the integration pattern for a real, table-producing parser (Docling,
//! Reducto, LlamaParse, …). Such parsers ARE the parser *and* the table
//! reconstructor — they emit tables with cells and bounding boxes — so they
//! bypass `.qdoc` and the cheap `PdfTextLayerReconstructor` entirely. The only
//! glue needed is this thin per-parser mapping onto the `Artifact` /
//! `SourceAnchor` model, after which the detector / adjudicator / eval core runs
//! unchanged.
//!
//! Docling emits bboxes with a BOTTOMLEFT origin by default; we flip to the
//! top-left convention `SourceAnchor`/`.qdoc` use, using the page height.

use crate::artifact::*;
use crate::core::*;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

// ---- Minimal view of the DoclingDocument JSON (unknown fields ignored) ----

#[derive(Deserialize)]
struct DoclingDoc {
    #[serde(default)]
    pages: HashMap<String, DPage>,
    #[serde(default)]
    tables: Vec<DTable>,
    #[serde(default)]
    texts: Vec<DText>,
    #[serde(default)]
    groups: Vec<DGroup>,
    #[serde(default)]
    pictures: Vec<DPicture>,
    #[serde(default)]
    body: DBody,
}

/// A picture/figure the layout model located. With `do_ocr=True` docling attaches the
/// figure's OCR'd text as `children` (refs to `#/texts/N`) — so the reading-order walk
/// recurses into it to capture figure text. `prov` is the figure's box (→ `Region`).
#[derive(Deserialize, Default)]
struct DPicture {
    #[serde(default)]
    prov: Vec<DProv>,
    #[serde(default)]
    children: Vec<DRef>,
}

/// A nesting group (a list, a multi-column section, …) — its `children` are more
/// refs (texts / sub-groups), so the reading-order walk recurses through it.
#[derive(Deserialize, Default)]
struct DGroup {
    #[serde(default)]
    children: Vec<DRef>,
}

#[derive(Deserialize)]
struct DText {
    #[serde(default)]
    label: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    prov: Vec<DProv>,
}

/// The document body: its `children` are refs (`#/texts/0`, `#/tables/0`, …) in
/// READING ORDER — docling's own linear document structure.
#[derive(Deserialize, Default)]
struct DBody {
    #[serde(default)]
    children: Vec<DRef>,
}

#[derive(Deserialize)]
struct DRef {
    #[serde(default, alias = "$ref")]
    cref: String,
}

#[derive(Deserialize)]
struct DPage {
    size: DSize,
}

#[derive(Deserialize)]
struct DSize {
    #[allow(dead_code)]
    width: f32,
    height: f32,
}

#[derive(Deserialize)]
struct DTable {
    #[serde(default)]
    prov: Vec<DProv>,
    data: DData,
}

#[derive(Deserialize)]
struct DProv {
    page_no: u32,
    bbox: DBBox,
}

#[derive(Deserialize)]
struct DData {
    num_rows: u32,
    num_cols: u32,
    table_cells: Vec<DCell>,
}

#[derive(Deserialize)]
struct DCell {
    bbox: Option<DBBox>,
    text: String,
    start_row_offset_idx: u32,
    start_col_offset_idx: u32,
    #[serde(default)]
    column_header: bool,
}

#[derive(Deserialize)]
struct DBBox {
    l: f32,
    t: f32,
    r: f32,
    b: f32,
    #[serde(default)]
    coord_origin: String,
}

impl DBBox {
    /// Convert to the top-left convention used throughout Quarry.
    fn to_topleft(&self, page_h: f32) -> BBox {
        match self.coord_origin.as_str() {
            // t/b are measured up from the page bottom (t is the higher edge).
            "BOTTOMLEFT" => BBox::new(self.l, page_h - self.t, self.r, page_h - self.b),
            _ => BBox::new(self.l, self.t, self.r, self.b),
        }
    }
}

/// Promote docling's full layout-detection list to first-class `Region` artifacts —
/// one per located element (text block, table, picture), each carrying its page +
/// bbox + label. These are the layout model's raw output: queryable ("all boxes on
/// page 10"), and the input a region-scoped extractor / cropper reads. Opt-in
/// (verbose), separate from the extracted tables/text.
pub fn regions_from_docling(json: &str, doc: DocHash, generation: Generation) -> Result<Vec<Region>> {
    let dd: DoclingDoc = serde_json::from_str(json).context("parsing Docling JSON")?;
    let mut out = Vec::new();
    let mut push = |prov: &[DProv], label: &str| {
        if let Some(p) = prov.first() {
            let page_h =
                dd.pages.get(&p.page_no.to_string()).map(|x| x.size.height).unwrap_or(792.0);
            out.push(region_artifact(doc, p.page_no, p.bbox.to_topleft(page_h), label, generation));
        }
    };
    for t in &dd.texts {
        push(&t.prov, &t.label);
    }
    for t in &dd.tables {
        push(&t.prov, "table");
    }
    for p in &dd.pictures {
        push(&p.prov, "picture");
    }
    Ok(out)
}

/// Build one `Region` (Source-anchored at its bbox), content-addressed by
/// `(label, page, bbox)` so the same detection mints the same id.
fn region_artifact(doc: DocHash, page: u32, bbox: BBox, label: &str, generation: Generation) -> Region {
    let key = format!("region:{label}:{page}:{}:{}:{}:{}", bbox.x0, bbox.y0, bbox.x1, bbox.y1);
    let content = DocHash::of(key.as_bytes());
    Region {
        meta: Meta {
            id: ArtifactId::mint(&content, generation),
            content_hash: content,
            provenance: Provenance::Source(SourceAnchor::Pdf { doc, page, bbox }),
            generation,
            risk: RiskMarkers::default(),
            origin: Origin::default(),
        },
        label: label.to_string(),
        confidence: 1.0,
    }
}

/// Parse Docling JSON into Quarry artifacts. `doc` is the document identity
/// (hash of the original PDF bytes); `generation` the per-document job counter.
pub fn artifacts_from_docling(
    json: &str,
    doc: DocHash,
    generation: Generation,
) -> Result<Vec<Box<dyn Artifact>>> {
    let dd: DoclingDoc = serde_json::from_str(json).context("parsing Docling JSON")?;
    let mut out: Vec<Box<dyn Artifact>> = Vec::new();

    for table in &dd.tables {
        let prov = table.prov.first();
        let page = prov.map(|p| p.page_no).unwrap_or(1);
        let page_h = dd
            .pages
            .get(&page.to_string())
            .map(|p| p.size.height)
            .unwrap_or(792.0);

        let n_rows = table.data.num_rows;
        let n_cols = table.data.num_cols;

        let cells: Vec<Cell> = table
            .data
            .table_cells
            .iter()
            .map(|c| {
                let bbox = c
                    .bbox
                    .as_ref()
                    .map(|b| b.to_topleft(page_h))
                    .unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0));
                Cell {
                    row: c.start_row_offset_idx,
                    col: c.start_col_offset_idx,
                    text: c.text.clone(),
                    anchor: SourceAnchor::Pdf { doc, page, bbox },
                    is_header: c.column_header,
                }
            })
            .collect();

        let html = render_html(&cells, n_rows, n_cols);
        let risk = risk_from(&cells, n_rows, n_cols);
        let content = DocHash::of(html.as_bytes());
        let table_bbox = prov
            .map(|p| p.bbox.to_topleft(page_h))
            .unwrap_or(BBox::new(0.0, 0.0, 0.0, page_h));

        out.push(Box::new(HtmlTable {
            meta: Meta {
                id: ArtifactId::mint(&content, generation),
                content_hash: content,
                provenance: Provenance::Source(SourceAnchor::Pdf {
                    doc,
                    page,
                    bbox: table_bbox,
                }),
                generation,
                risk,
                origin: Origin::default(),
            },
            n_rows,
            n_cols,
            cells,
            html,
        }));
    }

    Ok(out)
}

fn doc_role(label: &str) -> DocRole {
    match label {
        "title" => DocRole::Title,
        "section_header" => DocRole::Heading,
        "text" | "paragraph" => DocRole::Paragraph,
        "caption" => DocRole::Caption,
        "list_item" => DocRole::ListItem,
        _ => DocRole::Other,
    }
}

/// Walk a list of `body`/group/picture child refs in reading order, emitting a
/// `DocElement` per `#/texts/N`, **recursing through `#/groups/N`** (lists,
/// multi-column sections) **and `#/pictures/N`** (figure text recovered by docling's
/// OCR, which lands under the picture, not in body). `in_picture` text is tagged
/// `Other` so it stays distinguishable from prose. Tables are separate artifacts.
fn walk_children(
    children: &[DRef],
    dd: &DoclingDoc,
    doc: DocHash,
    out: &mut Vec<DocElement>,
    seen: &mut std::collections::HashSet<String>,
    in_picture: bool,
) {
    for child in children {
        if let Some(idx) = child.cref.strip_prefix("#/texts/").and_then(|s| s.parse::<usize>().ok()) {
            let Some(t) = dd.texts.get(idx) else { continue };
            if t.text.trim().is_empty() {
                continue;
            }
            let prov = t.prov.first();
            let page = prov.map(|p| p.page_no).unwrap_or(1);
            let page_h = dd.pages.get(&page.to_string()).map(|p| p.size.height).unwrap_or(792.0);
            let bbox = prov.map(|p| p.bbox.to_topleft(page_h)).unwrap_or(BBox::new(0.0, 0.0, 0.0, 0.0));
            out.push(DocElement {
                role: if in_picture { DocRole::Figure } else { doc_role(&t.label) },
                // collapse docling's layout-derived whitespace ("Though  born" -> "Though born")
                text: t.text.split_whitespace().collect::<Vec<_>>().join(" "),
                anchor: SourceAnchor::Pdf { doc, page, bbox },
            });
        } else if let Some(g) = child.cref.strip_prefix("#/groups/").and_then(|s| s.parse::<usize>().ok())
        {
            // recurse into the group (guard against cycles by ref)
            if seen.insert(child.cref.clone()) {
                let Some(grp) = dd.groups.get(g) else { continue };
                walk_children(&grp.children, dd, doc, out, seen, in_picture);
            }
        } else if let Some(p) = child.cref.strip_prefix("#/pictures/").and_then(|s| s.parse::<usize>().ok())
        {
            // recurse into the picture's OCR'd figure text (do_ocr=True), tagged Other
            if seen.insert(child.cref.clone()) {
                let Some(pic) = dd.pictures.get(p) else { continue };
                walk_children(&pic.children, dd, doc, out, seen, true);
            }
        }
    }
}

/// Extract the structured text — sections / paragraphs / captions in reading
/// order — from Docling JSON, using docling's own element labels (no font
/// heuristics) and its `body.children` reading order, recursing through groups.
/// Tables are separate `HtmlTable` artifacts; this is the prose spine.
pub fn structured_doc_from_docling(
    json: &str,
    doc: DocHash,
    generation: Generation,
) -> Result<StructuredDoc> {
    let dd: DoclingDoc = serde_json::from_str(json).context("parsing Docling JSON")?;
    let mut elements = Vec::new();
    let mut seen = std::collections::HashSet::new();
    walk_children(&dd.body.children, &dd, doc, &mut elements, &mut seen, false);
    let joined: String = elements.iter().map(|e| e.text.as_str()).collect();
    let content = DocHash::of(format!("structdoc:{joined}").as_bytes());
    let anchor = elements
        .first()
        .map(|e| e.anchor.clone())
        .unwrap_or(SourceAnchor::Pdf { doc, page: 1, bbox: BBox::new(0.0, 0.0, 0.0, 0.0) });
    Ok(StructuredDoc {
        meta: Meta {
            id: ArtifactId::mint(&content, generation),
            content_hash: content,
            provenance: Provenance::Source(anchor),
            generation,
            risk: RiskMarkers::default(),
            origin: Origin::default(),
        },
        elements,
    })
}

/// Risk markers consistent with the cheap reconstructor's, computed from the
/// placed grid — so `StructuralValidity` behaves identically regardless of which
/// extractor produced the table. A clean parser yields near-zero markers.
fn risk_from(cells: &[Cell], n_rows: u32, n_cols: u32) -> RiskMarkers {
    let mut filled = vec![0u32; n_rows.max(1) as usize];
    for c in cells {
        if (c.row as usize) < filled.len() && !c.text.trim().is_empty() {
            filled[c.row as usize] += 1;
        }
    }
    let mean = filled.iter().map(|&x| x as f32).sum::<f32>() / filled.len() as f32;
    let var = filled.iter().map(|&x| (x as f32 - mean).powi(2)).sum::<f32>() / filled.len() as f32;
    let merged = filled.iter().filter(|&&f| f < n_cols).count() as u32;
    let empty = filled.iter().map(|&f| n_cols.saturating_sub(f)).sum::<u32>();
    RiskMarkers {
        min_ocr_confidence: 1.0,
        column_count_variance: if mean > 0.0 { var / mean } else { 0.0 },
        merged_cell_rows: merged,
        empty_cells: empty,
        rotated_text: false,
        figure_score: 0.0, // Docling emits structured tables, not figures
        notes: vec![],
    }
}

fn render_html(cells: &[Cell], n_rows: u32, n_cols: u32) -> String {
    let mut grid = vec![vec![String::new(); n_cols as usize]; n_rows as usize];
    let mut header = vec![false; n_rows as usize];
    for c in cells {
        if (c.row as usize) < grid.len() && (c.col as usize) < n_cols as usize {
            grid[c.row as usize][c.col as usize] = c.text.clone();
            if c.is_header {
                header[c.row as usize] = true;
            }
        }
    }
    let mut s = String::from("<table>\n");
    for (r, row) in grid.iter().enumerate() {
        let tag = if header.get(r).copied().unwrap_or(false) { "th" } else { "td" };
        s.push_str("  <tr>");
        for cell in row {
            let esc = cell.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            s.push_str(&format!("<{tag}>{esc}</{tag}>"));
        }
        s.push_str("</tr>\n");
    }
    s.push_str("</table>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{CheckCtx, IntrinsicArithmetic, QualityCheck, StructuralValidity};
    use crate::doc::{DocFormat, QDoc};

    const SAMPLE: &str = include_str!("../tests/data/sample.docling.json");
    const FULL: &str = include_str!("../tests/data/sample.docling_full.json");

    #[test]
    fn regions_from_layout_boxes() {
        let json = r#"{
          "pages": {"1": {"size": {"width": 600, "height": 800}}},
          "texts": [{"label":"section_header","text":"H","prov":[{"page_no":1,"bbox":{"l":10,"t":20,"r":100,"b":40,"coord_origin":"TOPLEFT"}}]}],
          "tables": [{"prov":[{"page_no":1,"bbox":{"l":50,"t":300,"r":400,"b":500,"coord_origin":"TOPLEFT"}}],"data":{"num_rows":0,"num_cols":0,"table_cells":[]}}],
          "pictures": [{"prov":[{"page_no":1,"bbox":{"l":0,"t":600,"r":200,"b":700,"coord_origin":"TOPLEFT"}}]}]
        }"#;
        let regs = regions_from_docling(json, DocHash::of(b"d"), Generation(0)).unwrap();
        assert_eq!(regs.len(), 3, "one region per text/table/picture");
        let roles: Vec<RegionRole> = regs.iter().map(|r| r.role()).collect();
        assert!(roles.contains(&RegionRole::Text));
        assert!(roles.contains(&RegionRole::Table));
        assert!(roles.contains(&RegionRole::Figure));
        let table = regs.iter().find(|r| r.role() == RegionRole::Table).unwrap();
        assert_eq!(table.bbox(), BBox::new(50.0, 300.0, 400.0, 500.0), "bbox preserved");
    }

    #[test]
    fn recurses_into_groups() {
        // a heading followed by a list group containing two items
        let json = r##"{
          "texts": [
            {"label":"section_header","text":"H","prov":[{"page_no":1,"bbox":{"l":0,"t":10,"r":10,"b":0}}]},
            {"label":"list_item","text":"item A","prov":[{"page_no":1,"bbox":{"l":0,"t":20,"r":10,"b":10}}]},
            {"label":"list_item","text":"item B","prov":[{"page_no":1,"bbox":{"l":0,"t":30,"r":10,"b":20}}]}
          ],
          "groups": [ {"children":[{"cref":"#/texts/1"},{"cref":"#/texts/2"}]} ],
          "body": {"children":[{"cref":"#/texts/0"},{"cref":"#/groups/0"}]},
          "pages": {"1":{"size":{"width":100,"height":100}}}
        }"##;
        let sd = structured_doc_from_docling(json, DocHash::of(b"d"), Generation(0)).unwrap();
        let texts: Vec<&str> = sd.elements.iter().map(|e| e.text.as_str()).collect();
        // without group recursion, "item A"/"item B" would be missing
        assert_eq!(texts, vec!["H", "item A", "item B"]);
    }

    #[test]
    fn extracts_structured_text_and_sections() {
        let sd = structured_doc_from_docling(FULL, DocHash::of(b"pdf"), Generation(0)).unwrap();
        assert!(!sd.elements.is_empty(), "extracted text elements");
        // docling labelled three section headers in this fixture
        let headings: Vec<&str> = sd
            .elements
            .iter()
            .filter(|e| e.role == DocRole::Heading)
            .map(|e| e.text.as_str())
            .collect();
        assert!(headings.len() >= 3, "headings = {headings:?}");
        assert!(headings.iter().any(|h| h.contains("Statement of Operations")));
        // elements are anchored to the source page
        assert!(matches!(sd.elements[0].anchor, SourceAnchor::Pdf { page: 1, .. }));
        // sections() groups body under headings
        assert!(sd.sections().len() >= 3, "grouped into sections");
    }

    #[test]
    fn adapts_docling_tables_into_artifacts() {
        let arts = artifacts_from_docling(SAMPLE, DocHash::of(b"pdf"), Generation(0)).unwrap();
        assert_eq!(arts.len(), 2, "two tables in the sample");

        let t0 = arts[0].as_any().downcast_ref::<HtmlTable>().unwrap();
        assert_eq!((t0.n_rows, t0.n_cols), (4, 3));
        // Header cell carries a resolved top-left anchor on the correct page.
        let c = t0.cell(0, 0).unwrap();
        assert!(c.is_header);
        match &c.anchor {
            SourceAnchor::Pdf { page, bbox, .. } => {
                assert_eq!(*page, 1);
                // BOTTOMLEFT t=700 on a 792-high page -> top-left y0 = 92.
                assert!((bbox.y0 - 92.0).abs() < 0.5, "got y0={}", bbox.y0);
            }
            _ => panic!("expected PDF anchor"),
        }
    }

    #[test]
    fn detectors_run_on_docling_artifacts() {
        let arts = artifacts_from_docling(SAMPLE, DocHash::of(b"pdf"), Generation(0)).unwrap();
        let dummy = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let ctx = CheckCtx { source: &dummy };
        let arith = IntrinsicArithmetic::default();
        let structural = StructuralValidity;

        // Table 0 (income) reconciles; table 1 (balance) has a bad total (9,999).
        assert!(!arith.check(arts[0].as_ref(), &ctx).is_flag(), "income reconciles");
        assert!(arith.check(arts[1].as_ref(), &ctx).is_flag(), "balance total is wrong");
        // Docling grids are clean, so structural passes both (no false alarm).
        assert!(!structural.check(arts[0].as_ref(), &ctx).is_flag());
        assert!(!structural.check(arts[1].as_ref(), &ctx).is_flag());
    }
}
