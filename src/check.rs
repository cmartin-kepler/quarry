//! Quality checks run BEFORE an agent sees an artifact (brief §2, §4) — the
//! subsystem whose catch rate the eval harness exists to measure.
//!
//! `AnswerSupport` is deliberately NOT a `QualityCheck`: it is claim-time and
//! takes a claim + artifact, not just an artifact (a tension the brief flags in
//! §4). It gets its own trait.

use crate::analysis::{TableModel, reconcile, type_violations};
use crate::artifact::*;
use crate::core::*;
use crate::doc::QDoc;

#[derive(Clone, Debug)]
pub enum CheckOutcome {
    Pass { confidence: f32 },
    Flag { reason: String, severity: Severity },
}

impl CheckOutcome {
    pub fn is_flag(&self) -> bool {
        matches!(self, CheckOutcome::Flag { .. })
    }
}

/// Context available to a parse-time check. The source doc is here so checks may
/// re-crop if they wish (the cheap ones don't need it).
pub struct CheckCtx<'a> {
    pub source: &'a QDoc,
}

pub trait QualityCheck: Send + Sync {
    fn id(&self) -> CheckId;
    fn applies_to(&self, kind: ArtifactKind) -> bool;
    fn check(&self, artifact: &dyn Artifact, ctx: &CheckCtx<'_>) -> CheckOutcome;
}

// ---------------------------------------------------------------------------
// 1. IntrinsicArithmetic — the high-value, ~free detector (brief §2.1).
// ---------------------------------------------------------------------------

/// Checks the table's own arithmetic: does a labeled "total" row equal the sum
/// of the numeric rows above it, per column? Transposed columns and shifted rows
/// break this even when the grid looks clean.
pub struct IntrinsicArithmetic {
    pub rel_tolerance: f64,
}

impl Default for IntrinsicArithmetic {
    fn default() -> Self {
        IntrinsicArithmetic { rel_tolerance: 0.01 }
    }
}

impl QualityCheck for IntrinsicArithmetic {
    fn id(&self) -> CheckId {
        CheckId("intrinsic_arithmetic".into())
    }
    fn applies_to(&self, kind: ArtifactKind) -> bool {
        matches!(kind, ArtifactKind::HtmlTable | ArtifactKind::DbTable)
    }
    fn check(&self, artifact: &dyn Artifact, _ctx: &CheckCtx<'_>) -> CheckOutcome {
        let Some(t) = artifact.as_any().downcast_ref::<HtmlTable>() else {
            return CheckOutcome::Pass { confidence: 0.5 };
        };
        // Section-aware reconciliation: skip all header rows (multi-level),
        // ignore ratio/percentage columns, and sum the data rows since the
        // previous total — so subtotals in multi-section tables reconcile
        // instead of misfiring. abs tolerance absorbs cent-rounding in sums.
        let model = TableModel::from_table(t);
        let recon = reconcile(&model, self.rel_tolerance, 0.06);
        if recon.is_empty() {
            return CheckOutcome::Pass { confidence: 0.6 }; // no total rows to check
        }

        // Only a BROAD failure (a total row where no column reconciles) signals a
        // mis-parse. If some columns reconcile, the alignment is right and the
        // others are non-additive totals (unique counts etc.) — not an error.
        let mut fails = Vec::new();
        for tr in recon.iter().filter(|t| t.broadly_fails()) {
            for c in tr.cols.iter().filter(|c| !c.ok) {
                fails.push(format!(
                    "'{}' col {}: rows sum to {:.2} but total says {:.2}",
                    tr.label.trim(),
                    c.col,
                    c.sum,
                    c.total
                ));
            }
        }

        if fails.is_empty() {
            // Many independent subtotals reconciling is strong evidence.
            CheckOutcome::Pass { confidence: 0.97 }
        } else {
            CheckOutcome::Flag {
                reason: format!("arithmetic does not reconcile — {}", fails.join("; ")),
                severity: Severity::Error,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. StructuralValidity — column counts, header detection, empty cells (§2.2).
// ---------------------------------------------------------------------------

pub struct StructuralValidity;

impl QualityCheck for StructuralValidity {
    fn id(&self) -> CheckId {
        CheckId("structural_validity".into())
    }
    fn applies_to(&self, kind: ArtifactKind) -> bool {
        matches!(kind, ArtifactKind::HtmlTable | ArtifactKind::DbTable)
    }
    fn check(&self, artifact: &dyn Artifact, _ctx: &CheckCtx<'_>) -> CheckOutcome {
        let Some(t) = artifact.as_any().downcast_ref::<HtmlTable>() else {
            return CheckOutcome::Pass { confidence: 0.5 };
        };
        let model = TableModel::from_table(t);
        let nums = model.numeric_cols();
        let mut reasons = Vec::new();
        let mut hard = false; // error vs warn

        // Stray non-numeric text in a numeric column => a shifted/merged cell.
        let tv = type_violations(&model);
        if !tv.is_empty() {
            hard = true;
            let (r, c, ref txt) = tv[0];
            reasons.push(format!(
                "{} non-numeric cell(s) in numeric column(s) (e.g. [{r},{c}] {txt:?})",
                tv.len()
            ));
        }

        // Empty cells and ragged fill among DATA rows only — section-label rows
        // and multi-level header blanks are legitimate and not counted.
        if !nums.is_empty() {
            let fills: Vec<usize> = (model.header_rows..model.n_rows)
                .filter(|&r| model.row_kinds[r] == crate::analysis::RowKind::Data)
                .map(|r| {
                    nums.iter()
                        .filter(|&&c| !cell_blank(&model, r, c))
                        .count()
                })
                .collect();
            let empty_data: usize = fills.iter().map(|&f| nums.len() - f).sum();
            let ragged = fills.iter().any(|&f| f != 0 && f != nums.len());
            if empty_data > 0 {
                hard = true;
                reasons.push(format!("{empty_data} empty cell(s) in data rows"));
            }
            if ragged {
                reasons.push("ragged numeric-cell counts across data rows".into());
            }
        }

        // Parse-time signals not derivable from the grid.
        let rk = artifact.risk();
        // 0.0 means "not measured" (born-digital); only flag a real low value.
        if rk.min_ocr_confidence > 0.0 && rk.min_ocr_confidence < 0.85 {
            reasons.push(format!("low OCR confidence {:.2}", rk.min_ocr_confidence));
            if rk.min_ocr_confidence < 0.6 {
                hard = true;
            }
        }
        if rk.rotated_text {
            reasons.push("rotated text in region".into());
        }

        if reasons.is_empty() {
            CheckOutcome::Pass { confidence: 0.9 }
        } else {
            CheckOutcome::Flag {
                reason: reasons.join("; "),
                severity: if hard { Severity::Error } else { Severity::Warn },
            }
        }
    }
}

fn cell_blank(model: &TableModel, r: usize, c: usize) -> bool {
    model
        .grid
        .get(r)
        .and_then(|row| row.get(c))
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// AnswerSupport — claim-time vision verification (brief §2.3, §4).
// ---------------------------------------------------------------------------

/// A claim an agent intends to make, citing a specific element.
#[derive(Clone, Debug)]
pub struct Claim {
    pub element: ArtifactId,
    pub anchor: SourceAnchor,
    /// The value/text the agent asserts the source supports.
    pub asserted: String,
}

#[derive(Clone, Debug)]
pub enum SupportOutcome {
    Supported { confidence: f32 },
    Unsupported { reason: String },
}

/// Distinct from `QualityCheck`: takes a claim + the cited region, not just an
/// artifact. A real impl crops the source bbox and asks a vision model "does
/// this support the claim?". This deterministic stand-in re-reads the source
/// spans inside the cited bbox and checks the asserted value is actually there —
/// genuinely catches both parse errors and agent misreadings, no model needed.
pub trait AnswerSupport: Send + Sync {
    fn verify(&self, claim: &Claim, source: &QDoc) -> SupportOutcome;
}

pub struct SourceCropVerifier;

impl AnswerSupport for SourceCropVerifier {
    fn verify(&self, claim: &Claim, source: &QDoc) -> SupportOutcome {
        let SourceAnchor::Pdf { page, bbox, .. } = &claim.anchor else {
            return SupportOutcome::Unsupported {
                reason: "non-PDF anchor not supported by stub verifier".into(),
            };
        };
        let Some(p) = source.page(*page) else {
            return SupportOutcome::Unsupported {
                reason: format!("page {page} missing from source"),
            };
        };
        // Read every span whose center falls in the cited crop.
        let crop_text: String = p
            .spans
            .iter()
            .filter(|s| bbox.contains_center(&s.bbox()))
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        let want = normalize(&claim.asserted);
        let have = normalize(&crop_text);
        if want.is_empty() {
            return SupportOutcome::Unsupported {
                reason: "empty claim".into(),
            };
        }
        if have.contains(&want) {
            SupportOutcome::Supported { confidence: 0.9 }
        } else {
            SupportOutcome::Unsupported {
                reason: format!("claim {:?} not found in cited crop {:?}", claim.asserted, crop_text),
            }
        }
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '$')
        .collect::<String>()
        .to_lowercase()
}
