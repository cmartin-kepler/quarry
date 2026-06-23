//! Stage-0 page triage (doc-build-order.md Phase 1).
//!
//! Consume `scripts/triage.py` output and route each page:
//!   - `text`          → parse (Stage 1: docling + litparse)
//!   - `image_content` → `ImageRef{OcrDeferred}` (recorded OCR target, invariant 11)
//!   - `blank`         → skip
//!
//! This is the cheap gate that keeps image/blank pages out of docling — where the
//! table-structure model otherwise wastes ~950ms on a full-page image it misreads
//! as a table.

use crate::artifact::ImageRef;
use crate::core::{BBox, DocHash};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageClass {
    Text,
    ImageContent,
    Blank,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PageTriage {
    pub page: u32,
    #[serde(default)]
    pub width: f32,
    #[serde(default)]
    pub height: f32,
    #[serde(default)]
    pub words: u32,
    #[serde(default)]
    pub image_frac: f32,
    #[serde(default)]
    pub stddev: Option<f32>,
    pub klass: PageClass,
}

/// Parse `scripts/triage.py` JSON output.
pub fn parse(json: &str) -> serde_json::Result<Vec<PageTriage>> {
    serde_json::from_str(json)
}

/// The pages to send to Stage 1 (docling + litparse).
pub fn text_pages(t: &[PageTriage]) -> Vec<u32> {
    t.iter().filter(|p| p.klass == PageClass::Text).map(|p| p.page).collect()
}

/// OCR-deferred markers for image-content pages (invariant 11): one `ImageRef` per
/// page so a future OCR pass knows exactly which pages have content (and that they
/// aren't blank). The anchor is the full page bbox (from the triage dimensions).
pub fn ocr_markers(t: &[PageTriage], doc: DocHash) -> Vec<ImageRef> {
    t.iter()
        .filter(|p| p.klass == PageClass::ImageContent)
        .map(|p| ImageRef::ocr_deferred(doc, p.page, BBox::new(0.0, 0.0, p.width, p.height)))
        .collect()
}

/// `(text, image_content, blank)` page counts.
pub fn counts(t: &[PageTriage]) -> (usize, usize, usize) {
    let mut c = (0, 0, 0);
    for p in t {
        match p.klass {
            PageClass::Text => c.0 += 1,
            PageClass::ImageContent => c.1 += 1,
            PageClass::Blank => c.2 += 1,
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ImageStatus;

    const SAMPLE: &str = r#"[
        {"page":1,"width":612,"height":792,"words":361,"image_frac":0.0,"stddev":null,"klass":"text"},
        {"page":2,"width":612,"height":792,"words":0,"image_frac":0.0,"stddev":0.0,"klass":"blank"},
        {"page":3,"width":612,"height":792,"words":0,"image_frac":1.0,"stddev":33.5,"klass":"image_content"}
    ]"#;

    #[test]
    fn parses_and_routes() {
        let t = parse(SAMPLE).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(text_pages(&t), vec![1], "only the text page goes to Stage 1");
        assert_eq!(counts(&t), (1, 1, 1));
    }

    #[test]
    fn image_content_pages_become_ocr_deferred_markers() {
        let t = parse(SAMPLE).unwrap();
        let doc = DocHash::of(b"d");
        let markers = ocr_markers(&t, doc);
        assert_eq!(markers.len(), 1, "one marker for the image_content page (not blank/text)");
        assert_eq!(markers[0].status, ImageStatus::OcrDeferred);
        match markers[0].meta.provenance.anchor() {
            crate::core::SourceAnchor::Pdf { page, bbox, .. } => {
                assert_eq!(*page, 3);
                assert_eq!(*bbox, BBox::new(0.0, 0.0, 612.0, 792.0), "full-page anchor");
            }
            _ => panic!("expected a PDF page anchor"),
        }
    }
}
