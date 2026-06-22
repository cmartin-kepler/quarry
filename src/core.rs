//! Provenance, geometry, identity, and risk — the immutable, content-addressed
//! spine shared by every artifact (brief §3, §4).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// 32-byte content address. Doubles as document identity (`doc_hash` IS document
/// identity for now — brief §3) and as an artifact content hash for dedup.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocHash(pub [u8; 32]);

impl DocHash {
    pub fn of(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(bytes);
        let out = h.finalize();
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&out);
        DocHash(buf)
    }

    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Short prefix for human-facing labels and opaque-ID minting.
    pub fn short(&self) -> String {
        self.hex()[..12].to_string()
    }
}

impl fmt::Debug for DocHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DocHash({})", self.short())
    }
}

impl Serialize for DocHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for DocHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("DocHash must be 32 bytes"));
        }
        let mut buf = [0u8; 32];
        buf.copy_from_slice(&bytes);
        Ok(DocHash(buf))
    }
}

/// Opaque artifact handle. Agents only ever see these — never coordinates
/// (brief §3). In Phase 0 there are no re-parses, so we mint deterministically
/// from content; the cross-generation *matching* that keeps IDs stable is
/// explicitly deferred (brief §3, §6).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub String);

impl ArtifactId {
    /// Deterministic mint from the artifact's content hash + generation.
    pub fn mint(content: &DocHash, generation: Generation) -> Self {
        ArtifactId(format!("art_{}_{}", content.short(), generation.0))
    }
}

impl fmt::Debug for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtractorId(pub String);

/// Who or what produced an artifact (invariant 5: everything is attributable).
/// `Parser` is the default — the machine extractors. `Manual` (a correction) and
/// future judge origins slot in without a schema change; resolution prefers
/// `Manual` over `Parser` (a correction beats a parse).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Origin {
    Parser { extractor: ExtractorId, version: Version },
    Manual { author: String },
}

impl Default for Origin {
    /// Parser of unknown provenance — the safe default for artifacts minted before
    /// an extractor threads its own id through. Real extractors should set this.
    fn default() -> Self {
        Origin::Parser { extractor: ExtractorId("unknown".into()), version: Version(0) }
    }
}

impl Origin {
    pub fn parser(extractor: impl Into<String>, version: u32) -> Self {
        Origin::Parser { extractor: ExtractorId(extractor.into()), version: Version(version) }
    }
    pub fn is_manual(&self) -> bool {
        matches!(self, Origin::Manual { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CheckId(pub String);

/// Per-job monotonic counter per document (brief §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Generation(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version(pub u32);

// ---- Geometry -------------------------------------------------------------

/// Axis-aligned box in PDF points (origin top-left).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl BBox {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        BBox { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    pub fn contains_center(&self, other: &BBox) -> bool {
        let (cx, cy) = other.center();
        cx >= self.x0 && cx <= self.x1 && cy >= self.y0 && cy <= self.y1
    }

    /// Intersection-over-union — the bbox half of element matching (brief §3).
    pub fn iou(&self, other: &BBox) -> f32 {
        let ix0 = self.x0.max(other.x0);
        let iy0 = self.y0.max(other.y0);
        let ix1 = self.x1.min(other.x1);
        let iy1 = self.y1.min(other.y1);
        if ix1 <= ix0 || iy1 <= iy0 {
            return 0.0;
        }
        let inter = (ix1 - ix0) * (iy1 - iy0);
        let union = self.area() + other.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    /// Smallest box covering both.
    pub fn union(&self, other: &BBox) -> BBox {
        BBox {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// Cell range for spreadsheet anchors (defined for completeness; XLSX is deferred).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellRange {
    pub first: String,
    pub last: String,
}

/// Where an artifact lives in the original bytes. Always resolved & materialized
/// so citation lookup is O(1) (brief §3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum SourceAnchor {
    Pdf {
        doc: DocHash,
        page: u32,
        bbox: BBox,
    },
    Pptx {
        doc: DocHash,
        slide: u32,
        shape_id: String,
    },
    Xlsx {
        doc: DocHash,
        sheet: String,
        range: CellRange,
    },
}

impl SourceAnchor {
    pub fn doc(&self) -> DocHash {
        match self {
            SourceAnchor::Pdf { doc, .. }
            | SourceAnchor::Pptx { doc, .. }
            | SourceAnchor::Xlsx { doc, .. } => *doc,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Provenance {
    /// Derived directly from the original bytes.
    Source(SourceAnchor),
    /// Derived from other artifacts; `anchor` is the *resolved* source anchor,
    /// materialized so citation lookup never walks the chain (brief §3, §4).
    Derived {
        parents: Vec<ArtifactId>,
        anchor: SourceAnchor,
    },
}

impl Provenance {
    /// The materialized resolved anchor — present whether Source or Derived.
    pub fn anchor(&self) -> &SourceAnchor {
        match self {
            Provenance::Source(a) => a,
            Provenance::Derived { anchor, .. } => anchor,
        }
    }
}

// ---- Risk -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// Parse-time confidence signals (brief §2 detector 2, §4). These are cheap to
/// compute during extraction and are the second detector the eval harness scores.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RiskMarkers {
    /// Min span-level OCR/text confidence in the region (1.0 = born-digital).
    pub min_ocr_confidence: f32,
    /// Variance in cell count across rows — high means ragged/misaligned grid.
    pub column_count_variance: f32,
    /// Rows whose cell count is below the modal column count (merged-cell
    /// ambiguity or dropped cells).
    pub merged_cell_rows: u32,
    /// Cells that came out empty — suspicious for a dense financial table.
    pub empty_cells: u32,
    /// Any span flagged as rotated in the source.
    pub rotated_text: bool,
    /// Fraction of the region that is dark/saturated filled rectangles — high
    /// means the "table" region is really a chart/infographic (see §2 figure guard).
    #[serde(default)]
    pub figure_score: f32,
    /// Free-form notes accumulated during extraction.
    pub notes: Vec<String>,
}

impl RiskMarkers {
    /// True if any parse-time signal looks risky enough to warrant a flag.
    pub fn looks_risky(&self) -> bool {
        self.min_ocr_confidence < 0.85
            || self.column_count_variance > 0.5
            || self.merged_cell_rows > 0
            || self.empty_cells > 0
            || self.rotated_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dochash_roundtrips_through_hex() {
        let h = DocHash::of(b"hello quarry");
        let json = serde_json::to_string(&h).unwrap();
        let back: DocHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
        assert_eq!(h.hex().len(), 64);
    }

    #[test]
    fn iou_is_one_for_identical_boxes() {
        let b = BBox::new(0.0, 0.0, 10.0, 10.0);
        assert!((b.iou(&b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_is_zero_for_disjoint_boxes() {
        let a = BBox::new(0.0, 0.0, 1.0, 1.0);
        let b = BBox::new(5.0, 5.0, 6.0, 6.0);
        assert_eq!(a.iou(&b), 0.0);
    }
}
