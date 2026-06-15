//! "How do we know this table was parsed correctly?" — the evidence report.
//!
//! Without ground truth we can't *prove* a parse is right, but we can gather
//! signals that an analyst would use, each with concrete evidence:
//!
//! POSITIVE (suggests correct parse):
//!   - subtotal/total rows reconcile (many independent arithmetic constraints
//!     all holding is hard to achieve by accident — the strongest signal),
//!   - numeric columns parse cleanly (no stray text),
//!   - every data row fills its numeric columns (rectangular, consistent).
//!
//! NEGATIVE (suggests mis-parse):
//!   - a total doesn't reconcile,
//!   - non-numeric text sitting in a numeric column (a shifted/merged cell),
//!   - empty cells / ragged fill among data rows,
//!   - rotated text or low OCR confidence in the region.
//!
//! The overall impression weights reconciliation highest.

use crate::analysis::*;
use crate::artifact::{Artifact, HtmlTable};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impression {
    /// Arithmetic reconciles — strong evidence the parse is correct.
    Confirmed,
    /// No red flags, but no arithmetic to confirm it either.
    NoIssues,
    /// At least one signal suggests a mis-parse.
    Suspect,
}

impl Impression {
    pub fn label(self) -> &'static str {
        match self {
            Impression::Confirmed => "LIKELY CORRECT (arithmetic reconciles)",
            Impression::NoIssues => "NO ISSUES DETECTED (no arithmetic to confirm)",
            Impression::Suspect => "SUSPECT (see negative signals)",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Signal {
    pub positive: bool,
    pub detail: String,
}

pub struct TableEvidence {
    pub n_rows: usize,
    pub n_cols: usize,
    pub header_rows: usize,
    pub col_types: Vec<ColType>,
    pub row_kinds: Vec<RowKind>,
    pub signals: Vec<Signal>,
    pub impression: Impression,
}

impl TableEvidence {
    pub fn positives(&self) -> impl Iterator<Item = &Signal> {
        self.signals.iter().filter(|s| s.positive)
    }
    pub fn negatives(&self) -> impl Iterator<Item = &Signal> {
        self.signals.iter().filter(|s| !s.positive)
    }
}

pub fn assess(t: &HtmlTable) -> TableEvidence {
    let model = TableModel::from_table(t);
    let recon = reconcile(&model, 0.01, 0.06);
    let tv = type_violations(&model);
    let numeric_cols = model.numeric_cols();

    let mut signals = Vec::new();

    // --- Reconciliation (strongest signal) ---
    let total_checks: usize = recon.iter().map(|t| t.cols.len()).sum();
    let failing: Vec<&TotalRecon> = recon.iter().filter(|t| t.any_fail()).collect();
    if !recon.is_empty() {
        if failing.is_empty() {
            signals.push(Signal {
                positive: true,
                detail: format!(
                    "{} subtotal/total row(s) reconcile across {} column-check(s) — e.g. {}",
                    recon.len(),
                    total_checks,
                    recon
                        .first()
                        .map(|t| format!(
                            "'{}' col {} = {:.0}",
                            t.label.trim(),
                            t.cols[0].col,
                            t.cols[0].total
                        ))
                        .unwrap_or_default()
                ),
            });
        } else {
            for t in &failing {
                for c in t.cols.iter().filter(|c| !c.ok) {
                    signals.push(Signal {
                        positive: false,
                        detail: format!(
                            "'{}' col {}: rows sum to {:.2} but total says {:.2}",
                            t.label.trim(),
                            c.col,
                            c.sum,
                            c.total
                        ),
                    });
                }
            }
        }
    }

    // --- Type consistency ---
    if !numeric_cols.is_empty() {
        if tv.is_empty() {
            signals.push(Signal {
                positive: true,
                detail: format!(
                    "all {} numeric column(s) parse cleanly (no stray text)",
                    numeric_cols.len()
                ),
            });
        } else {
            let (r, c, ref txt) = tv[0];
            signals.push(Signal {
                positive: false,
                detail: format!(
                    "{} non-numeric cell(s) in numeric column(s) — e.g. [{r},{c}] {txt:?} (shifted/merged cell?)",
                    tv.len()
                ),
            });
        }
    }

    // --- Data-row completeness ---
    let data_rows: Vec<usize> = (model.header_rows..model.n_rows)
        .filter(|&r| model.row_kinds[r] == RowKind::Data)
        .collect();
    if !numeric_cols.is_empty() && !data_rows.is_empty() {
        let empty_data: usize = data_rows
            .iter()
            .map(|&r| {
                numeric_cols
                    .iter()
                    .filter(|&&c| blank(&model, r, c))
                    .count()
            })
            .sum();
        if empty_data == 0 {
            signals.push(Signal {
                positive: true,
                detail: format!(
                    "every data row fills all {} numeric column(s)",
                    numeric_cols.len()
                ),
            });
        } else {
            signals.push(Signal {
                positive: false,
                detail: format!("{empty_data} empty cell(s) in data rows"),
            });
        }
    }

    // --- Parse-time risk markers ---
    let rk = t.risk();
    if rk.rotated_text {
        signals.push(Signal { positive: false, detail: "rotated text in region".into() });
    }
    // 0.0 means "not measured" (born-digital); only flag a real low confidence.
    if rk.min_ocr_confidence > 0.0 && rk.min_ocr_confidence < 0.85 {
        signals.push(Signal {
            positive: false,
            detail: format!("low OCR confidence {:.2}", rk.min_ocr_confidence),
        });
    }

    let any_negative = signals.iter().any(|s| !s.positive);
    let reconciled = !recon.is_empty() && failing.is_empty();
    let impression = if any_negative {
        Impression::Suspect
    } else if reconciled {
        Impression::Confirmed
    } else {
        Impression::NoIssues
    };

    TableEvidence {
        n_rows: model.n_rows,
        n_cols: model.n_cols,
        header_rows: model.header_rows,
        col_types: model.col_types,
        row_kinds: model.row_kinds,
        signals,
        impression,
    }
}

// Truly empty only — a dash/"n/a" placeholder is an intentional "no value",
// not a missing cell, so it counts as filled.
fn blank(model: &TableModel, r: usize, c: usize) -> bool {
    model
        .grid
        .get(r)
        .and_then(|row| row.get(c))
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Cell, Meta};
    use crate::core::*;

    /// Build an HtmlTable from rows of text; the first `header_rows` rows are
    /// marked as header cells.
    fn tbl(rows: &[&[&str]], header_rows: usize) -> HtmlTable {
        let n_rows = rows.len() as u32;
        let n_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let anchor = SourceAnchor::Pdf {
            doc: DocHash::of(b"t"),
            page: 1,
            bbox: BBox::new(0.0, 0.0, 10.0, 10.0),
        };
        let mut cells = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            for (c, text) in row.iter().enumerate() {
                cells.push(Cell {
                    row: r as u32,
                    col: c as u32,
                    text: (*text).to_string(),
                    anchor: anchor.clone(),
                    is_header: r < header_rows,
                });
            }
        }
        let content = DocHash::of(format!("{rows:?}").as_bytes());
        HtmlTable {
            meta: Meta {
                id: ArtifactId::mint(&content, Generation(0)),
                content_hash: content,
                provenance: Provenance::Source(anchor),
                generation: Generation(0),
                risk: RiskMarkers { min_ocr_confidence: 1.0, ..Default::default() },
            },
            n_rows,
            n_cols,
            cells,
            html: String::new(),
        }
    }

    #[test]
    fn multi_section_footnoted_table_reconciles_and_is_confirmed() {
        // Two header rows, footnote-tagged labels, a section-label row, a split
        // percentage column, and a reconciling total — the real-world shape that
        // used to misfire.
        let t = tbl(
            &[
                &["", "Quarter Ended", "", "Change"],
                &["($ in millions)", "FY2026", "FY2025", "%"],
                &["Income before taxes (1)", "3,367", "3,087", "9 %"],
                &["Add (subtract):", "", "", ""],
                &["Corporate expenses (2)", "380", "395", "4 %"],
                &["Interest expense, net", "856", "954", "10 %"],
                &["Total segment operating income", "4,603", "4,436", "4 %"],
            ],
            2,
        );
        let ev = assess(&t);
        // Footnote labels must NOT make col 0 numeric.
        assert_eq!(ev.col_types[0], ColType::Label);
        // The two amount columns reconcile (3367+380+856 = 4603; 3087+395+954 = 4436).
        assert_eq!(ev.impression, Impression::Confirmed, "signals: {:?}", ev.signals);
        assert!(ev.negatives().count() == 0);
    }

    #[test]
    fn shifted_total_is_suspect() {
        let t = tbl(
            &[
                &["Item", "2024"],
                &["Cash", "100"],
                &["Debt", "200"],
                &["Total assets", "999"], // 100+200 = 300, not 999
            ],
            1,
        );
        let ev = assess(&t);
        assert_eq!(ev.impression, Impression::Suspect);
        assert!(ev.negatives().any(|s| s.detail.contains("Total assets")));
    }

    #[test]
    fn clean_table_without_totals_is_no_issues() {
        let t = tbl(
            &[&["Mode", "Value"], &["A", "10"], &["B", "20"]],
            1,
        );
        let ev = assess(&t);
        assert_eq!(ev.impression, Impression::NoIssues);
    }
}
