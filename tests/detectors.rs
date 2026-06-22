//! Integration tests for the three detectors against hand-built artifacts —
//! proving each fires on the failure mode it owns, independent of the fixture
//! end-to-end run.

use quarry::artifact::*;
use quarry::check::*;
use quarry::core::*;
use quarry::doc::{DocFormat, QDoc};

fn anchor(page: u32) -> SourceAnchor {
    SourceAnchor::Pdf {
        doc: DocHash::of(b"test"),
        page,
        bbox: BBox::new(0.0, 0.0, 100.0, 100.0),
    }
}

fn table_from(grid: &[&[&str]], risk: RiskMarkers) -> HtmlTable {
    let n_rows = grid.len() as u32;
    let n_cols = grid.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let mut cells = Vec::new();
    for (r, row) in grid.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            cells.push(Cell {
                row: r as u32,
                col: c as u32,
                text: (*text).to_string(),
                anchor: anchor(1),
                is_header: r == 0,
            });
        }
    }
    let content = DocHash::of(format!("{grid:?}").as_bytes());
    HtmlTable {
        meta: Meta {
            id: ArtifactId::mint(&content, Generation(0)),
            content_hash: content,
            provenance: Provenance::Source(anchor(1)),
            generation: Generation(0),
            risk,
            origin: Origin::default(),
        },
        n_rows,
        n_cols,
        cells,
        html: String::new(),
    }
}

fn empty_doc() -> QDoc {
    QDoc {
        format: DocFormat::Pdf,
        pages: vec![],
    }
}

#[test]
fn arithmetic_flags_a_sum_that_does_not_reconcile() {
    let table = table_from(
        &[
            &["Item", "2023"],
            &["Cash", "100"],
            &["Debt", "200"],
            &["Total", "250"], // 100 + 200 = 300, not 250
        ],
        RiskMarkers::default(),
    );
    let check = IntrinsicArithmetic::default();
    let doc = empty_doc();
    let ctx = CheckCtx { source: &doc };
    assert!(
        check.check(&table, &ctx).is_flag(),
        "should flag the 300 vs 250 mismatch"
    );
}

#[test]
fn arithmetic_passes_a_table_that_reconciles() {
    let table = table_from(
        &[
            &["Item", "2023"],
            &["Cash", "100"],
            &["Debt", "200"],
            &["Total", "300"],
        ],
        RiskMarkers::default(),
    );
    let check = IntrinsicArithmetic::default();
    let doc = empty_doc();
    let ctx = CheckCtx { source: &doc };
    assert!(!check.check(&table, &ctx).is_flag());
}

#[test]
fn structural_flags_stray_text_in_numeric_column() {
    // "oops" sitting in an otherwise-numeric column => a shifted/merged cell.
    let risk = RiskMarkers { min_ocr_confidence: 1.0, ..Default::default() };
    let table = table_from(
        &[&["Item", "2024"], &["Cash", "100"], &["Debt", "oops"], &["Misc", "300"]],
        risk,
    );
    let check = StructuralValidity;
    let doc = empty_doc();
    let ctx = CheckCtx { source: &doc };
    assert!(check.check(&table, &ctx).is_flag());
}

#[test]
fn structural_passes_a_clean_table() {
    let risk = RiskMarkers { min_ocr_confidence: 1.0, ..Default::default() };
    let table = table_from(
        &[&["Item", "2024"], &["Cash", "100"], &["Debt", "200"]],
        risk,
    );
    let check = StructuralValidity;
    let doc = empty_doc();
    let ctx = CheckCtx { source: &doc };
    assert!(!check.check(&table, &ctx).is_flag());
}

fn doc_with_spans(spans_json: &str) -> QDoc {
    serde_json::from_str(&format!(
        r#"{{"format":"pdf","pages":[{{"page":1,"width":600,"height":800,"spans":{spans_json}}}]}}"#
    ))
    .unwrap()
}

// Six source words filling the (0,0,100,100) region every `table_from` cell anchors
// to — the ground truth the reconstruction detector reprojects onto.
const SRC_WORDS: &str = r#"[
  {"text":"Item","bbox":[1,1,20,8]},  {"text":"2023","bbox":[40,1,60,8]},
  {"text":"Cash","bbox":[1,20,20,28]},{"text":"100","bbox":[40,20,60,28]},
  {"text":"Debt","bbox":[1,40,20,48]},{"text":"200","bbox":[40,40,60,48]}
]"#;

#[test]
fn reconstruction_passes_when_the_parse_covers_the_source_words() {
    let table = table_from(
        &[&["Item", "2023"], &["Cash", "100"], &["Debt", "200"]],
        RiskMarkers::default(),
    );
    let doc = doc_with_spans(SRC_WORDS);
    let ctx = CheckCtx { source: &doc };
    assert!(!ReconstructionError::default().check(&table, &ctx).is_flag());
}

#[test]
fn reconstruction_flags_a_word_dropped_from_the_parse() {
    // The source region holds "Debt 200" but the parse never captured that row:
    // internally consistent, no total to contradict — silent to the other two
    // detectors, but it leaves a reconstruction residual.
    let table = table_from(&[&["Item", "2023"], &["Cash", "100"]], RiskMarkers::default());
    let doc = doc_with_spans(SRC_WORDS);
    let ctx = CheckCtx { source: &doc };
    assert!(
        ReconstructionError::default().check(&table, &ctx).is_flag(),
        "dropped 'Debt 200' should leave a residual"
    );
}

#[test]
fn reconstruction_is_blind_to_a_pure_value_swap() {
    // Cash/Debt values swapped: every token is still present, so the residual is
    // zero. Documents the honest blind spot the vision/human rungs exist to cover.
    let table = table_from(
        &[&["Item", "2023"], &["Cash", "200"], &["Debt", "100"]],
        RiskMarkers::default(),
    );
    let doc = doc_with_spans(SRC_WORDS);
    let ctx = CheckCtx { source: &doc };
    assert!(!ReconstructionError::default().check(&table, &ctx).is_flag());
}

#[test]
fn reconstruction_is_a_noop_without_a_backing_source() {
    // A hand-built grid with no source words must not false-alarm.
    let table = table_from(&[&["Item", "2023"], &["Cash", "100"]], RiskMarkers::default());
    let doc = empty_doc();
    let ctx = CheckCtx { source: &doc };
    assert!(!ReconstructionError::default().check(&table, &ctx).is_flag());
}

// Build a table whose cells carry DISTINCT lattice bboxes (offset in x by `dx`),
// so the geometry-keyed comparator has real positions to match on.
fn geo_table(grid: &[&[&str]], dx: f32) -> HtmlTable {
    let n_rows = grid.len() as u32;
    let n_cols = grid.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let doc = DocHash::of(b"geo");
    let mut cells = Vec::new();
    for (r, row) in grid.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            let (x0, y0) = (dx + c as f32 * 20.0, r as f32 * 10.0);
            cells.push(Cell {
                row: r as u32,
                col: c as u32,
                text: (*text).to_string(),
                anchor: SourceAnchor::Pdf { doc, page: 1, bbox: BBox::new(x0, y0, x0 + 18.0, y0 + 8.0) },
                is_header: r == 0,
            });
        }
    }
    let content = DocHash::of(format!("{grid:?}{dx}").as_bytes());
    HtmlTable {
        meta: Meta {
            id: ArtifactId::mint(&content, Generation(0)),
            content_hash: content,
            provenance: Provenance::Source(SourceAnchor::Pdf { doc, page: 1, bbox: BBox::new(dx, 0.0, dx + 200.0, 200.0) }),
            generation: Generation(0),
            risk: RiskMarkers::default(),
            origin: Origin::default(),
        },
        n_rows,
        n_cols,
        cells,
        html: String::new(),
    }
}

#[test]
fn cross_tier_agrees_when_both_parses_match() {
    let a = geo_table(&[&["Segment", "Revenue"], &["Parks", "100"], &["Studios", "220"]], 0.0);
    let b = geo_table(&[&["Segment", "Revenue"], &["Parks", "100"], &["Studios", "220"]], 0.0);
    assert!(!cross_tier_agreement(&a, &b).is_flag());
}

#[test]
fn cross_tier_flags_a_value_swap_an_intrinsic_check_cannot_see() {
    // tier-A swapped Parks/Studios values; the column total still sums, so
    // arithmetic and structure both pass — only the independent tier-B disagrees,
    // at the same physical cell positions.
    let a = geo_table(&[&["Segment", "Revenue"], &["Parks", "220"], &["Studios", "100"]], 0.0);
    let b = geo_table(&[&["Segment", "Revenue"], &["Parks", "100"], &["Studios", "220"]], 0.0);
    assert!(cross_tier_agreement(&a, &b).is_flag(), "Parks/Studios swap should disagree across tiers");
}

#[test]
fn cross_tier_flags_a_column_transpose() {
    let a = geo_table(&[&["Segment", "Revenue", "Income"], &["Parks", "20", "100"], &["Studios", "44", "220"]], 0.0);
    let b = geo_table(&[&["Segment", "Revenue", "Income"], &["Parks", "100", "20"], &["Studios", "220", "44"]], 0.0);
    assert!(cross_tier_agreement(&a, &b).is_flag());
}

#[test]
fn cross_tier_tolerates_label_wrapping_but_flags_a_real_number_change() {
    // Same table, tier-B wraps a label and re-formats a number — must NOT flag;
    // but a genuinely different value at the same cell must.
    let a = geo_table(&[&["Segment", "Revenue"], &["North America", "1,234"]], 0.0);
    let b_ok = geo_table(&[&["Segment", "Revenue"], &["North America (a)", "$1,234"]], 0.0);
    let b_bad = geo_table(&[&["Segment", "Revenue"], &["North America", "1,284"]], 0.0);
    assert!(!cross_tier_agreement(&a, &b_ok).is_flag(), "wrapping + $ formatting should agree");
    assert!(cross_tier_agreement(&a, &b_bad).is_flag(), "1,234 vs 1,284 should disagree");
}

#[test]
fn cross_tier_cannot_judge_without_geometric_overlap() {
    // The second parse sits in a disjoint region (no cell overlaps) => "could not
    // judge", a low-confidence Pass, never a false confirmation.
    let a = geo_table(&[&["Segment", "Revenue"], &["Parks", "100"]], 0.0);
    let b = geo_table(&[&["Segment", "Revenue"], &["Parks", "100"]], 1000.0);
    let out = cross_tier_agreement(&a, &b);
    assert!(!out.is_flag());
    assert!(matches!(out, CheckOutcome::Pass { confidence } if confidence < 0.5));
}

#[test]
fn answer_support_flags_a_claim_absent_from_the_cited_crop() {
    // Source has "1,250" at this location; agent claims "9,999".
    let doc: QDoc = serde_json::from_str(
        r#"{
            "format": "pdf",
            "pages": [{
                "page": 1, "width": 600, "height": 800,
                "spans": [{ "text": "1,250", "bbox": [270, 120, 305, 130] }]
            }]
        }"#,
    )
    .unwrap();

    let verifier = SourceCropVerifier;
    let cited = SourceAnchor::Pdf {
        doc: DocHash::of(b"x"),
        page: 1,
        bbox: BBox::new(260.0, 115.0, 310.0, 135.0),
    };

    let good = Claim {
        element: ArtifactId("e".into()),
        anchor: cited.clone(),
        asserted: "1,250".into(),
    };
    let bad = Claim {
        element: ArtifactId("e".into()),
        anchor: cited,
        asserted: "9,999".into(),
    };

    assert!(matches!(
        verifier.verify(&good, &doc),
        SupportOutcome::Supported { .. }
    ));
    assert!(matches!(
        verifier.verify(&bad, &doc),
        SupportOutcome::Unsupported { .. }
    ));
}
