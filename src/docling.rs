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
