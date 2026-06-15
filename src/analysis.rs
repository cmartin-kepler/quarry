//! Table semantics shared by the detectors and the `explain` evidence report.
//!
//! Real financial tables aren't flat grids of numbers: they have multi-row
//! headers, section-label rows ("Exclude:", "Add (subtract):", "Quarter Ended
//! …"), ratio/percentage columns, and subtotal/total rows. Treating them as flat
//! is exactly what made the naive detectors misfire (summing a date header row,
//! or flagging legitimate section blanks). This module builds a structured
//! `TableModel` — typed columns + classified rows — and a section-aware
//! reconciliation engine, so both `check` and `explain` reason about the table
//! the way an analyst would.

use crate::artifact::HtmlTable;

/// Column role, inferred from the data cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColType {
    Label,   // text column (usually col 0)
    Numeric, // addable values (currency / counts)
    Ratio,   // percentages — NOT addable
    Empty,   // no data
}

/// Row role, inferred from content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Header,  // leading header row(s) — labels, period dates, units
    Section, // a label with no numeric data ("Exclude:", "Quarter Ended …")
    Data,    // a normal line item
    Total,   // a subtotal / total line (by label keyword)
}

pub struct TableModel {
    pub grid: Vec<Vec<String>>,
    pub n_rows: usize,
    pub n_cols: usize,
    pub header_rows: usize,
    pub col_types: Vec<ColType>,
    pub row_kinds: Vec<RowKind>,
}

const TOTAL_KEYWORDS: [&str; 7] = [
    "total",
    "net ",
    "sum",
    "excluding certain items",
    "subtotal",
    "grand total",
    "gross profit",
];

impl TableModel {
    pub fn from_table(t: &HtmlTable) -> TableModel {
        let grid = t.grid();
        let n_rows = grid.len();
        let n_cols = grid.iter().map(|r| r.len()).max().unwrap_or(0);

        // Header rows: leading contiguous rows that carry header cells. The
        // parser marks them (cheap parser: row 0; Docling: the real header
        // block). Fall back to "row 0 is header" when nothing is marked.
        let mut header_flag = vec![false; n_rows];
        for c in &t.cells {
            if c.is_header && (c.row as usize) < n_rows {
                header_flag[c.row as usize] = true;
            }
        }
        let mut header_rows = 0;
        while header_rows < n_rows && header_flag[header_rows] {
            header_rows += 1;
        }
        if header_rows == 0 && n_rows > 1 {
            header_rows = 1;
        }

        let col_types = infer_col_types(&grid, n_cols, header_rows);
        let row_kinds = classify_rows(&grid, n_rows, header_rows, &col_types);

        TableModel { grid, n_rows, n_cols, header_rows, col_types, row_kinds }
    }

    pub fn numeric_cols(&self) -> Vec<usize> {
        (0..self.n_cols)
            .filter(|&c| self.col_types.get(c) == Some(&ColType::Numeric))
            .collect()
    }

    fn cell(&self, r: usize, c: usize) -> &str {
        self.grid.get(r).and_then(|row| row.get(c)).map(String::as_str).unwrap_or("")
    }

    pub fn label(&self, r: usize) -> &str {
        self.cell(r, 0)
    }
}

fn infer_col_types(grid: &[Vec<String>], n_cols: usize, header_rows: usize) -> Vec<ColType> {
    let mut types = Vec::with_capacity(n_cols);
    for c in 0..n_cols {
        let mut nonempty = 0;
        let mut numeric = 0;
        let mut ratio = 0;
        for row in grid.iter().skip(header_rows) {
            let cell = row.get(c).map(String::as_str).unwrap_or("").trim();
            if cell.is_empty() {
                continue;
            }
            nonempty += 1;
            if cell.ends_with('%') {
                ratio += 1;
            } else if parse_num(cell).is_some() {
                numeric += 1;
            }
        }
        types.push(if nonempty == 0 {
            ColType::Empty
        } else if ratio * 2 > nonempty {
            ColType::Ratio
        } else if numeric * 2 >= nonempty {
            ColType::Numeric
        } else {
            ColType::Label
        });
    }
    types
}

fn classify_rows(
    grid: &[Vec<String>],
    n_rows: usize,
    header_rows: usize,
    col_types: &[ColType],
) -> Vec<RowKind> {
    let numeric_cols: Vec<usize> = (0..col_types.len())
        .filter(|&c| col_types[c] == ColType::Numeric)
        .collect();
    let mut kinds = vec![RowKind::Data; n_rows];
    for (r, kind) in kinds.iter_mut().enumerate() {
        if r < header_rows {
            *kind = RowKind::Header;
            continue;
        }
        let label = grid[r].first().map(String::as_str).unwrap_or("").to_lowercase();
        let filled_numeric = numeric_cols
            .iter()
            .filter(|&&c| !grid[r].get(c).map(String::as_str).unwrap_or("").trim().is_empty())
            .count();
        if is_total_label(&label) && filled_numeric > 0 {
            *kind = RowKind::Total;
        } else if filled_numeric == 0 && !label.trim().is_empty() {
            *kind = RowKind::Section;
        } else {
            *kind = RowKind::Data;
        }
    }
    kinds
}

fn is_total_label(label: &str) -> bool {
    TOTAL_KEYWORDS.iter().any(|k| label.contains(k))
}

// ---- Section-aware reconciliation ----------------------------------------

#[derive(Clone, Debug)]
pub struct ColRecon {
    pub col: usize,
    pub total: f64,
    pub sum: f64,
    pub ok: bool,
}

#[derive(Clone, Debug)]
pub struct TotalRecon {
    pub row: usize,
    pub label: String,
    /// The data rows summed for this total (for evidence).
    pub block: Vec<usize>,
    pub cols: Vec<ColRecon>,
}

impl TotalRecon {
    pub fn all_ok(&self) -> bool {
        !self.cols.is_empty() && self.cols.iter().all(|c| c.ok)
    }
    pub fn any_fail(&self) -> bool {
        self.cols.iter().any(|c| !c.ok)
    }
    /// At least one column reconciles — proof the columns are aligned, so any
    /// non-reconciling columns are non-additive totals (unique counts, averages,
    /// deduplicated totals), NOT a parse error.
    pub fn any_ok(&self) -> bool {
        self.cols.iter().any(|c| c.ok)
    }
    /// No column reconciles despite having some to check — the broad failure that
    /// signals a real misalignment / mis-parse.
    pub fn broadly_fails(&self) -> bool {
        !self.cols.is_empty() && !self.any_ok()
    }
}

/// For each Total row, sum the Data rows since the previous Total (Section rows
/// are skipped, not treated as boundaries — "Exclude:"/"Add (subtract):" are
/// annotations within a block), per Numeric column. Ratio/Label columns are
/// never summed.
pub fn reconcile(model: &TableModel, rel_tol: f64, abs_tol: f64) -> Vec<TotalRecon> {
    let numeric_cols = model.numeric_cols();
    let mut out = Vec::new();
    let mut block: Vec<usize> = Vec::new();

    for r in model.header_rows..model.n_rows {
        match model.row_kinds[r] {
            RowKind::Data => block.push(r),
            RowKind::Section => {} // annotation; keep accumulating
            RowKind::Header => {}
            RowKind::Total => {
                let mut cols = Vec::new();
                for &c in &numeric_cols {
                    let total_cell = model.grid[r].get(c).map(String::as_str).unwrap_or("");
                    // A percentage total means this column is a ratio, not an
                    // addable amount (e.g. a "Change %" column whose "%" was
                    // split off into a neighbour) — don't try to sum it.
                    if total_cell.trim().ends_with('%') {
                        continue;
                    }
                    let Some(total) = parse_num(total_cell) else {
                        continue;
                    };
                    let mut sum = 0.0;
                    let mut any = false;
                    for &br in &block {
                        if let Some(v) = parse_num(model.grid[br].get(c).map(String::as_str).unwrap_or("")) {
                            sum += v;
                            any = true;
                        }
                    }
                    if !any {
                        continue;
                    }
                    let ok = (sum - total).abs() <= abs_tol
                        || (sum - total).abs() / total.abs().max(1.0) <= rel_tol;
                    cols.push(ColRecon { col: c, total, sum, ok });
                }
                if !cols.is_empty() {
                    out.push(TotalRecon {
                        row: r,
                        label: model.label(r).to_string(),
                        block: block.clone(),
                        cols,
                    });
                }
                block.clear();
            }
        }
    }
    out
}

/// Cells in a Numeric column that don't parse as numbers (excluding empties and
/// section/header rows) — a strong mis-parse signal (a shifted or merged cell
/// dropped text into a number column).
pub fn type_violations(model: &TableModel) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    for &c in &model.numeric_cols() {
        for r in model.header_rows..model.n_rows {
            if model.row_kinds[r] == RowKind::Section {
                continue;
            }
            let cell = model.grid[r].get(c).map(String::as_str).unwrap_or("").trim();
            if cell.is_empty() || is_placeholder(cell) {
                continue;
            }
            if parse_num(cell).is_none() && !cell.ends_with('%') {
                out.push((r, c, cell.to_string()));
            }
        }
    }
    out
}

/// A "no value" marker that's legitimate in a numeric column (not a mis-parse):
/// dashes, n/a, nm, etc.
pub fn is_placeholder(s: &str) -> bool {
    matches!(
        s.trim().to_lowercase().as_str(),
        "-" | "—" | "–" | "--" | "n/a" | "na" | "nm" | "n/m"
    )
}

/// Parse a financial-table number: strips `$ , %`, treats `(123)` as negative
/// (accounting style), even with a currency prefix like `$ (902)`.
pub fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // Letters mean it isn't a bare number: rejects footnote-tagged labels like
    // "Revenue (1)" (which the paren rule would otherwise read as -1) and date
    // headers like "March 28, 2026".
    if t.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let neg = t.contains('(') && t.contains(')');
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
        assert_eq!(parse_num("$ (902)"), Some(-902.0)); // currency-prefixed negative
        assert_eq!(parse_num("12%"), Some(12.0));
        assert_eq!(parse_num("—"), None);
        assert_eq!(parse_num("Revenue"), None);
    }
}
