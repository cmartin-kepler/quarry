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
// 3. ReconstructionError — the "autoencoder" detector (brief §2, silent-failure
//    catch-rate experiment). The cheaper intrinsic checks only see contradictions
//    *inside* one table; this one looks *outside* it, at the source words the
//    table was built from.
// ---------------------------------------------------------------------------

/// A faithful parse reprojects onto the source words it came from: the multiset
/// of tokens the table reconstructed (its cells) should equal the multiset of
/// source words actually present in the table's region. A dropped word, a
/// spurious/stray token, or a merge/split boundary error leaves a residual even
/// when the grid is internally consistent and the arithmetic reconciles —
/// catching silent failures the other two detectors structurally cannot.
///
/// Its honest blind spot: a *pure rearrangement* (a value-swap or column
/// transpose that keeps every token) reprojects with zero residual, so it is
/// invisible here — that residue is what the vision/human adjudication rungs are
/// for. Needs source geometry (cells with real PDF anchors + `ctx.source`); it is
/// a no-op (Pass) when the artifact has no anchors or the source has no words in
/// the region — e.g. a hand-built grid with no backing document.
pub struct ReconstructionError {
    /// Fraction of source tokens left unaccounted-for (dropped + spurious) to
    /// tolerate before flagging. Absorbs tokenization quirks (footnote marks,
    /// stray units); a real mis-parse blows well past it.
    pub max_residual: f32,
}

impl Default for ReconstructionError {
    fn default() -> Self {
        ReconstructionError { max_residual: 0.15 }
    }
}

impl QualityCheck for ReconstructionError {
    fn id(&self) -> CheckId {
        CheckId("reconstruction_error".into())
    }
    fn applies_to(&self, kind: ArtifactKind) -> bool {
        matches!(kind, ArtifactKind::HtmlTable | ArtifactKind::DbTable)
    }
    fn check(&self, artifact: &dyn Artifact, ctx: &CheckCtx<'_>) -> CheckOutcome {
        use std::collections::HashMap;
        let Some(t) = artifact.as_any().downcast_ref::<HtmlTable>() else {
            return CheckOutcome::Pass { confidence: 0.5 };
        };

        // Region = the *detected* table region (the provenance anchor), which spans
        // the positions of any words the parse DROPPED. Scoping to the union of
        // captured cells instead would shrink exactly when words go missing —
        // hiding the drop. Fall back to the cell union only if no region is on file.
        let (page, region) = match &t.meta.provenance {
            Provenance::Source(SourceAnchor::Pdf { page, bbox, .. }) if bbox.area() > 1.0 => {
                (*page, *bbox)
            }
            _ => {
                let mut page = None;
                let mut region: Option<BBox> = None;
                for c in &t.cells {
                    if let SourceAnchor::Pdf { page: p, bbox, .. } = &c.anchor {
                        if bbox.area() <= 0.0 {
                            continue;
                        }
                        page = Some(*p);
                        region = Some(region.map_or(*bbox, |r| r.union(bbox)));
                    }
                }
                match (page, region) {
                    (Some(p), Some(r)) => (p, r),
                    _ => return CheckOutcome::Pass { confidence: 0.5 }, // no geometry — N/A
                }
            }
        };
        let Some(pg) = ctx.source.page(page) else {
            return CheckOutcome::Pass { confidence: 0.5 };
        };

        // Source tokens actually in the region.
        let mut src: HashMap<String, i64> = HashMap::new();
        for s in &pg.spans {
            if region.contains_center(&s.bbox()) {
                for tok in recon_tokens(&s.text) {
                    *src.entry(tok).or_default() += 1;
                }
            }
        }
        if src.is_empty() {
            return CheckOutcome::Pass { confidence: 0.5 }; // no source words here — N/A
        }

        // Tokens the table reconstructed.
        let mut rec: HashMap<String, i64> = HashMap::new();
        for c in &t.cells {
            for tok in recon_tokens(&c.text) {
                *rec.entry(tok).or_default() += 1;
            }
        }

        let total: i64 = src.values().sum();
        let (mut drop_n, mut spur_n) = (0i64, 0i64);
        let (mut dropped, mut spurious) = (Vec::new(), Vec::new());
        for (tok, &n) in &src {
            let have = rec.get(tok).copied().unwrap_or(0);
            if have < n {
                drop_n += n - have;
                if dropped.len() < 4 {
                    dropped.push(tok.clone());
                }
            }
        }
        for (tok, &n) in &rec {
            let have = src.get(tok).copied().unwrap_or(0);
            if have < n {
                spur_n += n - have;
                if spurious.len() < 4 {
                    spurious.push(tok.clone());
                }
            }
        }
        let residual = (drop_n + spur_n) as f32 / total.max(1) as f32;
        if residual <= self.max_residual {
            return CheckOutcome::Pass {
                confidence: (1.0 - residual).clamp(0.0, 1.0),
            };
        }
        let mut parts = Vec::new();
        if drop_n > 0 {
            parts.push(format!(
                "{drop_n} source word(s) absent from the parse (e.g. {dropped:?})"
            ));
        }
        if spur_n > 0 {
            parts.push(format!(
                "{spur_n} parsed token(s) absent from the source (e.g. {spurious:?})"
            ));
        }
        CheckOutcome::Flag {
            reason: format!(
                "reconstruction residual {:.0}% — {}",
                residual * 100.0,
                parts.join("; ")
            ),
            severity: if residual > 0.30 {
                Severity::Error
            } else {
                Severity::Warn
            },
        }
    }
}

/// Normalize a fragment into comparable tokens: split on whitespace, then keep
/// only alphanumerics and `.-%` per word (so `$1,234` and `1,234` and `(1234)`
/// all become `1234`, and an em-dash placeholder drops out entirely). Lowercased.
fn recon_tokens(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '%')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// 4. CrossTierAgreement — the INDEPENDENT-reconstruction detector. The only one
//    that can catch a token-preserving rearrangement (value swap, column
//    transpose, header swap), because it brings in information the parser under
//    test did not use: a second, independently-derived parse of the same region.
// ---------------------------------------------------------------------------

/// Compare two parses of the same region by GEOMETRY. Match each cell to the
/// best-overlapping cell in the other parse — a correspondence that is *stable*
/// because both parsers anchor to the same source coordinates — then compare
/// their contents. A rearrangement (value swap, column transpose, header swap)
/// puts different content at the same physical location, so it surfaces as a
/// disagreement. Keying on position rather than on label text is the whole point:
/// it keeps working when the two parsers tokenize labels/headers differently —
/// the brittleness that made a text-keyed comparison go dark on real tables.
///
/// Content comparison is asymmetric by type: numbers must match exactly on their
/// digits ("$1,234" == "1,234", but 100 ≠ 1000), while text agrees on substring
/// containment so wrapping/footnotes ("North America" vs "North America (a)")
/// don't read as disagreement.
///
/// Honest bounds, reported rather than hidden:
/// - **Independence-limited.** If both tiers make the *same* error they agree.
///   The catch rate is bounded by how differently the two parsers fail.
/// - **Geometry-required.** Cells must carry source bboxes; with too few
///   overlapping pairs to compare it returns a low-confidence Pass — a "could not
///   judge", never a "looks fine".
pub fn cross_tier_agreement(a: &HtmlTable, b: &HtmlTable) -> CheckOutcome {
    let (ca, cb) = (cells_with_box(a), cells_with_box(b));
    if ca.len() < 2 || cb.is_empty() {
        return CheckOutcome::Pass { confidence: 0.4 };
    }
    let mut matched = 0usize;
    let mut disagree: Vec<(String, String)> = Vec::new();
    for (ba, ta) in &ca {
        // The counterpart cell: the other parse's best-overlapping content.
        let best = cb
            .iter()
            .map(|(bb, tb)| (ba.iou(bb), tb))
            .filter(|(iou, _)| *iou > 0.1)
            .max_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
        let Some((_, tb)) = best else { continue };
        matched += 1;
        if !contents_agree(ta, tb) {
            disagree.push((ta.clone(), tb.clone()));
        }
    }
    if matched < 2 {
        return CheckOutcome::Pass { confidence: 0.4 }; // nothing to compare — N/A
    }
    if disagree.is_empty() {
        return CheckOutcome::Pass { confidence: 0.92 }; // independent tiers agree
    }
    disagree.sort();
    let ex = disagree
        .iter()
        .take(3)
        .map(|(va, vb)| format!("tier-A={va:?} vs tier-B={vb:?}"))
        .collect::<Vec<_>>()
        .join("; ");
    CheckOutcome::Flag {
        reason: format!(
            "{} of {matched} overlapping cell(s) disagree across independent parser tiers — {ex}",
            disagree.len()
        ),
        severity: Severity::Error,
    }
}

/// Non-empty cells that carry usable source geometry.
fn cells_with_box(t: &HtmlTable) -> Vec<(BBox, String)> {
    t.cells
        .iter()
        .filter_map(|c| match &c.anchor {
            SourceAnchor::Pdf { bbox, .. } if bbox.area() > 0.0 && !c.text.trim().is_empty() => {
                Some((*bbox, c.text.clone()))
            }
            _ => None,
        })
        .collect()
}

fn contents_agree(a: &str, b: &str) -> bool {
    if is_numlike(a) && is_numlike(b) {
        return digits(a) == digits(b); // exact: 100 must not "match" 1000
    }
    let (na, nb) = (text_norm(a), text_norm(b));
    // Punctuation-only cells (a stray "$" one parser split off) carry no comparable
    // content — treat as agreement, not a spurious disagreement.
    if na.is_empty() || nb.is_empty() {
        return true;
    }
    na == nb || na.contains(&nb) || nb.contains(&na)
}

fn is_numlike(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit()) && !s.chars().any(|c| c.is_alphabetic())
}

fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn text_norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
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
