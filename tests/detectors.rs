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
fn structural_flags_ragged_and_empty_grids() {
    let risk = RiskMarkers {
        merged_cell_rows: 2,
        empty_cells: 3,
        ..Default::default()
    };
    let table = table_from(&[&["a", "b"], &["1", "2"]], risk);
    let check = StructuralValidity;
    let doc = empty_doc();
    let ctx = CheckCtx { source: &doc };
    assert!(check.check(&table, &ctx).is_flag());
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
