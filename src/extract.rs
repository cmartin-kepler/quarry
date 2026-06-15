//! The `Extractor` trait + the Phase-0 PDF text-layer reconstructor.
//!
//! IMPORTANT: this does NOT parse PDF bytes. Turning a `.pdf` into positioned
//! text spans (the actual "PDF parsing") happens upstream in the pdfplumber
//! bridge (`scripts/pdf_to_qdoc.py`), which emits the `.qdoc` text layer this
//! consumes. `PdfTextLayerReconstructor` takes that already-extracted text layer
//! and *reconstructs tables* from it using naive x/y clustering.
//!
//! Cost tiers are FORMAT-SPECIFIC, not a global ladder (brief §4). The naivety
//! of the reconstruction is the point: right-aligned numeric columns, multi-level
//! headers, and merged cells make it produce a clean-looking grid that is wrong —
//! the silent failures detector quality is measured against.

use crate::artifact::*;
use crate::core::*;
use crate::doc::{Page, QDoc, TableRegion};
use anyhow::{Result, bail};

/// What an extractor can consume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// A raw region of the original document.
    DocumentRegion,
    /// A prior artifact of a given kind.
    Artifact(ArtifactKind),
}

/// Format-specific cost tier. Tier numbers are only comparable within one
/// format (brief §4 note).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CostTier(pub u8);

pub enum ExtractInput<'a> {
    DocumentRegion { doc: DocHash, anchor: SourceAnchor },
    Artifacts(&'a [&'a dyn Artifact]),
}

/// Context handed to an extractor: the loaded source document and the running
/// per-document generation counter.
pub struct ExtractCtx<'a> {
    pub source: &'a QDoc,
    pub generation: Generation,
}

pub trait Extractor: Send + Sync {
    fn id(&self) -> ExtractorId;
    fn version(&self) -> Version;
    fn cost_tier(&self) -> CostTier;
    fn accepts(&self) -> &[InputKind];
    fn produces(&self) -> ArtifactKind;
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>)
    -> Result<Vec<Box<dyn Artifact>>>;
}

// ---------------------------------------------------------------------------
// PDF text-layer reconstructor (tier 0 for the PDF format).
// Consumes a `.qdoc` text layer (produced by the pdfplumber bridge); it does not
// read PDF bytes itself. "Pdf" names the format whose text layer it handles.
// ---------------------------------------------------------------------------

pub struct PdfTextLayerReconstructor;

const ACCEPTS: [InputKind; 1] = [InputKind::DocumentRegion];

impl Extractor for PdfTextLayerReconstructor {
    fn id(&self) -> ExtractorId {
        ExtractorId("pdf_textlayer".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn accepts(&self) -> &[InputKind] {
        &ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        // Produces multiple kinds; `produces` reports the headline kind.
        ArtifactKind::HtmlTable
    }

    fn extract(
        &self,
        input: ExtractInput<'_>,
        ctx: &ExtractCtx<'_>,
    ) -> Result<Vec<Box<dyn Artifact>>> {
        let (doc, page_no) = match input {
            ExtractInput::DocumentRegion { doc, anchor } => match anchor {
                SourceAnchor::Pdf { page, .. } => (doc, page),
                _ => bail!("pdf_textlayer only handles PDF anchors"),
            },
            ExtractInput::Artifacts(_) => {
                bail!("pdf_textlayer consumes raw regions, not artifacts")
            }
        };

        let page = ctx
            .source
            .page(page_no)
            .ok_or_else(|| anyhow::anyhow!("page {page_no} not in document"))?;

        let mut out: Vec<Box<dyn Artifact>> = Vec::new();

        // 1) Reading-order text for the whole page.
        out.push(Box::new(extract_text(doc, page, ctx.generation)));

        // 2) One HtmlTable per marked region.
        for region in &page.table_regions {
            out.push(Box::new(reconstruct_table(
                doc,
                page,
                region,
                ctx.generation,
            )));
        }

        Ok(out)
    }
}

fn extract_text(doc: DocHash, page: &Page, generation: Generation) -> ExtractedText {
    // Reading order: top-to-bottom, then left-to-right.
    let mut idx: Vec<usize> = (0..page.spans.len()).collect();
    idx.sort_by(|&a, &b| {
        let (sa, sb) = (&page.spans[a], &page.spans[b]);
        sa.bbox()
            .y0
            .partial_cmp(&sb.bbox().y0)
            .unwrap()
            .then(sa.bbox().x0.partial_cmp(&sb.bbox().x0).unwrap())
    });

    let mut spans = Vec::with_capacity(idx.len());
    let mut full = BBox::new(f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut min_conf = 1.0f32;
    let mut rotated = false;
    for (order, &i) in idx.iter().enumerate() {
        let s = &page.spans[i];
        full = full.union(&s.bbox());
        min_conf = min_conf.min(s.confidence);
        rotated |= s.rotated;
        spans.push(TextSpan {
            text: s.text.clone(),
            bbox: s.bbox(),
            order: order as u32,
        });
    }
    if spans.is_empty() {
        full = BBox::new(0.0, 0.0, page.width, page.height);
    }

    let content = DocHash::of(
        spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\u{1}")
            .as_bytes(),
    );
    let anchor = SourceAnchor::Pdf {
        doc,
        page: page.page,
        bbox: full,
    };
    let risk = RiskMarkers {
        min_ocr_confidence: min_conf,
        rotated_text: rotated,
        ..Default::default()
    };
    ExtractedText {
        meta: Meta {
            id: ArtifactId::mint(&content, generation),
            content_hash: content,
            provenance: Provenance::Source(anchor),
            generation,
            risk,
        },
        spans,
    }
}

/// The naive grid reconstruction. Rows: cluster spans by y-center. Columns: one
/// global column model from clustering x0 across the region. Right-aligned
/// numbers and multi-level headers defeat a global x0 model — producing the
/// realistic, clean-looking-but-wrong tables.
fn reconstruct_table(
    doc: DocHash,
    page: &Page,
    region: &TableRegion,
    generation: Generation,
) -> HtmlTable {
    let rb = region.bbox();
    let mut spans: Vec<&crate::doc::Span> =
        page.spans.iter().filter(|s| rb.contains_center(&s.bbox())).collect();
    spans.sort_by(|a, b| a.bbox().y0.partial_cmp(&b.bbox().y0).unwrap());

    // --- Rows: greedy y clustering on a font-height-ish threshold.
    let row_tol = median_height(&spans).max(4.0) * 0.7;
    let mut rows: Vec<Vec<&crate::doc::Span>> = Vec::new();
    for s in &spans {
        let cy = s.bbox().center().1;
        match rows.last_mut() {
            Some(r) if (row_center(r) - cy).abs() <= row_tol => r.push(s),
            _ => rows.push(vec![s]),
        }
    }

    // --- Cell blocks: group words within a row into cells by horizontal gap.
    // Real PDF text extraction yields WORDS, not cells ("Total revenue" is two
    // spans), so a per-word column model shatters multi-word cells. Merge
    // adjacent words whose gap is below CELL_GAP into one block. CELL_GAP is the
    // new naive knob: too small splits a wide-spaced cell, too large swallows a
    // thin column — both realistic failure sources on hard tables.
    const CELL_GAP: f32 = 12.0;
    let row_blocks: Vec<Vec<Block>> = rows
        .iter()
        .map(|row| {
            let mut sorted: Vec<&crate::doc::Span> = row.clone();
            sorted.sort_by(|a, b| a.bbox().x0.partial_cmp(&b.bbox().x0).unwrap());
            let mut blocks: Vec<Block> = Vec::new();
            for s in sorted {
                let b = s.bbox();
                match blocks.last_mut() {
                    Some(prev) if b.x0 - prev.x1 <= CELL_GAP => {
                        prev.x1 = prev.x1.max(b.x1);
                        prev.text.push(' ');
                        prev.text.push_str(&s.text);
                    }
                    _ => blocks.push(Block {
                        text: s.text.clone(),
                        x0: b.x0,
                        x1: b.x1,
                    }),
                }
            }
            blocks
        })
        .collect();

    // --- Columns: cluster block x0 across all rows (still a naive global model;
    // right-aligned numbers and multi-level headers defeat it as before).
    let col_tol = 14.0_f32;
    let mut x0s: Vec<f32> = row_blocks.iter().flatten().map(|b| b.x0).collect();
    x0s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut col_anchors: Vec<f32> = Vec::new();
    for x in x0s {
        match col_anchors.last() {
            Some(&last) if (x - last).abs() <= col_tol => {}
            _ => col_anchors.push(x),
        }
    }
    let n_cols = col_anchors.len().max(1) as u32;

    let assign_col = |x: f32| -> u32 {
        col_anchors
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (x - **a).abs().partial_cmp(&(x - **b).abs()).unwrap())
            .map(|(i, _)| i as u32)
            .unwrap_or(0)
    };

    // Column band edges: midpoints between adjacent anchors, clamped to region.
    // A cell's citable anchor is its band rectangle, NOT the source span's own
    // box — so when the global x0 model misassigns a (correct) span to the wrong
    // column, the span sits OUTSIDE its cell's cited crop and AnswerSupport can
    // catch it. This is what gives the vision/crop detector teeth on alignment.
    let col_edges: Vec<(f32, f32)> = (0..n_cols as usize)
        .map(|i| {
            let lo = if i == 0 {
                rb.x0
            } else {
                (col_anchors[i - 1] + col_anchors[i]) / 2.0
            };
            let hi = if i + 1 >= col_anchors.len() {
                rb.x1
            } else {
                (col_anchors[i] + col_anchors[i + 1]) / 2.0
            };
            (lo, hi)
        })
        .collect();

    // Row band y-extents from each cluster's spans.
    let row_bands: Vec<(f32, f32)> = rows
        .iter()
        .map(|row| {
            let y0 = row.iter().map(|s| s.bbox().y0).fold(f32::MAX, f32::min);
            let y1 = row.iter().map(|s| s.bbox().y1).fold(f32::MIN, f32::max);
            (y0, y1)
        })
        .collect();

    // --- Build cells from blocks.
    let mut cells: Vec<Cell> = Vec::new();
    let mut cells_per_row: Vec<u32> = Vec::new();
    for (r, blocks) in row_blocks.iter().enumerate() {
        let mut seen_cols = std::collections::HashSet::new();
        let (ry0, ry1) = row_bands[r];
        for blk in blocks {
            let col = assign_col(blk.x0);
            // Two blocks landing in one column concatenate — a real merge bug source.
            if let Some(existing) = cells
                .iter_mut()
                .find(|c| c.row == r as u32 && c.col == col)
            {
                existing.text.push(' ');
                existing.text.push_str(&blk.text);
            } else {
                seen_cols.insert(col);
                let (cx0, cx1) = col_edges[col as usize];
                cells.push(Cell {
                    row: r as u32,
                    col,
                    text: blk.text.clone(),
                    anchor: SourceAnchor::Pdf {
                        doc,
                        page: page.page,
                        bbox: BBox::new(cx0, ry0, cx1, ry1),
                    },
                    is_header: r == 0,
                });
            }
        }
        cells_per_row.push(seen_cols.len() as u32);
    }
    let n_rows = rows.len() as u32;

    // --- Risk markers from the reconstruction.
    let risk = grid_risk(&spans, &cells_per_row, n_cols, n_rows);
    let html = render_html(&cells, n_rows, n_cols);
    let content = DocHash::of(html.as_bytes());
    let anchor = SourceAnchor::Pdf {
        doc,
        page: page.page,
        bbox: rb,
    };

    HtmlTable {
        meta: Meta {
            id: ArtifactId::mint(&content, generation),
            content_hash: content,
            provenance: Provenance::Source(anchor),
            generation,
            risk,
        },
        n_rows,
        n_cols,
        cells,
        html,
    }
}

/// A run of words merged into one cell candidate by the gap heuristic.
struct Block {
    text: String,
    x0: f32,
    x1: f32,
}

fn grid_risk(
    spans: &[&crate::doc::Span],
    cells_per_row: &[u32],
    n_cols: u32,
    _n_rows: u32,
) -> RiskMarkers {
    let mut min_conf = 1.0f32;
    let mut rotated = false;
    for s in spans {
        min_conf = min_conf.min(s.confidence);
        rotated |= s.rotated;
    }

    // Column-count variance, normalized by the modal column count.
    let mean = if cells_per_row.is_empty() {
        0.0
    } else {
        cells_per_row.iter().map(|&c| c as f32).sum::<f32>() / cells_per_row.len() as f32
    };
    let var = if cells_per_row.is_empty() {
        0.0
    } else {
        cells_per_row
            .iter()
            .map(|&c| (c as f32 - mean).powi(2))
            .sum::<f32>()
            / cells_per_row.len() as f32
    };
    let norm_var = if mean > 0.0 { var / mean } else { 0.0 };

    let merged = cells_per_row.iter().filter(|&&c| c < n_cols).count() as u32;
    let empty = cells_per_row
        .iter()
        .map(|&c| n_cols.saturating_sub(c))
        .sum::<u32>();

    let mut notes = Vec::new();
    if norm_var > 0.5 {
        notes.push(format!("ragged rows (normalized col-count variance {norm_var:.2})"));
    }
    if merged > 0 {
        notes.push(format!("{merged} row(s) below modal column count"));
    }

    RiskMarkers {
        min_ocr_confidence: min_conf,
        column_count_variance: norm_var,
        merged_cell_rows: merged,
        empty_cells: empty,
        rotated_text: rotated,
        notes,
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
        s.push_str("  <tr>");
        let tag = if header.get(r).copied().unwrap_or(false) {
            "th"
        } else {
            "td"
        };
        for cell in row {
            s.push_str(&format!("<{tag}>{}</{tag}>", html_escape(cell)));
        }
        s.push_str("</tr>\n");
    }
    s.push_str("</table>\n");
    s
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn row_center(row: &[&crate::doc::Span]) -> f32 {
    row.iter().map(|s| s.bbox().center().1).sum::<f32>() / row.len() as f32
}

fn median_height(spans: &[&crate::doc::Span]) -> f32 {
    if spans.is_empty() {
        return 0.0;
    }
    let mut hs: Vec<f32> = spans.iter().map(|s| s.bbox().height()).collect();
    hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    hs[hs.len() / 2]
}
