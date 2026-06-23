//! Whitespace / projection segmentation (recursive XY-cut) — the cheap,
//! model-free, *decorrelated* region source for build-plan B′ (§1a's independent
//! opinion).
//!
//! It keys ONLY on the gaps between word boxes, so its bias is the opposite of a
//! learned detector's: it over-*splits* on whitespace where YOLO over-*merges* on
//! semantic priors. That opposite bias is the entire value — agreement between the
//! two is meaningful, disagreement is informative. Deterministic, pure geometry,
//! no model. The gap thresholds are the technique's known weak knob (too small
//! over-splits, too large merges), which is exactly why it is a *cross-check that
//! flags disagreement*, not the authority that draws final boxes.

use crate::artifact::Word;
use crate::core::BBox;

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

/// Minimum whitespace gap (PDF points) counting as a cut on each axis.
pub struct CutParams {
    pub min_x_gap: f32,
    pub min_y_gap: f32,
}

impl Default for CutParams {
    fn default() -> Self {
        CutParams { min_x_gap: 12.0, min_y_gap: 8.0 }
    }
}

fn lo(b: &BBox, axis: Axis) -> f32 {
    match axis {
        Axis::X => b.x0,
        Axis::Y => b.y0,
    }
}
fn hi(b: &BBox, axis: Axis) -> f32 {
    match axis {
        Axis::X => b.x1,
        Axis::Y => b.y1,
    }
}

/// Split a group into bands separated by blank gaps ≥ `min_gap` along `axis`.
/// Uses the running max far-edge, so nested/overlapping words stay in one band.
fn bands(words: &[Word], axis: Axis, min_gap: f32) -> Vec<Vec<Word>> {
    if words.is_empty() {
        return vec![];
    }
    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by(|&a, &b| lo(&words[a].bbox, axis).total_cmp(&lo(&words[b].bbox, axis)));

    let mut out: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    let mut cur_far = f32::MIN;
    for &i in &order {
        let (l, h) = (lo(&words[i].bbox, axis), hi(&words[i].bbox, axis));
        if cur.is_empty() || l <= cur_far + min_gap {
            cur.push(words[i].clone());
            cur_far = cur_far.max(h);
        } else {
            out.push(std::mem::take(&mut cur));
            cur.push(words[i].clone());
            cur_far = h;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn union_bbox(words: &[Word]) -> BBox {
    let mut iter = words.iter();
    let mut b = iter.next().expect("non-empty leaf").bbox;
    for w in iter {
        b = b.union(&w.bbox);
    }
    b
}

/// Recursively cut: split on whichever axis has a qualifying gap (preferring the
/// axis that yields more bands), recurse into each band, emit a leaf bbox when
/// neither axis splits. Terminates because every split strictly shrinks the group.
fn cut(words: Vec<Word>, p: &CutParams, out: &mut Vec<BBox>) {
    if words.is_empty() {
        return;
    }
    let y_bands = bands(&words, Axis::Y, p.min_y_gap);
    let x_bands = bands(&words, Axis::X, p.min_x_gap);
    if y_bands.len() > 1 && y_bands.len() >= x_bands.len() {
        for b in y_bands {
            cut(b, p, out);
        }
    } else if x_bands.len() > 1 {
        for b in x_bands {
            cut(b, p, out);
        }
    } else {
        out.push(union_bbox(&words));
    }
}

/// Segment a page's spans into block bboxes by recursive XY-cut — the independent
/// region opinion compared against YOLO for B′'s agreement bar.
pub fn xy_cut(spans: &[Word], p: &CutParams) -> Vec<BBox> {
    let mut out = Vec::new();
    cut(spans.to_vec(), p, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(x0: f32, y0: f32, x1: f32, y1: f32) -> Word {
        Word { text: "x".into(), bbox: BBox::new(x0, y0, x1, y1) }
    }

    #[test]
    fn two_stacked_blocks_split_on_the_vertical_gap() {
        let spans = vec![
            w(0.0, 0.0, 10.0, 10.0),
            w(12.0, 0.0, 22.0, 10.0),
            w(0.0, 100.0, 10.0, 110.0), // tall blank band above this row
            w(12.0, 100.0, 22.0, 110.0),
        ];
        assert_eq!(xy_cut(&spans, &CutParams::default()).len(), 2);
    }

    #[test]
    fn two_columns_split_on_the_horizontal_gutter() {
        let spans = vec![
            w(0.0, 0.0, 10.0, 10.0),
            w(0.0, 12.0, 10.0, 22.0),
            w(100.0, 0.0, 110.0, 10.0), // wide gutter to the right column
            w(100.0, 12.0, 110.0, 22.0),
        ];
        assert_eq!(xy_cut(&spans, &CutParams::default()).len(), 2);
    }

    #[test]
    fn a_tight_cluster_is_a_single_block() {
        let spans = vec![
            w(0.0, 0.0, 10.0, 10.0),
            w(11.0, 0.0, 20.0, 10.0),
            w(0.0, 9.0, 10.0, 18.0),
        ];
        assert_eq!(xy_cut(&spans, &CutParams::default()).len(), 1);
    }

    #[test]
    fn over_splitting_bias_a_wide_intra_block_gap_cuts() {
        // The documented opposite-bias failure: a single logical block with a wide
        // internal gutter gets over-split. This is the *feature* for a cross-check
        // (it disagrees with YOLO's merge), not a bug to suppress.
        let spans = vec![w(0.0, 0.0, 10.0, 10.0), w(200.0, 0.0, 210.0, 10.0)];
        assert_eq!(xy_cut(&spans, &CutParams::default()).len(), 2);
    }
}
