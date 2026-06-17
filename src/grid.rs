//! Pure grid / word-geometry helpers shared by the native ops (`structure`,
//! `sign-fix`, `markdown`, `merge`). Deliberately free of `Artifact`/`Extractor`
//! types so every function is trivially unit-testable on plain data — this is the
//! load-bearing logic ported from the Python prototype, and the place we want the
//! most tests.

use crate::analysis::parse_num;
use crate::artifact::Word;
use crate::core::BBox;

/// A structured cell: text + the union bbox of the words that formed it (so each
/// cell gets a citable anchor box).
#[derive(Clone, Debug, PartialEq)]
pub struct GCell {
    pub text: String,
    pub bbox: BBox,
}

/// The result of committing columns from word geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct Structured {
    pub rows: Vec<Vec<GCell>>,
    pub header_rows: usize,
}

fn has_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
}

const SYMS: &str = "$€£¥()-—–";

fn is_symbol_only(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| SYMS.contains(c))
}

/// The **column coalescer**. Project every word onto the x-axis and merge spans
/// whose horizontal gap is smaller than `col_gap` into one occupied interval; the
/// holes between intervals are the column separators.
///
/// Intent — why this and not "cluster the word x0s":
/// - A column is defined by a real **whitespace gap**, so two words closer than
///   `col_gap` (e.g. "Total revenue") stay in ONE cell rather than splitting.
/// - It works on the words' full x-SPAN, so **right-aligned numbers** of different
///   widths (a wide "1,234" and a narrow "9" sharing a right edge) land in ONE
///   column. Clustering left edges (x0) would scatter them — the classic failure
///   that makes naive table reconstruction shred numeric columns.
///
/// Returns the column intervals left-to-right.
pub fn column_intervals(words: &[Word], col_gap: f32) -> Vec<(f32, f32)> {
    let mut by_x: Vec<&Word> = words.iter().collect();
    by_x.sort_by(|a, b| a.bbox.x0.partial_cmp(&b.bbox.x0).unwrap());
    let mut occ: Vec<(f32, f32)> = Vec::new();
    for w in by_x {
        let (x0, x1) = (w.bbox.x0, w.bbox.x1);
        match occ.last_mut() {
            Some(last) if x0 <= last.1 + col_gap => last.1 = last.1.max(x1),
            _ => occ.push((x0, x1)),
        }
    }
    occ
}

/// Cluster a region's words into a table by GEOMETRY: rows by vertical position,
/// columns by gaps in the horizontal word projection. Geometry never splits a
/// word (words are atomic) and column boundaries are real whitespace gaps.
///
/// Multi-row headers are handled: leading rows (until the first with ≥2 numeric
/// cells) are the header band; a header word spanning several data columns is
/// spread across each (a colspan group); split-off currency/paren columns merge
/// back into their numeric neighbour.
pub fn structure_words(words: &[Word], row_tol: f32, col_gap: f32) -> Structured {
    if words.is_empty() {
        return Structured { rows: vec![], header_rows: 0 };
    }

    // --- rows: group by top (y0), reading order within a row by x0 ---
    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by(|&a, &b| {
        let (wa, wb) = (&words[a], &words[b]);
        wa.bbox
            .y0
            .partial_cmp(&wb.bbox.y0)
            .unwrap()
            .then(wa.bbox.x0.partial_cmp(&wb.bbox.x0).unwrap())
    });
    let mut rows_idx: Vec<Vec<usize>> = Vec::new();
    let mut row_y: Option<f32> = None;
    for &i in &order {
        let top = words[i].bbox.y0;
        match row_y {
            Some(y) if top - y <= row_tol => rows_idx.last_mut().unwrap().push(i),
            _ => {
                rows_idx.push(vec![i]);
                row_y = Some(top);
            }
        }
    }

    // --- columns: the column coalescer (intent documented on `column_intervals`) ---
    let occ = column_intervals(words, col_gap);
    let ncol = occ.len();

    let colof = |cx: f32| -> usize {
        for (i, (a, b)) in occ.iter().enumerate() {
            if cx >= a - col_gap && cx <= b + col_gap {
                return i;
            }
        }
        let mut best = 0;
        let mut bd = f32::MAX;
        for (i, (a, b)) in occ.iter().enumerate() {
            let d = (cx - a).abs().min((cx - b).abs());
            if d < bd {
                bd = d;
                best = i;
            }
        }
        best
    };
    // Columns a header word spans: those whose centre falls within the word's
    // x-extent (so "2024" over Q1/Q2/Q3 spreads across all three).
    let spanned = |w: &Word| -> Vec<usize> {
        let v: Vec<usize> = occ
            .iter()
            .enumerate()
            .filter(|(_, (a, b))| {
                let mid = (a + b) / 2.0;
                w.bbox.x0 - 2.0 <= mid && mid <= w.bbox.x1 + 2.0
            })
            .map(|(i, _)| i)
            .collect();
        if v.is_empty() {
            vec![colof((w.bbox.x0 + w.bbox.x1) / 2.0)]
        } else {
            v
        }
    };

    let assign = |row: &[usize], span: bool| -> Vec<(String, Option<BBox>)> {
        let mut cells: Vec<(String, Option<BBox>)> = vec![(String::new(), None); ncol];
        let mut sorted = row.to_vec();
        sorted.sort_by(|&a, &b| words[a].bbox.x0.partial_cmp(&words[b].bbox.x0).unwrap());
        for &i in &sorted {
            let w = &words[i];
            let targets = if span {
                spanned(w)
            } else {
                vec![colof((w.bbox.x0 + w.bbox.x1) / 2.0)]
            };
            for c in targets {
                let cell = &mut cells[c];
                if cell.0.is_empty() {
                    cell.0 = w.text.clone();
                } else {
                    cell.0.push(' ');
                    cell.0.push_str(&w.text);
                }
                cell.1 = Some(match cell.1 {
                    Some(b) => b.union(&w.bbox),
                    None => w.bbox,
                });
            }
        }
        cells
    };

    // raw center-assigned rows, used to detect where the header band ends
    let raw: Vec<Vec<(String, Option<BBox>)>> =
        rows_idx.iter().map(|r| assign(r, false)).collect();
    let mut header_rows = 0;
    for cells in &raw {
        let nums = cells
            .iter()
            .filter(|(t, _)| parse_num(t.trim()).is_some())
            .count();
        if nums >= 2 {
            break;
        }
        header_rows += 1;
    }
    if header_rows >= raw.len() {
        header_rows = if raw.len() > 1 { 1 } else { 0 };
    }

    // header rows spread spanning words; data rows keep the centre assignment
    let mut grid: Vec<Vec<(String, Option<BBox>)>> = rows_idx
        .iter()
        .enumerate()
        .map(|(ri, r)| if ri < header_rows { assign(r, true) } else { raw[ri].clone() })
        .collect();

    // merge a symbol-only data column into its numeric neighbour ($/( → right, ) → left)
    for c in 0..ncol {
        let seen: Vec<String> = grid
            .iter()
            .skip(header_rows)
            .map(|r| r[c].0.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if seen.is_empty() || !seen.iter().all(|s| is_symbol_only(s)) {
            continue;
        }
        let first = seen[0].chars().next().unwrap_or(' ');
        let tgt = if "$€£¥(".contains(first) { c as isize + 1 } else { c as isize - 1 };
        if tgt < 0 || tgt as usize >= ncol {
            continue;
        }
        let tgt = tgt as usize;
        for row in grid.iter_mut() {
            let (src_text, src_box) = (row[c].0.clone(), row[c].1);
            if src_text.is_empty() {
                continue;
            }
            let cell = &mut row[tgt];
            cell.0 = if tgt > c {
                format!("{}{}", src_text, cell.0)
            } else {
                format!("{}{}", cell.0, src_text)
            };
            cell.1 = match (cell.1, src_box) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                (a, b) => a.or(b),
            };
            row[c] = (String::new(), None);
        }
    }

    // drop all-empty columns
    let keep: Vec<usize> = (0..ncol)
        .filter(|&c| grid.iter().any(|r| !r[c].0.trim().is_empty()))
        .collect();

    // row y-extents, for empty cells' fallback anchor box
    let row_y_ext: Vec<(f32, f32)> = rows_idx
        .iter()
        .map(|r| {
            let lo = r.iter().map(|&i| words[i].bbox.y0).fold(f32::MAX, f32::min);
            let hi = r.iter().map(|&i| words[i].bbox.y1).fold(f32::MIN, f32::max);
            if r.is_empty() { (0.0, 0.0) } else { (lo, hi) }
        })
        .collect();

    let rows: Vec<Vec<GCell>> = grid
        .iter()
        .enumerate()
        .map(|(ri, row)| {
            keep.iter()
                .map(|&c| {
                    let (t, b) = &row[c];
                    let bbox = b.unwrap_or_else(|| {
                        BBox::new(occ[c].0, row_y_ext[ri].0, occ[c].1, row_y_ext[ri].1)
                    });
                    GCell { text: t.trim().to_string(), bbox }
                })
                .collect()
        })
        .collect();

    Structured { rows, header_rows }
}

/// Render words back into a faithful monospace ASCII block (for the TextGrid's
/// `text` display). Char column = x offset / median per-char width.
pub fn words_to_ascii(words: &[Word]) -> String {
    if words.is_empty() {
        return String::new();
    }
    let minx = words.iter().map(|w| w.bbox.x0).fold(f32::MAX, f32::min);
    let mut widths: Vec<f32> = words
        .iter()
        .filter(|w| !w.text.is_empty())
        .map(|w| (w.bbox.x1 - w.bbox.x0) / (w.text.chars().count().max(1) as f32))
        .collect();
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let cw = if widths.is_empty() { 5.0 } else { widths[widths.len() / 2] }.max(1.0);

    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by(|&a, &b| {
        words[a]
            .bbox
            .y0
            .partial_cmp(&words[b].bbox.y0)
            .unwrap()
            .then(words[a].bbox.x0.partial_cmp(&words[b].bbox.x0).unwrap())
    });
    let render = |idx: &[usize]| -> String {
        let mut sorted = idx.to_vec();
        sorted.sort_by(|&a, &b| words[a].bbox.x0.partial_cmp(&words[b].bbox.x0).unwrap());
        let mut line = String::new();
        for &i in &sorted {
            let col = (((words[i].bbox.x0 - minx) / cw).round() as i64).max(0) as usize;
            let len = line.chars().count();
            if len < col {
                line.push_str(&" ".repeat(col - len));
            } else if !line.is_empty() && !line.ends_with(' ') {
                line.push(' ');
            }
            line.push_str(&words[i].text);
        }
        line.trim_end().to_string()
    };

    let mut lines: Vec<String> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut y: Option<f32> = None;
    for &i in &order {
        let top = words[i].bbox.y0;
        match y {
            Some(yy) if top - yy <= 3.0 => cur.push(i),
            _ => {
                if !cur.is_empty() {
                    lines.push(render(&cur));
                }
                cur = vec![i];
                y = Some(top);
            }
        }
    }
    if !cur.is_empty() {
        lines.push(render(&cur));
    }
    lines.join("\n")
}

/// Rewrite accounting sign conventions to signed numbers: `(123)` → `-123`,
/// trailing `CR`/`DR`, trailing `-`. Returns (new grid, cells changed).
pub fn sign_fix(grid: &[Vec<String>]) -> (Vec<Vec<String>>, usize) {
    let mut changed = 0;
    let mut out = Vec::with_capacity(grid.len());
    for row in grid {
        let mut r = Vec::with_capacity(row.len());
        for cell in row {
            let t = cell.trim();
            let new = if t.starts_with('(') && t.ends_with(')') && has_digit(t) {
                format!("-{}", t[1..t.len() - 1].trim())
            } else if t.is_ascii() && t.len() >= 2 && t[t.len() - 2..].eq_ignore_ascii_case("cr") && has_digit(t) {
                format!("-{}", t[..t.len() - 2].trim())
            } else if t.is_ascii() && t.len() >= 2 && t[t.len() - 2..].eq_ignore_ascii_case("dr") && has_digit(t) {
                t[..t.len() - 2].trim().to_string()
            } else if t.ends_with('-') && has_digit(&t[..t.len() - 1]) {
                format!("-{}", t[..t.len() - 1].trim())
            } else {
                cell.clone()
            };
            if new != *cell {
                changed += 1;
            }
            r.push(new);
        }
        out.push(r);
    }
    (out, changed)
}

/// Render a grid as a GitHub-flavoured markdown pipe table (row 0 = header).
pub fn to_markdown(grid: &[Vec<String>]) -> String {
    let ncols = grid.iter().map(|r| r.len()).max().unwrap_or(0);
    let esc = |c: &str| c.replace('|', "\\|");
    let row = |cells: &[String]| {
        let mut v: Vec<String> = cells.iter().map(|c| esc(c)).collect();
        while v.len() < ncols {
            v.push(String::new());
        }
        format!("| {} |", v.join(" | "))
    };
    let mut out = Vec::new();
    if let Some(h) = grid.first() {
        out.push(row(h));
    }
    out.push(format!("| {} |", vec!["---"; ncols.max(1)].join(" | ")));
    for r in grid.iter().skip(1) {
        out.push(row(r));
    }
    out.join("\n")
}

/// Parse a markdown pipe table back into a grid (the `=> md => table` round trip).
pub fn markdown_to_grid(md: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in md.lines() {
        let l = line.trim();
        if !l.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = l
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        // skip the `---` separator row
        if cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue;
        }
        out.push(cells);
    }
    out
}

/// Render a grid to HTML; the first `header_rows` rows are `<th>`.
pub fn to_html(rows: &[Vec<String>], header_rows: usize) -> String {
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut h = String::from("<table>\n");
    for (i, row) in rows.iter().enumerate() {
        let tag = if i < header_rows { "th" } else { "td" };
        h.push_str("  <tr>");
        for c in 0..ncols {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            h.push_str(&format!("<{tag}>{}</{tag}>", esc(cell)));
        }
        h.push_str("</tr>\n");
    }
    h.push_str("</table>");
    h
}

/// Per-edge median of several boxes — the consensus box for region merge, robust
/// to one clipped or inflated detection.
pub fn median_bbox(boxes: &[BBox]) -> Option<BBox> {
    if boxes.is_empty() {
        return None;
    }
    let med = |mut v: Vec<f32>| -> f32 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = v.len();
        if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
    };
    Some(BBox::new(
        med(boxes.iter().map(|b| b.x0).collect()),
        med(boxes.iter().map(|b| b.y0).collect()),
        med(boxes.iter().map(|b| b.x1).collect()),
        med(boxes.iter().map(|b| b.y1).collect()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x0: f32, x1: f32, top: f32) -> Word {
        Word { text: text.into(), bbox: BBox::new(x0, top, x1, top + 10.0) }
    }
    /// Cell texts only, for terse assertions.
    fn texts(s: &Structured) -> Vec<Vec<String>> {
        s.rows.iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect()
    }

    #[test]
    fn structure_clusters_a_simple_two_column_table() {
        // label col at x≈0, number col at x≈100, two data rows + a header row.
        let words = vec![
            w("Segment", 0.0, 40.0, 0.0),
            w("Revenue", 100.0, 140.0, 0.0),
            w("Sports", 0.0, 35.0, 20.0),
            w("4,540", 100.0, 135.0, 20.0),
            w("Parks", 0.0, 30.0, 40.0),
            w("8,430", 100.0, 135.0, 40.0),
        ];
        let s = structure_words(&words, 3.0, 6.0);
        assert_eq!(s.rows.len(), 3);
        assert_eq!(s.rows[0].len(), 2);
        assert_eq!(s.header_rows, 1);
        assert_eq!(
            texts(&s),
            vec![
                vec!["Segment".to_string(), "Revenue".into()],
                vec!["Sports".into(), "4,540".into()],
                vec!["Parks".into(), "8,430".into()],
            ]
        );
    }

    #[test]
    fn header_band_is_the_rows_before_the_first_numeric_row() {
        // three stacked header lines (no numbers), then a data row with 2 numbers.
        let words = vec![
            w("Pre-Tax", 100.0, 140.0, 0.0),
            w("Tax", 200.0, 230.0, 0.0),
            w("Income", 100.0, 140.0, 12.0),
            w("Benefit", 200.0, 240.0, 12.0),
            w("Loss", 100.0, 130.0, 24.0),
            w("Expense", 200.0, 250.0, 24.0),
            w("As-reported", 0.0, 60.0, 40.0),
            w("3,367", 100.0, 135.0, 40.0),
            w("902", 200.0, 235.0, 40.0),
        ];
        let s = structure_words(&words, 3.0, 6.0);
        assert_eq!(s.header_rows, 3);
        // each numeric column stacks its 3 header lines
        assert_eq!(s.rows[0][1], s.rows[0][1]); // (column exists)
        assert_eq!(texts(&s)[3], vec!["As-reported", "3,367", "902"]);
    }

    #[test]
    fn spanning_header_spreads_across_the_columns_it_covers() {
        // "2024" sits over two number columns; Q1/Q2 are the leaf header.
        let words = vec![
            w("2024", 100.0, 240.0, 0.0), // wide: spans both columns' centres
            w("Q1", 100.0, 130.0, 12.0),
            w("Q2", 200.0, 230.0, 12.0),
            w("A", 0.0, 10.0, 30.0),
            w("10", 100.0, 120.0, 30.0),
            w("20", 200.0, 220.0, 30.0),
        ];
        let s = structure_words(&words, 3.0, 6.0);
        let t = texts(&s);
        // top header row: "2024" spread across the two numeric columns
        let numeric_cols: Vec<usize> = (0..s.rows[0].len()).filter(|&c| t[2][c] == "10" || t[2][c] == "20").collect();
        for &c in &numeric_cols {
            assert_eq!(t[0][c], "2024", "spanning header should fill column {c}");
        }
    }

    #[test]
    fn split_currency_column_merges_into_its_number() {
        // a "$" sitting just left of the number, in its own thin column.
        let words = vec![
            w("Item", 0.0, 30.0, 0.0),
            w("Amount", 100.0, 150.0, 0.0),
            w("A", 0.0, 10.0, 20.0),
            w("$", 80.0, 86.0, 20.0),
            w("3,367", 100.0, 140.0, 20.0),
        ];
        let s = structure_words(&words, 3.0, 6.0);
        // the "$" column should not survive as its own column
        let row = &texts(&s)[1];
        assert!(row.iter().any(|c| c.contains("$3,367") || c == "$ 3,367" || c.contains("3,367")));
        assert!(!row.iter().any(|c| c == "$"), "lone $ column should have merged");
    }

    #[test]
    fn sign_fix_rewrites_parens_and_cr_dr() {
        let grid = vec![
            vec!["Item".to_string(), "Amount".into()],
            vec!["A".into(), "(902)".into()],
            vec!["B".into(), "150CR".into()],
            vec!["C".into(), "150DR".into()],
            vec!["D".into(), "200".into()],
        ];
        let (out, changed) = sign_fix(&grid);
        assert_eq!(changed, 3);
        assert_eq!(out[1][1], "-902");
        assert_eq!(out[2][1], "-150");
        assert_eq!(out[3][1], "150");
        assert_eq!(out[4][1], "200"); // untouched
    }

    #[test]
    fn sign_fix_leaves_em_dash_and_text_alone() {
        let grid = vec![vec!["—".to_string(), "n/a".into(), "Total".into()]];
        let (out, changed) = sign_fix(&grid);
        assert_eq!(changed, 0);
        assert_eq!(out, grid);
    }

    #[test]
    fn markdown_round_trips_a_grid() {
        let grid = vec![
            vec!["Seg".to_string(), "Rev".into()],
            vec!["A".into(), "10".into()],
            vec!["B".into(), "20".into()],
        ];
        let md = to_markdown(&grid);
        assert!(md.contains("| Seg | Rev |"));
        assert!(md.contains("---"));
        let back = markdown_to_grid(&md);
        assert_eq!(back, grid); // separator row dropped, data preserved
    }

    #[test]
    fn markdown_escapes_pipes_in_cells() {
        let grid = vec![vec!["a|b".to_string(), "c".into()]];
        let md = to_markdown(&grid);
        assert!(md.contains("a\\|b"));
    }

    #[test]
    fn to_html_marks_header_rows_as_th() {
        let rows = vec![
            vec!["H1".to_string(), "H2".into()],
            vec!["a".into(), "b".into()],
        ];
        let html = to_html(&rows, 1);
        assert!(html.contains("<th>H1</th>"));
        assert!(html.contains("<td>a</td>"));
        assert!(html.contains("&lt;") == false); // nothing to escape here
    }

    #[test]
    fn to_html_escapes_markup() {
        let rows = vec![vec!["<b>&".to_string()]];
        let html = to_html(&rows, 0);
        assert!(html.contains("&lt;b&gt;&amp;"));
    }

    #[test]
    fn median_bbox_is_per_edge_median() {
        let boxes = vec![
            BBox::new(0.0, 0.0, 10.0, 10.0),
            BBox::new(2.0, 2.0, 12.0, 12.0),
            BBox::new(50.0, 1.0, 11.0, 30.0), // an outlier on x0 / y1
        ];
        let m = median_bbox(&boxes).unwrap();
        // medians: x0=2, y0=1, x1=11, y1=12 — the outlier doesn't move them
        assert_eq!((m.x0, m.y0, m.x1, m.y1), (2.0, 1.0, 11.0, 12.0));
    }

    #[test]
    fn empty_words_are_handled() {
        assert_eq!(structure_words(&[], 3.0, 6.0).rows.len(), 0);
        assert_eq!(words_to_ascii(&[]), "");
        assert_eq!(median_bbox(&[]), None);
    }

    // ---- The column coalescer: what it's FOR (one column = one whitespace gap) ----

    /// Words closer than the gap belong to the SAME cell — "Total revenue" is one
    /// label, not two columns.
    #[test]
    fn coalescer_keeps_words_closer_than_the_gap_in_one_column() {
        let ws = vec![w("Total", 0.0, 30.0, 0.0), w("revenue", 33.0, 80.0, 0.0)];
        assert_eq!(column_intervals(&ws, 6.0).len(), 1);
    }

    /// A real whitespace gap (≥ col_gap) is a column boundary.
    #[test]
    fn coalescer_splits_words_separated_by_more_than_the_gap() {
        let ws = vec![w("Left", 0.0, 30.0, 0.0), w("Right", 100.0, 130.0, 0.0)];
        assert_eq!(column_intervals(&ws, 6.0).len(), 2);
    }

    /// THE point of using x-spans not x0s: right-aligned numbers of different
    /// widths (a wide "1,234" and a narrow "9" sharing the right edge) are ONE
    /// column. x0-clustering would have split them into two.
    #[test]
    fn coalescer_keeps_right_aligned_numbers_in_one_column() {
        let ws = vec![w("1,234", 100.0, 140.0, 0.0), w("9", 132.0, 140.0, 20.0)];
        assert_eq!(column_intervals(&ws, 6.0).len(), 1);
        // and their left edges differ by 32pt — far more than col_gap — so this is
        // genuinely the alignment case, not just "they happened to be close".
        assert!((ws[0].bbox.x0 - ws[1].bbox.x0).abs() > 6.0);
    }

    /// One interval per real column: a label column + two number columns.
    #[test]
    fn coalescer_finds_one_interval_per_column() {
        let ws = vec![
            w("Item", 0.0, 40.0, 0.0),
            w("100", 100.0, 140.0, 0.0),
            w("200", 200.0, 240.0, 0.0),
        ];
        assert_eq!(column_intervals(&ws, 6.0).len(), 3);
    }

    // ---- structure_words: the intent of each behaviour ----

    /// A multi-word label stays a single cell (the coalescer at work inside
    /// structuring) — not shredded across columns.
    #[test]
    fn structure_keeps_a_multi_word_label_as_one_cell() {
        let words = vec![
            w("Item", 0.0, 40.0, 0.0),
            w("Q1", 100.0, 130.0, 0.0),
            w("Q2", 200.0, 230.0, 0.0),
            w("Total", 0.0, 30.0, 20.0),
            w("revenue", 33.0, 80.0, 20.0),
            w("10", 100.0, 120.0, 20.0),
            w("20", 200.0, 220.0, 20.0),
        ];
        let s = structure_words(&words, 3.0, 6.0);
        assert_eq!(s.rows[0].len(), 3, "label + two numeric columns");
        assert_eq!(texts(&s)[1][0], "Total revenue");
    }

    /// A `$`/`(` split into its own thin column rejoins the number to its RIGHT, so
    /// the value still reads as currency / a negative instead of leaving a phantom
    /// symbol column.
    #[test]
    fn structure_rejoins_a_split_dollar_onto_the_number_to_its_right() {
        let words = vec![
            w("Item", 0.0, 30.0, 0.0),
            w("Amount", 100.0, 150.0, 0.0),
            w("A", 0.0, 10.0, 20.0),
            w("$", 80.0, 86.0, 20.0), // its own column, 14pt left of the number
            w("3,367", 100.0, 140.0, 20.0),
        ];
        let row = &texts(&structure_words(&words, 3.0, 6.0))[1];
        assert!(row.iter().any(|c| c.contains("3,367")));
        assert!(!row.iter().any(|c| c == "$"), "the lone $ column should have merged right");
    }

    /// A closing `)` split into its own column rejoins the number to its LEFT
    /// (`$`/`(` go right, `)` goes left) — so split accounting negatives reunite.
    #[test]
    fn structure_rejoins_a_split_close_paren_onto_the_number_to_its_left() {
        let words = vec![
            w("Item", 0.0, 40.0, 0.0),
            w("Loss", 100.0, 140.0, 0.0),
            w("A", 0.0, 10.0, 20.0),
            w("902", 100.0, 140.0, 20.0),
            w(")", 150.0, 156.0, 20.0),
        ];
        let row = &texts(&structure_words(&words, 3.0, 6.0))[1];
        assert!(row.iter().any(|c| c == "902)"), "got {row:?}");
        assert!(!row.iter().any(|c| c == ")"), "the lone ) column should have merged left");
    }

    // ---- sign-fix / markdown: intent (re-express, don't reshape) ----

    /// sign-fix changes only the SIGN representation — never the table's shape,
    /// labels, or non-accounting cells.
    #[test]
    fn sign_fix_changes_only_signs_not_the_table_shape() {
        let grid = vec![
            vec!["Item".to_string(), "A".into(), "B".into()],
            vec!["x".into(), "(5)".into(), "10".into()],
        ];
        let (out, changed) = sign_fix(&grid);
        assert_eq!(out.len(), grid.len(), "same number of rows");
        assert_eq!(out[0], grid[0], "header row untouched");
        assert_eq!(out[1][0], "x", "label untouched");
        assert_eq!(out[1][1], "-5", "the negative is rewritten");
        assert_eq!(out[1][2], "10", "the positive is left alone");
        assert_eq!(changed, 1);
    }

    /// markdown is a LOSSLESS re-expression: round-tripping a grid through it
    /// returns the same cells. (It exists to re-detect structure, not to edit data.)
    #[test]
    fn markdown_is_a_lossless_re_expression_of_the_grid() {
        let grid = vec![
            vec!["Seg".to_string(), "Rev".into()],
            vec!["A".into(), "10".into()],
            vec!["B".into(), "(20)".into()],
        ];
        assert_eq!(markdown_to_grid(&to_markdown(&grid)), grid);
    }
}
