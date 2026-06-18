//! Demand-driven escalation (the prototype's `route()`): when a `QualityCheck`
//! FLAGS a table, decide the next op to try — a cheap targeted transform before a
//! costly re-parse — apply it, re-check, and keep the result only if it improves.
//! This is what makes parsing *iterative*: better/different parses run on demand,
//! exactly where current artifacts can't pass their checks (brief §1, §2).

use crate::artifact::{Artifact, ArtifactKind, HtmlTable, StoredArtifact};
use crate::check::{CheckCtx, CheckOutcome, QualityCheck};
use crate::core::*;
use crate::doc::QDoc;
use crate::extract::*;
use crate::pipeline::run_document_extractor;
use anyhow::Result;
use std::path::PathBuf;

/// Best-first next ops for a flagged table, given the flags' reasons + the table
/// HTML, minus ops already tried. Cheap targeted fixes first, then re-parse.
pub fn route(flags: &[CheckOutcome], html: &str, tried: &[String]) -> Vec<String> {
    let reasons: String = flags
        .iter()
        .filter_map(|c| match c {
            CheckOutcome::Flag { reason, .. } => Some(reason.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ; ");

    let mut cands: Vec<String> = Vec::new();
    let mut add = |op: &str| {
        let op = op.to_string();
        if !tried.contains(&op) && !cands.contains(&op) {
            cands.push(op);
        }
    };

    // Totals don't add up: if accounting signs are present, a free sign rewrite may
    // fix it; otherwise re-express (re-detect) then re-parse with a real parser.
    if reasons.contains("reconcile") || reasons.contains("sum") {
        if html.contains('(') || html.contains("CR") || html.contains("DR") {
            add("sign-fix");
        }
        add("markdown");
        add("docling");
    }
    // Stray text / ragged / empty in numeric columns: a structure problem — re-parse.
    if reasons.contains("non-numeric") || reasons.contains("ragged") || reasons.contains("empty") {
        add("markdown");
        add("docling");
    }
    // Parse-time signals (OCR / rotation): only a stronger parser helps.
    if reasons.contains("OCR") || reasons.contains("rotated") {
        add("docling");
    }
    // Always leave a way forward.
    add("markdown");
    add("docling");
    cands
}

fn run_checks(a: &dyn Artifact, checks: &[&dyn QualityCheck], ctx: &CheckCtx<'_>) -> Vec<CheckOutcome> {
    checks
        .iter()
        .filter(|c| c.applies_to(a.kind()))
        .map(|c| c.check(a, ctx))
        .collect()
}

fn error_count(outcomes: &[CheckOutcome]) -> usize {
    outcomes
        .iter()
        .filter(|c| matches!(c, CheckOutcome::Flag { severity: Severity::Error, .. }))
        .count()
}

fn accepts_kind(ex: &dyn Extractor, kind: ArtifactKind) -> bool {
    ex.accepts().iter().any(|k| matches!(k, InputKind::Artifact(a) if *a == kind))
}

fn accepts_region(ex: &dyn Extractor) -> bool {
    ex.accepts().iter().any(|k| matches!(k, InputKind::DocumentRegion))
}

fn anchor_box(a: &dyn Artifact) -> Option<BBox> {
    match a.anchor() {
        SourceAnchor::Pdf { bbox, .. } => Some(*bbox),
        _ => None,
    }
}

/// Of a re-parse's tables, the one whose region best matches `target` (IoU).
fn best_overlap(produced: Vec<Box<dyn Artifact>>, target: BBox) -> Option<Box<dyn Artifact>> {
    produced
        .into_iter()
        .filter(|a| a.kind() == ArtifactKind::HtmlTable)
        .map(|a| {
            let iou = anchor_box(a.as_ref()).map(|b| b.iou(&target)).unwrap_or(0.0);
            (iou, a)
        })
        .filter(|(iou, _)| *iou > 0.0)
        .max_by(|x, y| x.0.partial_cmp(&y.0).unwrap())
        .map(|(_, a)| a)
}

fn clone_dyn(a: &dyn Artifact) -> Option<Box<dyn Artifact>> {
    StoredArtifact::from_dyn(a).map(StoredArtifact::into_dyn)
}

/// Escalate every flagged table among `artifacts`: route → apply → re-check, keep
/// the result if it cut the error count, repeat up to `budget` steps. Returns the
/// originals PLUS every escalation attempt (all generations land in the registry;
/// the adjudicator picks the winner). `resolve` looks an op id up to an
/// extractor — injected so production passes `pipeline::extractor_by_id` and tests
/// pass stubs.
pub fn escalate(
    doc: &QDoc,
    doc_hash: DocHash,
    source_path: Option<PathBuf>,
    artifacts: &[Box<dyn Artifact>],
    checks: &[&dyn QualityCheck],
    budget: usize,
    resolve: &dyn Fn(&str) -> Option<Box<dyn Extractor>>,
) -> Result<Vec<Box<dyn Artifact>>> {
    let cctx = CheckCtx { source: doc };
    let mut produced: Vec<Box<dyn Artifact>> = Vec::new();

    for (i, a) in artifacts.iter().enumerate() {
        if a.kind() != ArtifactKind::HtmlTable {
            continue;
        }
        // `cur` is the best table so far for this slot; clone it each step so we
        // never hold a borrow into `produced` while pushing to it.
        let mut cur: Box<dyn Artifact> = match clone_dyn(a.as_ref()) {
            Some(c) => c,
            None => continue,
        };
        let mut tried: Vec<String> = Vec::new();

        for step in 0..budget {
            let flags = run_checks(cur.as_ref(), checks, &cctx);
            let errs = error_count(&flags);
            if errs == 0 {
                break; // clean — nothing to escalate
            }
            let html = cur.as_any().downcast_ref::<HtmlTable>().map(|t| t.html.clone()).unwrap_or_default();
            let cands = route(&flags, &html, &tried);

            let mut advanced = false;
            for cand in cands {
                tried.push(cand.clone());
                let Some(ex) = resolve(&cand) else { continue };
                let generation = Generation((i as u32) * 100 + step as u32 + 1);

                // A candidate that errors (e.g. its sidecar tool isn't installed)
                // is skipped, not fatal — escalation just tries the next candidate.
                let attempt: Option<Box<dyn Artifact>> = if accepts_kind(ex.as_ref(), ArtifactKind::HtmlTable) {
                    // transform: run it on the current table
                    let input = [cur.as_ref()];
                    let ctx = ExtractCtx { source: doc, generation, source_path: source_path.clone() };
                    ex.extract(ExtractInput::Artifacts(&input), &ctx).ok().and_then(|v| v.into_iter().next())
                } else if accepts_region(ex.as_ref()) && source_path.is_some() {
                    // re-parse the document and match the table over the same region
                    let target = anchor_box(cur.as_ref());
                    match run_document_extractor(doc, doc_hash, source_path.clone(), generation, ex.as_ref()) {
                        Ok(reparsed) => target.and_then(|t| best_overlap(reparsed, t)),
                        Err(_) => None,
                    }
                } else {
                    None
                };

                let Some(nt) = attempt else { continue };
                let nflags = run_checks(nt.as_ref(), checks, &cctx);
                let better = error_count(&nflags) < errs;
                // keep the attempt for the registry either way
                if let Some(c) = clone_dyn(nt.as_ref()) {
                    produced.push(c);
                }
                if better {
                    cur = nt;
                    advanced = true;
                    break; // re-route from the improved table
                }
            }
            if !advanced {
                break; // no candidate improved — stop escalating this table
            }
        }
    }
    Ok(produced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Cell, Meta};
    use crate::check::{IntrinsicArithmetic, StructuralValidity};
    use crate::doc::DocFormat;

    fn table(id: &str, cells: Vec<Cell>, n_rows: u32, n_cols: u32) -> HtmlTable {
        let dh = DocHash::of(id.as_bytes());
        HtmlTable {
            meta: Meta {
                id: ArtifactId(id.into()),
                content_hash: dh,
                provenance: Provenance::Source(SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 10.0, 10.0) }),
                generation: Generation(0),
                risk: RiskMarkers::default(),
            },
            n_rows,
            n_cols,
            cells,
            html: format!("<table id={id}>(902)</table>"),
        }
    }
    fn cell(r: u32, c: u32, t: &str, h: bool) -> Cell {
        Cell { row: r, col: c, text: t.into(), anchor: SourceAnchor::Pdf { doc: DocHash::of(b"d"), page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) }, is_header: h }
    }

    #[test]
    fn route_prefers_sign_fix_when_totals_fail_with_parens() {
        let flags = vec![CheckOutcome::Flag { reason: "arithmetic does not reconcile — rows sum to 5".into(), severity: Severity::Error }];
        let cands = route(&flags, "<table>(902)</table>", &[]);
        assert_eq!(cands.first().map(String::as_str), Some("sign-fix"));
        assert!(cands.contains(&"docling".to_string()));
        // already-tried ops drop out
        let cands2 = route(&flags, "<table>(902)</table>", &["sign-fix".into()]);
        assert!(!cands2.contains(&"sign-fix".to_string()));
    }

    #[test]
    fn route_goes_straight_to_reparse_for_structural_damage() {
        let flags = vec![CheckOutcome::Flag { reason: "2 non-numeric cell(s) in numeric column(s)".into(), severity: Severity::Error }];
        let cands = route(&flags, "<table></table>", &[]);
        assert!(!cands.contains(&"sign-fix".to_string()), "sign-fix can't fix a shifted cell");
        assert!(cands.contains(&"markdown".to_string()) && cands.contains(&"docling".to_string()));
    }

    /// A stub re-parser that returns one clean, reconciling table — stands in for
    /// Docling so the escalation loop is tested without a real tool.
    struct CleanReparser;
    impl Extractor for CleanReparser {
        fn id(&self) -> ExtractorId { ExtractorId("docling".into()) }
        fn version(&self) -> Version { Version(1) }
        fn cost_tier(&self) -> CostTier { CostTier(2) }
        fn op_kind(&self) -> OpKind { OpKind::Extract }
        fn accepts(&self) -> &[InputKind] { &[InputKind::DocumentRegion] }
        fn produces(&self) -> ArtifactKind { ArtifactKind::HtmlTable }
        fn extract(&self, _i: ExtractInput<'_>, _ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
            // a clean 3-row table: 100 + 220 == 320, no structural issues
            let cells = vec![
                cell(0, 0, "Seg", true), cell(0, 1, "Rev", true),
                cell(1, 0, "A", false), cell(1, 1, "100", false),
                cell(2, 0, "Total", false), cell(2, 1, "100", false),
            ];
            Ok(vec![Box::new(table("clean", cells, 3, 2))])
        }
    }

    #[test]
    fn escalate_reparses_a_flagged_table_and_keeps_the_clean_result() {
        // a flagged table: a non-numeric cell sitting in a numeric column
        let bad = table(
            "bad",
            vec![
                cell(0, 0, "Seg", true), cell(0, 1, "Rev", true),
                cell(1, 0, "A", false), cell(1, 1, "oops", false), // stray text in numeric col
                cell(2, 0, "B", false), cell(2, 1, "10", false),
            ],
            3,
            2,
        );
        let arts: Vec<Box<dyn Artifact>> = vec![Box::new(bad)];
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let arith = IntrinsicArithmetic::default();
        let structural = StructuralValidity;
        let checks: Vec<&dyn QualityCheck> = vec![&arith, &structural];

        let resolve = |id: &str| -> Option<Box<dyn Extractor>> {
            (id == "docling").then(|| Box::new(CleanReparser) as Box<dyn Extractor>)
        };
        let out = escalate(
            &doc,
            DocHash::of(b"d"),
            Some(PathBuf::from("x.pdf")), // present so the re-parse branch runs
            &arts,
            &checks,
            4,
            &resolve,
        )
        .unwrap();
        // escalation produced the clean re-parse
        assert!(!out.is_empty(), "escalation should produce an attempt");
        let clean = out.iter().find(|a| a.id() == ArtifactId("clean".into())).expect("the re-parse");
        let c = CheckCtx { source: &doc };
        assert!(!structural.check(clean.as_ref(), &c).is_flag(), "the kept result is clean");
    }

    #[test]
    fn escalate_leaves_a_clean_table_untouched() {
        let good = table(
            "good",
            vec![
                cell(0, 0, "Seg", true), cell(0, 1, "Rev", true),
                cell(1, 0, "A", false), cell(1, 1, "100", false),
                cell(2, 0, "Total", false), cell(2, 1, "100", false),
            ],
            3,
            2,
        );
        let arts: Vec<Box<dyn Artifact>> = vec![Box::new(good)];
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let arith = IntrinsicArithmetic::default();
        let structural = StructuralValidity;
        let checks: Vec<&dyn QualityCheck> = vec![&arith, &structural];
        let out = escalate(&doc, DocHash::of(b"d"), None, &arts, &checks, 4, &|_| None).unwrap();
        assert!(out.is_empty(), "a clean table triggers no escalation");
    }
}
