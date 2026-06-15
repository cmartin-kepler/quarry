//! Quality checks run BEFORE an agent sees an artifact (brief §2, §4) — the
//! subsystem whose catch rate the eval harness exists to measure.
//!
//! `AnswerSupport` is deliberately NOT a `QualityCheck`: it is claim-time and
//! takes a claim + artifact, not just an artifact (a tension the brief flags in
//! §4). It gets its own trait.

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
        let grid = t.grid();
        if grid.len() < 2 {
            return CheckOutcome::Pass { confidence: 0.5 };
        }

        // Find a "total" row by label in column 0.
        let total_row = grid.iter().position(|row| {
            row.first()
                .map(|c| {
                    let l = c.to_lowercase();
                    l.contains("total") || l.contains("net ") || l == "sum"
                })
                .unwrap_or(false)
        });

        let Some(tr) = total_row else {
            // Nothing to check arithmetically; weak pass.
            return CheckOutcome::Pass { confidence: 0.6 };
        };

        let mut checked = 0;
        let mut mismatches = Vec::new();
        for col in 1..t.n_cols as usize {
            let Some(total) = parse_num(&grid[tr][col]) else {
                continue;
            };
            // Sum the data rows above the total row (skip header row 0).
            let mut sum = 0.0;
            let mut any = false;
            for row in grid.iter().take(tr).skip(1) {
                if let Some(v) = parse_num(&row[col]) {
                    sum += v;
                    any = true;
                }
            }
            if !any {
                continue;
            }
            checked += 1;
            let denom = total.abs().max(1.0);
            if (sum - total).abs() / denom > self.rel_tolerance {
                mismatches.push(format!(
                    "col {col}: rows sum to {sum:.2} but total row says {total:.2}"
                ));
            }
        }

        if !mismatches.is_empty() {
            CheckOutcome::Flag {
                reason: format!("arithmetic does not hold — {}", mismatches.join("; ")),
                severity: Severity::Error,
            }
        } else if checked > 0 {
            CheckOutcome::Pass { confidence: 0.95 }
        } else {
            CheckOutcome::Pass { confidence: 0.6 }
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
        // This check leans on the parse-time risk markers — they ARE the
        // structural signal, computed once during extraction.
        let r = artifact.risk();
        let mut reasons = Vec::new();
        if r.column_count_variance > 0.5 {
            reasons.push(format!(
                "ragged column counts (variance {:.2})",
                r.column_count_variance
            ));
        }
        if r.merged_cell_rows > 0 {
            reasons.push(format!("{} row(s) with missing cells", r.merged_cell_rows));
        }
        if r.empty_cells > 0 {
            reasons.push(format!("{} empty cell(s)", r.empty_cells));
        }
        if r.min_ocr_confidence < 0.85 {
            reasons.push(format!("low OCR confidence {:.2}", r.min_ocr_confidence));
        }
        if r.rotated_text {
            reasons.push("rotated text in region".into());
        }

        if reasons.is_empty() {
            CheckOutcome::Pass { confidence: 0.9 }
        } else {
            let severity = if r.merged_cell_rows > 0 || r.min_ocr_confidence < 0.6 {
                Severity::Error
            } else {
                Severity::Warn
            };
            CheckOutcome::Flag {
                reason: reasons.join("; "),
                severity,
            }
        }
    }
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

/// Parse a financial-table number: strips `$ , %`, handles `(123)` as negative.
pub fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let neg = t.starts_with('(') && t.ends_with(')');
    let cleaned: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    if cleaned.is_empty() || cleaned == "-" || cleaned == "." {
        return None;
    }
    let v: f64 = cleaned.parse().ok()?;
    Some(if neg { -v.abs() } else { v })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_financial_numbers() {
        assert_eq!(parse_num("$1,234.50"), Some(1234.50));
        assert_eq!(parse_num("(500)"), Some(-500.0));
        assert_eq!(parse_num("12%"), Some(12.0));
        assert_eq!(parse_num("—"), None);
        assert_eq!(parse_num("Revenue"), None);
    }
}
