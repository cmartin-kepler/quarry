//! Glue: pick the right format-specific extractor for a tier and run it over the
//! whole document. In Phase 0 there's exactly one PDF extractor (tier 0); higher
//! tiers and PPTX/XLSX are deferred (brief §6) and surface as clear errors.

use crate::artifact::Artifact;
use crate::core::*;
use crate::doc::{DocFormat, QDoc};
use crate::extract::*;
use anyhow::{Result, bail};
use std::path::PathBuf;

/// Resolve any op by id — native ops and sidecars (with their default commands).
/// The single registry the CLI / orchestration look ops up in.
pub fn extractor_by_id(id: &str) -> Option<Box<dyn Extractor>> {
    use crate::sidecar::{DoclingSidecar, LayoutSidecar, LiteParseSidecar, TableSidecar};
    match id {
        "pdf-text" => Some(Box::new(PdfTextLayerReconstructor)),
        "docling" => Some(Box::new(DoclingSidecar::default_cmd())),
        "text-table" => Some(Box::new(LiteParseSidecar::default_cmd())),
        "yolo26n" | "yolo26s" | "yolo26m" | "doclayout" | "surya" => {
            Some(Box::new(LayoutSidecar::model(id)))
        }
        "reducto" | "llamaparse" => Some(Box::new(TableSidecar::parser(id))),
        // structure / sign-fix / markdown / merge / regions (artifact- or page-level)
        other => crate::ops::op_by_id(other),
    }
}

/// Run a document-region extractor (native or sidecar) over the whole document:
/// once per page for the .qdoc fixtures, once for a sidecar on a real file (empty
/// QDoc context). Artifact-consuming ops are rejected — those belong in an op
/// chain (orchestration, deferred).
pub fn run_document_extractor(
    doc: &QDoc,
    doc_hash: DocHash,
    source_path: Option<PathBuf>,
    generation: Generation,
    ex: &dyn Extractor,
) -> Result<Vec<Box<dyn Artifact>>> {
    if !ex.accepts().iter().any(|k| matches!(k, InputKind::DocumentRegion)) {
        bail!(
            "op `{}` consumes artifacts, not a document region — run it inside an op chain",
            ex.id().0
        );
    }
    let ctx = ExtractCtx { source: doc, generation, source_path };
    let pages: Vec<(u32, f32, f32)> = if doc.pages.is_empty() {
        vec![(1, 612.0, 792.0)]
    } else {
        doc.pages.iter().map(|p| (p.page, p.width, p.height)).collect()
    };
    let mut out = Vec::new();
    for (page, w, h) in pages {
        let anchor = SourceAnchor::Pdf { doc: doc_hash, page, bbox: BBox::new(0.0, 0.0, w, h) };
        out.append(&mut ex.extract(ExtractInput::DocumentRegion { doc: doc_hash, anchor }, &ctx)?);
    }
    Ok(out)
}

fn op_accepts_kind(ex: &dyn Extractor, kind: crate::artifact::ArtifactKind) -> bool {
    ex.accepts().iter().any(|k| matches!(k, InputKind::Artifact(a) if *a == kind))
}

/// Run a CHAIN of ops over a document, feeding each op the previous op's outputs.
/// The first op consumes the document (a Layout/Extract over each page); each
/// later op consumes the frontier — `Transform`s run per matching artifact,
/// `Merge` runs once over all matching artifacts (N→1). Returns EVERY artifact
/// produced (all generations), so the whole DAG lands in the registry.
///
/// This is the executable form of the prototype's op graph — e.g.
/// `["regions", "text-grid", "structure", "sign-fix"]`.
pub fn run_chain(
    doc: &QDoc,
    doc_hash: DocHash,
    source_path: Option<PathBuf>,
    op_ids: &[String],
) -> Result<Vec<Box<dyn Artifact>>> {
    let Some((first, rest)) = op_ids.split_first() else { bail!("empty op chain") };
    let first_ex = extractor_by_id(first).ok_or_else(|| anyhow::anyhow!("unknown op `{first}`"))?;
    let mut frontier = run_document_extractor(doc, doc_hash, source_path.clone(), Generation(0), first_ex.as_ref())?;

    let mut all: Vec<Box<dyn Artifact>> = Vec::new();
    for (i, op) in rest.iter().enumerate() {
        let ex = extractor_by_id(op).ok_or_else(|| anyhow::anyhow!("unknown op `{op}`"))?;
        let ctx = ExtractCtx { source: doc, generation: Generation((i + 1) as u32), source_path: source_path.clone() };
        let refs: Vec<&dyn Artifact> = frontier.iter().map(|b| b.as_ref()).collect();

        let next = if ex.op_kind() == crate::extract::OpKind::Merge {
            let matching: Vec<&dyn Artifact> = refs.iter().copied().filter(|a| op_accepts_kind(ex.as_ref(), a.kind())).collect();
            if matching.len() < 2 {
                bail!("op `{op}` (merge) needs ≥2 matching inputs, found {}", matching.len());
            }
            ex.extract(ExtractInput::Artifacts(&matching), &ctx)?
        } else {
            let mut out = Vec::new();
            for a in &refs {
                if op_accepts_kind(ex.as_ref(), a.kind()) {
                    out.extend(ex.extract(ExtractInput::Artifacts(std::slice::from_ref(a)), &ctx)?);
                }
            }
            if out.is_empty() {
                bail!("op `{op}` accepted none of the {} frontier artifact(s)", refs.len());
            }
            out
        };
        all.append(&mut frontier); // the consumed frontier is now history
        frontier = next;
    }
    all.append(&mut frontier); // the final outputs
    Ok(all)
}

/// Resolve the extractor for a (format, tier) pair.
pub fn extractor_for(format: DocFormat, tier: u8) -> Result<Box<dyn Extractor>> {
    match (format, tier) {
        (DocFormat::Pdf, 0) => Ok(Box::new(PdfTextLayerReconstructor)),
        (DocFormat::Pdf, t) => bail!("PDF tier {t} not built yet (Phase 0 ships tier 0 only)"),
        (DocFormat::Pptx, _) => bail!("PPTX extractor deferred (brief §6)"),
        (DocFormat::Xlsx, _) => bail!("XLSX extractor deferred (brief §6)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactKind, HtmlTable};
    use crate::doc::{DocFormat, Page, Span, TableRegion};

    fn span(t: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Span {
        Span { text: t.into(), bbox: [x0, y0, x1, y1], confidence: 1.0, rotated: false }
    }

    fn fixture() -> (QDoc, DocHash) {
        let page = Page {
            page: 1,
            width: 600.0,
            height: 800.0,
            spans: vec![
                span("Item", 0.0, 0.0, 40.0, 10.0),
                span("Amount", 100.0, 0.0, 150.0, 10.0),
                span("A", 0.0, 20.0, 10.0, 30.0),
                span("(902)", 100.0, 20.0, 140.0, 30.0),
                span("B", 0.0, 40.0, 10.0, 50.0),
                span("150", 100.0, 40.0, 140.0, 50.0),
            ],
            table_regions: vec![TableRegion { bbox: [0.0, 0.0, 200.0, 60.0], note: None, figure_score: 0.0 }],
        };
        (QDoc { format: DocFormat::Pdf, pages: vec![page] }, DocHash::of(b"chain"))
    }

    #[test]
    fn run_chain_executes_the_native_op_graph() {
        let (doc, dh) = fixture();
        let ops: Vec<String> = ["regions", "text-grid", "structure", "sign-fix"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let arts = run_chain(&doc, dh, None, &ops).unwrap();

        // every generation of the DAG is returned for the registry
        assert!(arts.iter().any(|a| a.kind() == ArtifactKind::Region), "regions stage");
        assert!(arts.iter().any(|a| a.kind() == ArtifactKind::TextGrid), "text-grid stage");
        let tables: Vec<&HtmlTable> = arts
            .iter()
            .filter_map(|a| a.as_any().downcast_ref::<HtmlTable>())
            .collect();
        assert!(tables.len() >= 2, "structure + sign-fix each produced a table");
        // the LAST table (sign-fix) rewrote the parenthesised negative
        let last = tables.last().unwrap();
        assert!(last.cells.iter().any(|c| c.text == "-902"), "sign-fix should have signed (902)");
        // and that final table is Derived (it has a parent in the DAG)
        assert!(matches!(last.provenance(), Provenance::Derived { .. }));
    }

    #[test]
    fn run_chain_rejects_an_unknown_op() {
        let (doc, dh) = fixture();
        let ops = vec!["regions".to_string(), "nope".to_string()];
        assert!(run_chain(&doc, dh, None, &ops).is_err());
    }
}

/// Cheap-parse every page of the document with the tier-n extractor.
pub fn cheap_parse(doc: &QDoc, doc_hash: DocHash, tier: u8) -> Result<Vec<Box<dyn Artifact>>> {
    let extractor = extractor_for(doc.format, tier)?;
    let ctx = ExtractCtx {
        source: doc,
        generation: Generation(0),
        source_path: None, // native tier-0 needs no external file
    };

    let mut artifacts = Vec::new();
    for page in &doc.pages {
        let anchor = SourceAnchor::Pdf {
            doc: doc_hash,
            page: page.page,
            bbox: BBox::new(0.0, 0.0, page.width, page.height),
        };
        let input = ExtractInput::DocumentRegion {
            doc: doc_hash,
            anchor,
        };
        let mut produced = extractor.extract(input, &ctx)?;
        artifacts.append(&mut produced);
    }
    Ok(artifacts)
}
