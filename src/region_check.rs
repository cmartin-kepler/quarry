//! Region-quality checks — build-plan Step B′, invariant 8.
//!
//! Pure geometry over a page's regions and its text spans, run BEFORE trusting
//! layout. YOLO is the dominant failure surface, and the downstream table
//! cross-check runs *inside* a region, so it cannot see a bad box — these are the
//! independent look at the box itself.
//!
//! What is a GATE vs a DIAGNOSTIC (build-plan B′):
//! - **gate:** `overlapping_table_pairs` empty at the 0.1 IoU threshold (no two
//!   distinct table boxes overlap — a duplicate or merged region). The other gate
//!   bar, ≥90% agreement with an *independent* region source, needs the
//!   whitespace/projection segmenter and lands next.
//! - **diagnostic (NOT a gate):** `typed_orphans`. The page-text catch-all covers
//!   every span by construction, so a "zero orphans" bar is vacuous, and page
//!   furniture (headers/footers/page numbers) is legitimately un-regioned. Eyeball
//!   the orphans; a *body-content* orphan means YOLO missed a box.

use crate::artifact::{Region, RegionRole, Word};
use crate::core::BBox;

/// IoU above which two distinct regions overlap enough to signal a mis-drawn
/// boundary (a duplicate or merged box). Build-plan B′ gate threshold.
pub const REGION_OVERLAP_IOU: f32 = 0.1;

/// B′ agreement-bar constants: a YOLO region "agrees" with the independent source
/// at IoU ≥ 0.7, and the bar passes when ≥ 90% of regions agree.
pub const AGREEMENT_IOU: f32 = 0.7;
pub const AGREEMENT_MIN_FRAC: f32 = 0.9;

/// Whether a span's center falls inside this region's box.
fn covers(r: &Region, w: &Word) -> bool {
    r.bbox().contains_center(&w.bbox)
}

/// Coverage DIAGNOSTIC (not a gate): indices of spans inside no *typed* region
/// (`Table`/`Text`/`Caption`/`Figure`). They should be page furniture; a
/// body-content orphan means YOLO missed a box. Ink inside a `Figure` is expected
/// (invariant 11 — a known image, not a gap), so figures count as covering.
pub fn typed_orphans(regions: &[Region], spans: &[Word]) -> Vec<usize> {
    let typed: Vec<&Region> = regions
        .iter()
        .filter(|r| !matches!(r.role(), RegionRole::Other))
        .collect();
    spans
        .iter()
        .enumerate()
        .filter(|(_, w)| !typed.iter().any(|r| covers(r, w)))
        .map(|(i, _)| i)
        .collect()
}

/// Overlap GATE bar (build-plan B′): pairs of distinct `Table` regions whose IoU
/// exceeds [`REGION_OVERLAP_IOU`] — a duplicate or merged box. Empty ⇒ bar passes.
/// Returns `(i, j, iou)` indices into `regions` for any offending pair.
pub fn overlapping_table_pairs(regions: &[Region]) -> Vec<(usize, usize, f32)> {
    let tables: Vec<(usize, &Region)> = regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.role() == RegionRole::Table)
        .collect();
    let mut hits = Vec::new();
    for a in 0..tables.len() {
        for b in (a + 1)..tables.len() {
            let iou = tables[a].1.bbox().iou(&tables[b].1.bbox());
            if iou > REGION_OVERLAP_IOU {
                hits.push((tables[a].0, tables[b].0, iou));
            }
        }
    }
    hits
}

/// The B′ overlap gate: true iff no two distinct table regions overlap above
/// threshold. (The full B′ gate is this AND the independent-source agreement bar.)
pub fn passes_overlap_bar(regions: &[Region]) -> bool {
    overlapping_table_pairs(regions).is_empty()
}

/// B′ agreement metric: the fraction of `yolo` region boxes that have a matching
/// block (IoU ≥ `iou`) in the `independent` source (e.g. the XY-cut segmenter).
/// Decorrelated by construction — the independent source fails *differently*, so
/// agreement is meaningful. A region with no match is flagged for review.
pub fn boundary_agreement(yolo: &[BBox], independent: &[BBox], iou: f32) -> f32 {
    if yolo.is_empty() {
        return 1.0;
    }
    let matched = yolo
        .iter()
        .filter(|y| independent.iter().any(|b| y.iou(b) >= iou))
        .count();
    matched as f32 / yolo.len() as f32
}

/// The B′ agreement gate bar: ≥ [`AGREEMENT_MIN_FRAC`] of YOLO regions agree
/// (IoU ≥ [`AGREEMENT_IOU`]) with the independent region source.
pub fn passes_agreement_bar(yolo: &[BBox], independent: &[BBox]) -> bool {
    boundary_agreement(yolo, independent, AGREEMENT_IOU) >= AGREEMENT_MIN_FRAC
}

/// Indices of `yolo` regions with NO independent match at IoU ≥ [`AGREEMENT_IOU`]
/// — the regions to flag for escalation/review (the disagreement set).
pub fn disagreeing_regions(yolo: &[BBox], independent: &[BBox]) -> Vec<usize> {
    yolo.iter()
        .enumerate()
        .filter(|(_, y)| !independent.iter().any(|b| y.iou(b) >= AGREEMENT_IOU))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Meta;
    use crate::core::*;

    fn region(label: &str, bbox: BBox) -> Region {
        let dh = DocHash::of(label.as_bytes());
        Region {
            meta: Meta {
                id: ArtifactId::mint(&dh, Generation(0)),
                content_hash: dh,
                provenance: Provenance::Source(SourceAnchor::Pdf { doc: dh, page: 1, bbox }),
                generation: Generation(0),
                risk: RiskMarkers::default(),
                origin: Origin::default(),
            },
            label: label.into(),
            confidence: 1.0,
        }
    }

    fn word(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Word {
        Word { text: text.into(), bbox: BBox::new(x0, y0, x1, y1) }
    }

    #[test]
    fn typed_orphans_flags_only_uncovered_body_spans() {
        let regions = vec![
            region("Table", BBox::new(0.0, 0.0, 100.0, 100.0)),
            region("Figure", BBox::new(0.0, 200.0, 100.0, 300.0)),
        ];
        let spans = vec![
            word("in-table", 10.0, 10.0, 20.0, 20.0),  // covered by Table
            word("in-figure", 10.0, 210.0, 20.0, 220.0), // covered by Figure (expected, invariant 11)
            word("footer", 10.0, 500.0, 20.0, 510.0),  // outside every region → orphan
        ];
        let orphans = typed_orphans(&regions, &spans);
        assert_eq!(orphans, vec![2], "only the uncovered span is an orphan; figure ink is not");
    }

    #[test]
    fn other_role_regions_do_not_count_as_coverage() {
        // An unmapped ("Other") region is recorded but is NOT a typed region, so a
        // span only inside it is still an orphan (no silent coverage by junk boxes).
        let regions = vec![region("doodle", BBox::new(0.0, 0.0, 100.0, 100.0))];
        let spans = vec![word("x", 10.0, 10.0, 20.0, 20.0)];
        assert_eq!(typed_orphans(&regions, &spans), vec![0]);
    }

    #[test]
    fn overlapping_table_pairs_flags_a_merged_or_duplicate_box() {
        let regions = vec![
            region("Table", BBox::new(0.0, 0.0, 100.0, 100.0)),
            region("Table", BBox::new(10.0, 10.0, 110.0, 110.0)), // heavy overlap
            region("Table", BBox::new(500.0, 500.0, 600.0, 600.0)), // disjoint
        ];
        let pairs = overlapping_table_pairs(&regions);
        assert_eq!(pairs.len(), 1, "only the overlapping pair");
        assert_eq!((pairs[0].0, pairs[0].1), (0, 1));
        assert!(!passes_overlap_bar(&regions));
    }

    #[test]
    fn overlap_bar_ignores_table_over_figure() {
        // A table legitimately overlapping a figure (e.g. an inset) is not a
        // table-table collision — the overlap bar is about distinct *table* boxes.
        let regions = vec![
            region("Table", BBox::new(0.0, 0.0, 100.0, 100.0)),
            region("Figure", BBox::new(10.0, 10.0, 90.0, 90.0)),
        ];
        assert!(passes_overlap_bar(&regions), "table-vs-figure overlap is not a gate failure");
    }

    #[test]
    fn disjoint_tables_pass_the_overlap_bar() {
        let regions = vec![
            region("Table", BBox::new(0.0, 0.0, 100.0, 100.0)),
            region("Table", BBox::new(0.0, 200.0, 100.0, 300.0)),
        ];
        assert!(passes_overlap_bar(&regions));
    }

    #[test]
    fn agreement_is_full_when_the_independent_source_matches() {
        let yolo = vec![BBox::new(0.0, 0.0, 100.0, 100.0), BBox::new(0.0, 200.0, 100.0, 300.0)];
        // independent boxes nudged slightly but well above the 0.7 IoU bar
        let indep = vec![BBox::new(1.0, 1.0, 101.0, 99.0), BBox::new(0.0, 201.0, 100.0, 299.0)];
        assert_eq!(boundary_agreement(&yolo, &indep, AGREEMENT_IOU), 1.0);
        assert!(passes_agreement_bar(&yolo, &indep));
        assert!(disagreeing_regions(&yolo, &indep).is_empty());
    }

    #[test]
    fn agreement_drops_and_flags_when_a_region_is_unmatched() {
        // One YOLO box has no independent counterpart (the dangerous merged/clipped
        // box the cross-check exists to catch) ⇒ below bar, flagged for review.
        let yolo = vec![BBox::new(0.0, 0.0, 100.0, 100.0), BBox::new(0.0, 200.0, 100.0, 300.0)];
        let indep = vec![BBox::new(1.0, 1.0, 101.0, 99.0)]; // second region missing
        assert_eq!(boundary_agreement(&yolo, &indep, AGREEMENT_IOU), 0.5);
        assert!(!passes_agreement_bar(&yolo, &indep));
        assert_eq!(disagreeing_regions(&yolo, &indep), vec![1]);
    }
}
