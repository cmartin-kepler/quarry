//! Materialize an `HtmlTable` into a queryable `DbTable` (build-plan Stage 3, MVP).
//!
//! MVP scope: clean the cell strings (strip dot-leaders / collapse whitespace),
//! take the header row(s) as column names, and infer a type per column. The richer
//! normalizers — multi-index headers and section-header promotion — are deferred.

use crate::artifact::{ColType, DbTable, HtmlTable, Meta};
use crate::core::{ArtifactId, DocHash, Generation, Origin, Provenance, RiskMarkers};

/// Clean a raw cell: drop dot-leaders — both consecutive (`13......`) and
/// space-separated (`1965 . . . . .`) — while keeping decimals (`13.5`), and
/// collapse/trim whitespace. Per whitespace token, strip trailing dots; an all-dots
/// token then becomes empty and is dropped.
pub fn clean_cell(s: &str) -> String {
    s.split_whitespace()
        .map(|t| t.trim_end_matches('.'))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a cleaned cell as a number ($, commas, %, and `(123)` negatives handled).
fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let neg = t.starts_with('(') && t.ends_with(')');
    let body: String = t
        .chars()
        .filter(|c| !matches!(c, ',' | '$' | '%' | '(' | ')' | ' '))
        .collect();
    body.parse::<f64>().ok().map(|v| if neg { -v } else { v })
}

/// Infer a column's type from its data cells.
fn infer_coltype<'a>(cells: impl Iterator<Item = &'a String>) -> ColType {
    let (mut any, mut all_num, mut all_int) = (false, true, true);
    for c in cells {
        if c.trim().is_empty() {
            continue;
        }
        any = true;
        match parse_num(c) {
            Some(v) if v.fract() != 0.0 => all_int = false,
            Some(_) => {}
            None => all_num = false,
        }
    }
    match (any, all_num, all_int) {
        (false, ..) => ColType::Empty,
        (true, true, true) => ColType::Int,
        (true, true, false) => ColType::Float,
        (true, false, _) => ColType::Text,
    }
}

/// `HtmlTable → DbTable`. Header = the leading rows whose cells are `is_header`
/// (multi-level headers are flattened by joining with a space — MVP); the rest are
/// data rows.
pub fn materialize(t: &HtmlTable, generation: Generation) -> DbTable {
    let (nr, nc) = (t.n_rows as usize, t.n_cols as usize);
    let mut grid = vec![vec![String::new(); nc]; nr];
    let mut row_hdr = vec![false; nr];
    for c in &t.cells {
        let (r, col) = (c.row as usize, c.col as usize);
        if r < nr && col < nc {
            grid[r][col] = clean_cell(&c.text);
            if c.is_header {
                row_hdr[r] = true;
            }
        }
    }

    // leading header rows; if none are marked, treat the first row as the header
    let mut hr = 0usize;
    while hr < nr && row_hdr[hr] {
        hr += 1;
    }
    if hr == 0 && nr > 1 {
        hr = 1;
    }

    let columns: Vec<String> = (0..nc)
        .map(|c| {
            let parts: Vec<&str> =
                (0..hr).map(|r| grid[r][c].as_str()).filter(|s| !s.is_empty()).collect();
            if parts.is_empty() {
                format!("col{c}")
            } else {
                parts.join(" ")
            }
        })
        .collect();

    let rows: Vec<Vec<String>> = grid[hr..].to_vec();
    let dtypes: Vec<ColType> =
        (0..nc).map(|c| infer_coltype(rows.iter().map(|row| &row[c]))).collect();

    let content = DocHash::of(format!("dbtable:{}", t.meta.id).as_bytes());
    DbTable {
        meta: Meta {
            id: ArtifactId::mint(&content, generation),
            content_hash: content,
            provenance: Provenance::Derived {
                parents: vec![t.meta.id.clone()],
                anchor: t.meta.provenance.anchor().clone(),
            },
            generation,
            risk: RiskMarkers::default(),
            origin: Origin::default(),
        },
        columns,
        dtypes,
        rows,
        source: t.meta.id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Cell, HtmlTable};
    use crate::core::{ArtifactId, BBox, SourceAnchor};

    #[test]
    fn cleans_dot_leaders_but_keeps_decimals() {
        assert_eq!(clean_cell("13......"), "13");
        assert_eq!(clean_cell("13.5"), "13.5");
        assert_eq!(clean_cell("Revenue ....  1,234"), "Revenue 1,234");
        assert_eq!(clean_cell("  spaced   out "), "spaced out");
        // space-separated dot-leaders (real Berkshire tables use these)
        assert_eq!(clean_cell("1965 . . . . . . ."), "1965");
        assert_eq!(clean_cell("Insurance-underwriting . . . ."), "Insurance-underwriting");
    }

    fn cell(r: u32, c: u32, text: &str, hdr: bool) -> Cell {
        Cell {
            row: r,
            col: c,
            text: text.into(),
            anchor: SourceAnchor::Pdf { doc: DocHash::of(b"d"), page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) },
            is_header: hdr,
        }
    }

    #[test]
    fn materializes_columns_rows_and_dtypes() {
        // header: Segment | Revenue ; rows: Parks/100, Studios/220.5, Total/320.5
        let cells = vec![
            cell(0, 0, "Segment", true),
            cell(0, 1, "Revenue", true),
            cell(1, 0, "Parks", false),
            cell(1, 1, "100......", false), // dot-leader noise
            cell(2, 0, "Studios", false),
            cell(2, 1, "220.5", false),
        ];
        let t = HtmlTable {
            meta: Meta {
                id: ArtifactId("t".into()),
                content_hash: DocHash::of(b"t"),
                provenance: Provenance::Source(SourceAnchor::Pdf { doc: DocHash::of(b"d"), page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) }),
                generation: Generation(0),
                risk: RiskMarkers::default(),
                origin: Origin::default(),
            },
            n_rows: 3,
            n_cols: 2,
            cells,
            html: String::new(),
        };
        let db = materialize(&t, Generation(0));
        assert_eq!(db.columns, vec!["Segment", "Revenue"]);
        assert_eq!(db.n_rows(), 2);
        assert_eq!(db.rows[0], vec!["Parks", "100"], "dot-leader cleaned");
        assert_eq!(db.dtypes[0], ColType::Text, "labels");
        assert_eq!(db.dtypes[1], ColType::Float, "100 + 220.5 → Float");
        assert_eq!(db.source, ArtifactId("t".into()));
    }
}
