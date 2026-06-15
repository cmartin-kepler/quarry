//! The `.qdoc` fixture format: a stand-in for "the original bytes". A real build
//! parses PDF/PPTX/XLSX here; for the example this is a JSON text-layer so the
//! whole pipeline — extraction, risk, checks, eval — runs end to end and is
//! testable. Crucially the cheap extractor reconstructs tables *geometrically*
//! from these spans, so it can still get a clean-looking table wrong (transposed
//! column, shifted row) — exactly the silent failure the brief is built to catch.

use crate::core::{BBox, DocHash};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    /// `[x0, y0, x1, y1]` in points, origin top-left.
    pub bbox: [f32; 4],
    /// Text/OCR confidence; 1.0 for born-digital. Optional (defaults to 1.0).
    #[serde(default = "one")]
    pub confidence: f32,
    /// Rotated text in the source — a parse-time risk signal.
    #[serde(default)]
    pub rotated: bool,
}

fn one() -> f32 {
    1.0
}

impl Span {
    pub fn bbox(&self) -> BBox {
        BBox::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3])
    }
}

/// A region the source marks as a table. Detecting *that a table exists* is a
/// separate problem; here we hand the extractor the region so the example stays
/// focused on the high-value failure surface: getting the cell contents right.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableRegion {
    pub bbox: [f32; 4],
    #[serde(default)]
    pub note: Option<String>,
}

impl TableRegion {
    pub fn bbox(&self) -> BBox {
        BBox::new(self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub page: u32,
    pub width: f32,
    pub height: f32,
    pub spans: Vec<Span>,
    #[serde(default)]
    pub table_regions: Vec<TableRegion>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocFormat {
    Pdf,
    Pptx,
    Xlsx,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QDoc {
    pub format: DocFormat,
    pub pages: Vec<Page>,
}

impl QDoc {
    pub fn load(path: &Path) -> Result<(QDoc, DocHash)> {
        let bytes =
            std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        // doc_hash IS document identity (brief §3): hash the raw bytes.
        let hash = DocHash::of(&bytes);
        let doc: QDoc = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {} as .qdoc JSON", path.display()))?;
        Ok((doc, hash))
    }

    pub fn page(&self, page: u32) -> Option<&Page> {
        self.pages.iter().find(|p| p.page == page)
    }
}
