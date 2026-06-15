//! Glue: pick the right format-specific extractor for a tier and run it over the
//! whole document. In Phase 0 there's exactly one PDF extractor (tier 0); higher
//! tiers and PPTX/XLSX are deferred (brief §6) and surface as clear errors.

use crate::artifact::Artifact;
use crate::core::*;
use crate::doc::{DocFormat, QDoc};
use crate::extract::*;
use anyhow::{Result, bail};

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
