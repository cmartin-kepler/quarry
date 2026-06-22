//! Column-alignment clustering — a cheap, *decorrelated* tabularity / schema check.
//!
//! Cluster the x-extents of a region's word boxes into vertical bands. The count
//! of bands is an INDEPENDENT statement about whether a `Table` box really has
//! column structure — decorrelated from any parser's own column inference, since
//! it uses only geometry.
//!
//! It is also the one genuinely *additive* detector for the silent schema failure
//! discussed in the plan: a parse whose grid is shape- and type-clean but whose
//! cells landed under the wrong headers. `StructuralValidity` is blind to that
//! (nothing is ragged or mistyped); a disagreement between the parsed column count
//! and the geometric column count surfaces it.

use crate::artifact::Word;

/// Minimum blank x-gap (PDF points) between word extents that separates two
/// columns. The technique's weak knob (see `segment`): a cross-check, not authority.
pub const COLUMN_GUTTER: f32 = 12.0;

/// The vertical column bands `(x0, x1)` of a region's words, left to right —
/// maximal x-ranges separated by gutters ≥ `min_gutter`. Uses a running far-edge
/// so a wide cell that spans a gutter keeps its band joined (conservative: it
/// under-counts rather than inventing columns).
pub fn column_bands(words: &[Word], min_gutter: f32) -> Vec<(f32, f32)> {
    if words.is_empty() {
        return vec![];
    }
    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by(|&a, &b| words[a].bbox.x0.total_cmp(&words[b].bbox.x0));

    let mut bands: Vec<(f32, f32)> = Vec::new();
    let (mut lo, mut far) = (f32::MAX, f32::MIN);
    let mut open = false;
    for &i in &order {
        let (x0, x1) = (words[i].bbox.x0, words[i].bbox.x1);
        if !open {
            lo = x0;
            far = x1;
            open = true;
        } else if x0 <= far + min_gutter {
            far = far.max(x1);
        } else {
            bands.push((lo, far));
            lo = x0;
            far = x1;
        }
    }
    if open {
        bands.push((lo, far));
    }
    bands
}

/// The geometric column count of a region's words.
pub fn column_count(words: &[Word], min_gutter: f32) -> usize {
    column_bands(words, min_gutter).len()
}

/// Schema-coherence check: compare a parse's `claimed_cols` against the geometric
/// column count of its source words. `Some((claimed, geometric))` when they
/// disagree — a candidate silent schema error (mangled columns) that intrinsic,
/// single-grid checks miss. `None` when they agree.
pub fn schema_column_mismatch(
    claimed_cols: usize,
    words: &[Word],
    min_gutter: f32,
) -> Option<(usize, usize)> {
    let geometric = column_count(words, min_gutter);
    (geometric != 0 && geometric != claimed_cols).then_some((claimed_cols, geometric))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::BBox;

    fn w(x0: f32, x1: f32) -> Word {
        // y is irrelevant to column banding; fix it.
        Word { text: "x".into(), bbox: BBox::new(x0, 0.0, x1, 10.0) }
    }

    #[test]
    fn three_clean_columns_count_as_three() {
        // three vertical bands with wide gutters between them
        let words = vec![w(0.0, 20.0), w(0.0, 18.0), w(100.0, 120.0), w(102.0, 118.0), w(200.0, 220.0)];
        assert_eq!(column_count(&words, COLUMN_GUTTER), 3);
    }

    #[test]
    fn words_within_a_gutter_stay_one_column() {
        // touching/closely-spaced words do not invent a column boundary
        let words = vec![w(0.0, 10.0), w(11.0, 20.0), w(21.0, 30.0)];
        assert_eq!(column_count(&words, COLUMN_GUTTER), 1);
    }

    #[test]
    fn mismatch_flags_a_parse_claiming_the_wrong_column_count() {
        // geometry shows two columns; a parse claiming three has mangled the schema
        let words = vec![w(0.0, 20.0), w(100.0, 120.0)];
        assert_eq!(schema_column_mismatch(3, &words, COLUMN_GUTTER), Some((3, 2)));
        assert_eq!(schema_column_mismatch(2, &words, COLUMN_GUTTER), None, "agreement ⇒ no flag");
    }

    #[test]
    fn no_words_does_not_flag() {
        assert_eq!(schema_column_mismatch(4, &[], COLUMN_GUTTER), None);
    }
}
