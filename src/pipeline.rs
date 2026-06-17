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
        "yolo26" | "doclayout" | "surya" => Some(Box::new(LayoutSidecar::model(id))),
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
    ex: &dyn Extractor,
) -> Result<Vec<Box<dyn Artifact>>> {
    if !ex.accepts().iter().any(|k| matches!(k, InputKind::DocumentRegion)) {
        bail!(
            "op `{}` consumes artifacts, not a document region — run it inside an op chain",
            ex.id().0
        );
    }
    let ctx = ExtractCtx { source: doc, generation: Generation(0), source_path };
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

/// Resolve the extractor for a (format, tier) pair.
pub fn extractor_for(format: DocFormat, tier: u8) -> Result<Box<dyn Extractor>> {
    match (format, tier) {
        (DocFormat::Pdf, 0) => Ok(Box::new(PdfTextLayerReconstructor)),
        (DocFormat::Pdf, t) => bail!("PDF tier {t} not built yet (Phase 0 ships tier 0 only)"),
        (DocFormat::Pptx, _) => bail!("PPTX extractor deferred (brief §6)"),
        (DocFormat::Xlsx, _) => bail!("XLSX extractor deferred (brief §6)"),
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
