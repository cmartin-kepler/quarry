//! The eval harness (brief §2, §5) — the deliverable that matters first. It
//! measures the **silent-failure catch rate**: of the extractions that are
//! actually wrong (vs hand-labeled truth), what fraction did ≥1 detector flag?
//! Broken down by detector so we learn which one carries the weight.

use crate::artifact::*;
use crate::check::*;
use crate::core::*;
use crate::doc::QDoc;
use crate::pipeline;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---- Ground-truth format (brief §5) ---------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TruthTable {
    /// Human label for reporting.
    pub name: String,
    /// The page + bbox the table should map to (for matching to an extraction).
    pub page: u32,
    pub bbox: [f32; 4],
    /// The correct extraction, row-major. Header row included.
    pub cells: Vec<Vec<String>>,
    /// Optional per-column types (e.g. "text", "currency", "percent").
    #[serde(default)]
    pub column_types: Vec<String>,
    /// Optional difficulty tag (merged-cells / hierarchical-header / …).
    #[serde(default)]
    pub difficulty: Option<String>,
}

impl TruthTable {
    fn bbox(&self) -> BBox {
        BBox::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroundTruth {
    pub tables: Vec<TruthTable>,
}

impl GroundTruth {
    pub fn load(path: &Path) -> Result<GroundTruth> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
    }
}

// ---- Per-table eval result ------------------------------------------------

/// What one detector saw on one table — flagged or not, plus the evidence
/// (the "how we know"): an arithmetic mismatch, the parse-time risk markers, or
/// the cited crop that didn't support a cell.
#[derive(Clone, Debug)]
pub struct DetectorOutcome {
    pub detector: String,
    pub flagged: bool,
    pub severity: Option<Severity>,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct TableEval {
    pub name: String,
    pub difficulty: Option<String>,
    /// Did any reconstructed table map to this truth region (by anchor IoU)?
    pub matched: bool,
    pub matched_id: Option<String>,
    pub iou: f32,
    /// The reconstructed grid (what the cheap parser produced).
    pub got_grid: Vec<Vec<String>>,
    /// The hand-labeled correct grid.
    pub want_grid: Vec<Vec<String>>,
    /// Parse-time risk markers of the matched extraction.
    pub risk: Option<RiskMarkers>,
    pub wrong: bool,
    /// Every cell-level divergence from truth.
    pub cell_diffs: Vec<String>,
    pub detectors: Vec<DetectorOutcome>,
}

impl TableEval {
    pub fn flagged(&self) -> bool {
        self.detectors.iter().any(|d| d.flagged)
    }
    pub fn flagged_by(&self) -> Vec<String> {
        self.detectors
            .iter()
            .filter(|d| d.flagged)
            .map(|d| d.detector.clone())
            .collect()
    }
    pub fn got_dims(&self) -> (usize, usize) {
        dims(&self.got_grid)
    }
    pub fn want_dims(&self) -> (usize, usize) {
        dims(&self.want_grid)
    }
    /// A wrong extraction that NO detector flagged — the dangerous case.
    pub fn missed(&self) -> bool {
        self.wrong && !self.flagged()
    }
}

fn dims(grid: &[Vec<String>]) -> (usize, usize) {
    (grid.len(), grid.iter().map(|r| r.len()).max().unwrap_or(0))
}

#[derive(Clone, Debug, Default)]
pub struct CatchReport {
    pub tables: Vec<TableEval>,
    /// Total tables the cheap parser reconstructed across the document.
    pub total_extracted: usize,
}

impl CatchReport {
    pub fn n_wrong(&self) -> usize {
        self.tables.iter().filter(|t| t.wrong).count()
    }

    /// Of the wrong extractions, the fraction flagged by ≥1 detector.
    pub fn catch_rate(&self) -> Option<f32> {
        let wrong = self.n_wrong();
        if wrong == 0 {
            return None;
        }
        let caught = self.tables.iter().filter(|t| t.wrong && t.flagged()).count();
        Some(caught as f32 / wrong as f32)
    }

    /// Flags raised on extractions that were actually correct (false alarms).
    pub fn false_positive_rate(&self) -> Option<f32> {
        let correct = self.tables.iter().filter(|t| t.matched && !t.wrong).count();
        if correct == 0 {
            return None;
        }
        let noisy = self
            .tables
            .iter()
            .filter(|t| t.matched && !t.wrong && t.flagged())
            .count();
        Some(noisy as f32 / correct as f32)
    }

    /// Wrong extractions that slipped past every detector.
    pub fn missed(&self) -> Vec<&TableEval> {
        self.tables.iter().filter(|t| t.missed()).collect()
    }

    /// Per-detector: how many of the wrong tables each detector caught.
    pub fn per_detector_catches(&self) -> Vec<(String, usize)> {
        ["intrinsic_arithmetic", "structural_validity", "answer_support"]
            .iter()
            .map(|n| {
                let c = self
                    .tables
                    .iter()
                    .filter(|t| t.wrong && t.flagged_by().iter().any(|f| f == n))
                    .count();
                (n.to_string(), c)
            })
            .collect()
    }
}

/// Run the cheap parse + all detectors against ground truth.
pub fn run_eval(doc: &QDoc, doc_hash: DocHash, truth: &GroundTruth, tier: u8) -> Result<CatchReport> {
    let artifacts = pipeline::cheap_parse(doc, doc_hash, tier)?;
    let tables: Vec<&HtmlTable> = artifacts
        .iter()
        .filter_map(|a| a.as_any().downcast_ref::<HtmlTable>())
        .collect();

    let arithmetic = IntrinsicArithmetic::default();
    let structural = StructuralValidity;
    let verifier = SourceCropVerifier;
    let ctx = CheckCtx { source: doc };

    let mut report = CatchReport {
        total_extracted: tables.len(),
        ..Default::default()
    };

    for tt in &truth.tables {
        // Match by anchor IoU on the same page.
        let want = tt.bbox();
        let best = tables
            .iter()
            .filter(|t| match t.anchor() {
                SourceAnchor::Pdf { page, .. } => *page == tt.page,
                _ => false,
            })
            .map(|t| {
                let b = match t.anchor() {
                    SourceAnchor::Pdf { bbox, .. } => *bbox,
                    _ => unreachable!(),
                };
                (t, want.iou(&b))
            })
            .filter(|(_, iou)| *iou > 0.3)
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap());

        let Some((table, iou)) = best else {
            // No extraction mapped here at all — a wrong "extraction" nobody can
            // flag with cell checks. Count it wrong & unflagged (worst case).
            report.tables.push(TableEval {
                name: tt.name.clone(),
                difficulty: tt.difficulty.clone(),
                matched: false,
                matched_id: None,
                iou: 0.0,
                got_grid: vec![],
                want_grid: tt.cells.clone(),
                risk: None,
                wrong: true,
                cell_diffs: vec!["no reconstructed table mapped to this region".into()],
                detectors: vec![],
            });
            continue;
        };

        let got = table.grid();
        let (wrong, cell_diffs) = diff_grid(&got, &tt.cells);

        // Run the three detectors, keeping their full outcome (the evidence).
        let detectors = vec![
            to_outcome("intrinsic_arithmetic", &arithmetic.check(*table, &ctx)),
            to_outcome("structural_validity", &structural.check(*table, &ctx)),
            answer_support_outcome(table, &verifier, doc),
        ];

        report.tables.push(TableEval {
            name: tt.name.clone(),
            difficulty: tt.difficulty.clone(),
            matched: true,
            matched_id: Some(table.id().to_string()),
            iou,
            got_grid: got,
            want_grid: tt.cells.clone(),
            risk: Some(table.risk().clone()),
            wrong,
            cell_diffs,
            detectors,
        });
    }

    Ok(report)
}

fn to_outcome(name: &str, o: &CheckOutcome) -> DetectorOutcome {
    match o {
        CheckOutcome::Pass { confidence } => DetectorOutcome {
            detector: name.to_string(),
            flagged: false,
            severity: None,
            detail: format!("pass (confidence {confidence:.2})"),
        },
        CheckOutcome::Flag { reason, severity } => DetectorOutcome {
            detector: name.to_string(),
            flagged: true,
            severity: Some(*severity),
            detail: reason.clone(),
        },
    }
}

/// Sampled AnswerSupport: for each non-header cell, treat the extracted value as
/// the agent's claim and verify it against the source crop at the cell's cited
/// anchor. A misassigned cell's text won't be inside its band rectangle → flag.
/// Reports the first unsupported cell (with the cited crop) as the evidence.
fn answer_support_outcome(
    table: &HtmlTable,
    verifier: &SourceCropVerifier,
    source: &QDoc,
) -> DetectorOutcome {
    let mut sampled = 0;
    let mut first_fail = None;
    for c in &table.cells {
        if c.is_header || c.text.trim().is_empty() {
            continue;
        }
        sampled += 1;
        if first_fail.is_some() {
            continue; // keep counting `sampled`, but only report the first failure
        }
        let claim = Claim {
            element: table.id(),
            anchor: c.anchor.clone(),
            asserted: c.text.clone(),
        };
        if let SupportOutcome::Unsupported { reason } = verifier.verify(&claim, source) {
            first_fail = Some(format!("cell [{},{}] {:?} — {}", c.row, c.col, c.text, reason));
        }
    }
    match first_fail {
        Some(detail) => DetectorOutcome {
            detector: "answer_support".to_string(),
            flagged: true,
            severity: Some(Severity::Error),
            detail,
        },
        None => DetectorOutcome {
            detector: "answer_support".to_string(),
            flagged: false,
            severity: None,
            detail: format!("{sampled} sampled cell(s) all present in their cited crops"),
        },
    }
}

/// Structural diff. Returns (is_wrong, all cell-level diffs). Normalizes
/// whitespace and financial punctuation so formatting isn't counted as error.
fn diff_grid(got: &[Vec<String>], want: &[Vec<String>]) -> (bool, Vec<String>) {
    let mut diffs = Vec::new();
    let (gr, gc) = dims(got);
    let (wr, wc) = dims(want);
    if (gr, gc) != (wr, wc) {
        diffs.push(format!("dimensions: got {gr}x{gc}, want {wr}x{wc}"));
    }
    let rows = gr.max(wr);
    for r in 0..rows {
        let g_row = got.get(r);
        let w_row = want.get(r);
        let cols = g_row.map_or(0, |x| x.len()).max(w_row.map_or(0, |x| x.len()));
        for c in 0..cols {
            let g = g_row.and_then(|x| x.get(c)).map(String::as_str).unwrap_or("");
            let w = w_row.and_then(|x| x.get(c)).map(String::as_str).unwrap_or("");
            if norm(g) != norm(w) {
                diffs.push(format!("[{r},{c}] got {g:?} want {w:?}"));
            }
        }
    }
    (!diffs.is_empty(), diffs)
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['$', ','], "")
        .to_lowercase()
}
