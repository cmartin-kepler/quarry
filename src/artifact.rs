//! The object-safe `Artifact` core + the two Phase-0 concrete payloads
//! (Text, HtmlTable). Payload strategy is the brief's recommended hybrid: a
//! `dyn`-safe trait for shared metadata/provenance/risk, an `ArtifactKind` enum
//! for cheap matching, and `as_any()` for downcasting to the concrete payload.

use crate::core::*;
use serde::{Deserialize, Serialize};
use std::any::Any;

/// Closed kind tag for matching/routing. The *payload* set stays open via the
/// trait + downcast; this enum just makes dispatch and persistence ergonomic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    Text,
    /// A located area on a page (a layout detection): the input to extraction.
    Region,
    /// Raw region text + word geometry, columns not yet committed (LiteParse-style
    /// ASCII); `structure` turns it into an HtmlTable.
    TextGrid,
    HtmlTable,
    DbTable,
    ChartData,
    Index,
}

/// Shared metadata carried by every artifact. Concrete payloads embed one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub id: ArtifactId,
    pub content_hash: DocHash,
    pub provenance: Provenance,
    pub generation: Generation,
    pub risk: RiskMarkers,
    /// Who/what produced this artifact (invariant 5). Defaults to `Parser` so
    /// existing stored rows and literals deserialize unchanged.
    #[serde(default)]
    pub origin: Origin,
}

/// Object-safe core trait. Everything shared lives here; payload accessors live
/// on the concrete types reached via `as_any()`.
pub trait Artifact: Send + Sync {
    fn id(&self) -> ArtifactId;
    fn content_hash(&self) -> DocHash;
    fn provenance(&self) -> &Provenance;
    fn kind(&self) -> ArtifactKind;
    fn generation(&self) -> Generation;
    fn risk(&self) -> &RiskMarkers;
    fn as_any(&self) -> &dyn Any;

    /// The resolved source anchor (materialized; never walks the DAG).
    fn anchor(&self) -> &SourceAnchor {
        self.provenance().anchor()
    }
}

// ---- ExtractedText --------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextSpan {
    pub text: String,
    pub bbox: BBox,
    /// Index in reading order.
    pub order: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExtractedText {
    pub meta: Meta,
    pub spans: Vec<TextSpan>,
}

impl ExtractedText {
    /// Reading-order plain text.
    pub fn reading_order(&self) -> String {
        let mut spans: Vec<&TextSpan> = self.spans.iter().collect();
        spans.sort_by_key(|s| s.order);
        spans
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Artifact for ExtractedText {
    fn id(&self) -> ArtifactId {
        self.meta.id.clone()
    }
    fn content_hash(&self) -> DocHash {
        self.meta.content_hash
    }
    fn provenance(&self) -> &Provenance {
        &self.meta.provenance
    }
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Text
    }
    fn generation(&self) -> Generation {
        self.meta.generation
    }
    fn risk(&self) -> &RiskMarkers {
        &self.meta.risk
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- HtmlTable ------------------------------------------------------------

/// One table cell. Each cell carries its own resolved anchor (brief §4) so a
/// citation can point at a single number, not just the whole table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub text: String,
    pub anchor: SourceAnchor,
    /// Header cells are excluded from arithmetic checks.
    pub is_header: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HtmlTable {
    pub meta: Meta,
    pub n_rows: u32,
    pub n_cols: u32,
    pub cells: Vec<Cell>,
    /// Rendered HTML (the primary artifact form per brief §1).
    pub html: String,
}

impl HtmlTable {
    /// Dense row-major view; missing cells become empty strings.
    pub fn grid(&self) -> Vec<Vec<String>> {
        let mut grid = vec![vec![String::new(); self.n_cols as usize]; self.n_rows as usize];
        for c in &self.cells {
            if (c.row as usize) < grid.len() && (c.col as usize) < self.n_cols as usize {
                grid[c.row as usize][c.col as usize] = c.text.clone();
            }
        }
        grid
    }

    pub fn cell(&self, row: u32, col: u32) -> Option<&Cell> {
        self.cells.iter().find(|c| c.row == row && c.col == col)
    }
}

impl Artifact for HtmlTable {
    fn id(&self) -> ArtifactId {
        self.meta.id.clone()
    }
    fn content_hash(&self) -> DocHash {
        self.meta.content_hash
    }
    fn provenance(&self) -> &Provenance {
        &self.meta.provenance
    }
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::HtmlTable
    }
    fn generation(&self) -> Generation {
        self.meta.generation
    }
    fn risk(&self) -> &RiskMarkers {
        &self.meta.risk
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- Region ---------------------------------------------------------------

/// Typed region classification (the layout roles). The detector's raw string
/// lives in `Region::label`; `RegionRole` is the canonical typed view, *derived*
/// from the label so it stays a deterministic function of stored data — adding a
/// stored field later is additive, never a migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionRole {
    Table,
    Text,
    Figure,
    Caption,
    /// The detector said something we don't map. Kept explicit, never dropped:
    /// an unclassified region is still a recorded slot (no silent gaps).
    Other,
}

impl RegionRole {
    /// Map a detector's free-text label to a typed role. Case-insensitive; covers
    /// the common layout vocabularies (YOLO / docling / pdfplumber).
    pub fn from_label(label: &str) -> RegionRole {
        match label.trim().to_ascii_lowercase().as_str() {
            "table" => RegionRole::Table,
            "figure" | "image" | "picture" | "chart" => RegionRole::Figure,
            "caption" | "table-caption" | "figure-caption" => RegionRole::Caption,
            "text" | "paragraph" | "title" | "section-header" | "list" | "page-header"
            | "page-footer" | "footnote" => RegionRole::Text,
            _ => RegionRole::Other,
        }
    }

    /// Whether extraction is deferred for this role: figures are recorded as image
    /// markers (an `ImageRef`), not parsed — so the region is never a silent gap.
    pub fn extraction_deferred(self) -> bool {
        matches!(self, RegionRole::Figure)
    }
}

/// A located area on a page (a layout detection). Its resolved anchor IS its
/// bbox, so it's both an artifact and the input a region-scoped extractor reads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Region {
    pub meta: Meta,
    /// What the detector called it ("Table", "Figure", "Text", …).
    pub label: String,
    /// Detector / cross-model-agreement confidence in [0, 1].
    pub confidence: f32,
}

impl Region {
    /// The located bbox (the anchor's box).
    pub fn bbox(&self) -> BBox {
        match self.meta.provenance.anchor() {
            SourceAnchor::Pdf { bbox, .. } => *bbox,
            _ => BBox::new(0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Typed classification of this region (see [`RegionRole`]).
    pub fn role(&self) -> RegionRole {
        RegionRole::from_label(&self.label)
    }
}

impl Artifact for Region {
    fn id(&self) -> ArtifactId {
        self.meta.id.clone()
    }
    fn content_hash(&self) -> DocHash {
        self.meta.content_hash
    }
    fn provenance(&self) -> &Provenance {
        &self.meta.provenance
    }
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::Region
    }
    fn generation(&self) -> Generation {
        self.meta.generation
    }
    fn risk(&self) -> &RiskMarkers {
        &self.meta.risk
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- TextGrid -------------------------------------------------------------

/// One positioned word (token) — the geometry `structure` clusters into columns.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub bbox: BBox,
}

/// A region's text as a faithful monospace block (`text`, for display) PLUS the
/// word geometry (`words`, what `structure` actually clusters). Columns are not
/// yet committed — this is the representation between a Region and an HtmlTable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextGrid {
    pub meta: Meta,
    pub text: String,
    pub words: Vec<Word>,
}

impl Artifact for TextGrid {
    fn id(&self) -> ArtifactId {
        self.meta.id.clone()
    }
    fn content_hash(&self) -> DocHash {
        self.meta.content_hash
    }
    fn provenance(&self) -> &Provenance {
        &self.meta.provenance
    }
    fn kind(&self) -> ArtifactKind {
        ArtifactKind::TextGrid
    }
    fn generation(&self) -> Generation {
        self.meta.generation
    }
    fn risk(&self) -> &RiskMarkers {
        &self.meta.risk
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Serializable, kind-tagged envelope for persistence and round-tripping the
/// open payload set through the flat store. The live pipeline uses
/// `Box<dyn Artifact>`; the store speaks `StoredArtifact`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StoredArtifact {
    Text(ExtractedText),
    Region(Region),
    TextGrid(TextGrid),
    HtmlTable(HtmlTable),
}

impl StoredArtifact {
    pub fn from_dyn(a: &dyn Artifact) -> Option<StoredArtifact> {
        let any = a.as_any();
        if let Some(t) = any.downcast_ref::<ExtractedText>() {
            Some(StoredArtifact::Text(t.clone()))
        } else if let Some(r) = any.downcast_ref::<Region>() {
            Some(StoredArtifact::Region(r.clone()))
        } else if let Some(g) = any.downcast_ref::<TextGrid>() {
            Some(StoredArtifact::TextGrid(g.clone()))
        } else {
            any.downcast_ref::<HtmlTable>()
                .map(|h| StoredArtifact::HtmlTable(h.clone()))
        }
    }

    pub fn into_dyn(self) -> Box<dyn Artifact> {
        match self {
            StoredArtifact::Text(t) => Box::new(t),
            StoredArtifact::Region(r) => Box::new(r),
            StoredArtifact::TextGrid(g) => Box::new(g),
            StoredArtifact::HtmlTable(h) => Box::new(h),
        }
    }

    pub fn meta(&self) -> &Meta {
        match self {
            StoredArtifact::Text(t) => &t.meta,
            StoredArtifact::Region(r) => &r.meta,
            StoredArtifact::TextGrid(g) => &g.meta,
            StoredArtifact::HtmlTable(h) => &h.meta,
        }
    }
}
