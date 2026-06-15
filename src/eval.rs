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

#[derive(Clone, Debug)]
pub struct TableEval {
    pub name: String,
    pub matched: bool,
    pub wrong: bool,
    /// Detector → did it flag this table.
    pub flagged_by: Vec<String>,
    pub diff_summary: String,
}

impl TableEval {
    pub fn flagged(&self) -> bool {
        !self.flagged_by.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct CatchReport {
    pub tables: Vec<TableEval>,
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

    /// Per-detector: how many of the wrong tables each detector alone caught.
    pub fn per_detector_catches(&self) -> Vec<(String, usize)> {
        let mut names = vec![
            "intrinsic_arithmetic".to_string(),
            "structural_validity".to_string(),
            "answer_support".to_string(),
        ];
        let mut out = Vec::new();
        for n in names.drain(..) {
            let c = self
                .tables
                .iter()
                .filter(|t| t.wrong && t.flagged_by.contains(&n))
                .count();
            out.push((n, c));
        }
        out
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

    let mut report = CatchReport::default();

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

        let Some((table, _iou)) = best else {
            // No extraction mapped here at all — a wrong "extraction" nobody can
            // flag with cell checks. Count it wrong & unflagged (worst case).
            report.tables.push(TableEval {
                name: tt.name.clone(),
                matched: false,
                wrong: true,
                flagged_by: vec![],
                diff_summary: "no extracted table matched this region".into(),
            });
            continue;
        };

        let (wrong, diff) = diff_grid(&table.grid(), &tt.cells);

        // Run the three detectors.
        let mut flagged_by = Vec::new();
        if arithmetic.check(*table, &ctx).is_flag() {
            flagged_by.push("intrinsic_arithmetic".to_string());
        }
        if structural.check(*table, &ctx).is_flag() {
            flagged_by.push("structural_validity".to_string());
        }
        if answer_support_flags(table, &verifier, doc) {
            flagged_by.push("answer_support".to_string());
        }

        report.tables.push(TableEval {
            name: tt.name.clone(),
            matched: true,
            wrong,
            flagged_by,
            diff_summary: diff,
        });
    }

    Ok(report)
}

/// Sampled AnswerSupport: for each non-header cell, treat the extracted value as
/// the agent's claim and verify it against the source crop at the cell's cited
/// anchor. A misassigned cell's text won't be inside its band rectangle → flag.
fn answer_support_flags(table: &HtmlTable, verifier: &SourceCropVerifier, source: &QDoc) -> bool {
    for c in &table.cells {
        if c.is_header || c.text.trim().is_empty() {
            continue;
        }
        let claim = Claim {
            element: table.id(),
            anchor: c.anchor.clone(),
            asserted: c.text.clone(),
        };
        if let SupportOutcome::Unsupported { .. } = verifier.verify(&claim, source) {
            return true;
        }
    }
    false
}

/// Structural diff. Returns (is_wrong, human_summary). Normalizes whitespace and
/// financial punctuation so formatting differences aren't counted as errors.
fn diff_grid(got: &[Vec<String>], want: &[Vec<String>]) -> (bool, String) {
    if got.len() != want.len() {
        return (
            true,
            format!("row count differs: got {}, want {}", got.len(), want.len()),
        );
    }
    let mut diffs = Vec::new();
    for (r, (gr, wr)) in got.iter().zip(want.iter()).enumerate() {
        if gr.len() != wr.len() {
            diffs.push(format!("row {r}: col count got {} want {}", gr.len(), wr.len()));
            continue;
        }
        for (c, (g, w)) in gr.iter().zip(wr.iter()).enumerate() {
            if norm(g) != norm(w) {
                diffs.push(format!("[{r},{c}] got {g:?} want {w:?}"));
            }
        }
    }
    if diffs.is_empty() {
        (false, "exact match".into())
    } else {
        let n = diffs.len();
        diffs.truncate(4);
        let mut s = diffs.join("; ");
        if n > 4 {
            s.push_str(&format!("; (+{} more)", n - 4));
        }
        (true, s)
    }
}

fn norm(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['$', ','], "")
        .to_lowercase()
}
