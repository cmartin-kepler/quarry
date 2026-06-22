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
    /// A recorded image region whose extraction is deferred (invariant 11: no
    /// silent gaps — the area is a known image, not an empty hole).
    Image,
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
            // captions/footnotes (hyphen + DocLayout-YOLO underscore vocabularies)
            "caption" | "table-caption" | "table_caption" | "figure-caption"
            | "figure_caption" | "table_footnote" | "formula_caption" => RegionRole::Caption,
            // body/heading/furniture text. "plain text" + "abandon" are DocLayout-YOLO's
            // (abandon = its throwaway class for running headers/footers/page numbers —
            // mapped to Text so that furniture is "covered", not a false body-orphan).
            "text" | "plain text" | "plain-text" | "paragraph" | "title" | "section-header"
            | "section_header" | "list" | "page-header" | "page-footer" | "footnote"
            | "abandon" => RegionRole::Text,
            // e.g. DocLayout-YOLO "isolate_formula" — recorded, not mapped (no silent drop)
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

// ---- ImageRef -------------------------------------------------------------

/// A recorded-but-not-extracted image region (invariant 11: no silent gaps).
/// A `Figure` region yields one of these, so downstream knows the area IS an image
/// with extraction *deferred* — never an empty hole someone mistakes for a bug. A
/// future figure/chart extractor is a competing artifact on the same `element_id`
/// (additive, invariant 9).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageRef {
    pub meta: Meta,
    /// Path to a rendered crop of the image, if one was produced (None until
    /// rendering lands; the marker is useful without it).
    #[serde(default)]
    pub crop: Option<String>,
}

impl ImageRef {
    /// Build the deferred-extraction marker for a region (intended for `Figure`).
    /// Derived from the region (its bbox is the anchor), with a deterministic id.
    pub fn from_region(region: &Region) -> ImageRef {
        let content = DocHash::of(format!("image:{}", region.id()).as_bytes());
        ImageRef {
            meta: Meta {
                id: ArtifactId::mint(&content, region.generation()),
                content_hash: content,
                provenance: Provenance::Derived {
                    parents: vec![region.id()],
                    anchor: region.provenance().anchor().clone(),
                },
                generation: region.generation(),
                risk: RiskMarkers::default(),
                origin: Origin::default(),
            },
            crop: None,
        }
    }

    /// The image's located bbox (the anchor's box).
    pub fn bbox(&self) -> BBox {
        match self.meta.provenance.anchor() {
            SourceAnchor::Pdf { bbox, .. } => *bbox,
            _ => BBox::new(0.0, 0.0, 0.0, 0.0),
        }
    }
}

impl Artifact for ImageRef {
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
        ArtifactKind::Image
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

/// The `FigureMarker` op (build-plan Step C): turn every `Figure` region into a
/// recorded `ImageRef` (invariant 11). Non-figure regions are left for their own
/// extractors; figures are recorded, not dropped.
pub fn figure_markers(regions: &[Region]) -> Vec<ImageRef> {
    regions
        .iter()
        .filter(|r| r.role() == RegionRole::Figure)
        .map(ImageRef::from_region)
        .collect()
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
    Image(ImageRef),
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
        } else if let Some(i) = any.downcast_ref::<ImageRef>() {
            Some(StoredArtifact::Image(i.clone()))
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
            StoredArtifact::Image(i) => Box::new(i),
        }
    }

    pub fn meta(&self) -> &Meta {
        match self {
            StoredArtifact::Text(t) => &t.meta,
            StoredArtifact::Region(r) => &r.meta,
            StoredArtifact::TextGrid(g) => &g.meta,
            StoredArtifact::HtmlTable(h) => &h.meta,
            StoredArtifact::Image(i) => &i.meta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(label: &str, bbox: BBox) -> Region {
        let dh = DocHash::of(label.as_bytes());
        Region {
            meta: Meta {
                id: ArtifactId::mint(&dh, Generation(0)),
                content_hash: dh,
                provenance: Provenance::Source(SourceAnchor::Pdf { doc: dh, page: 2, bbox }),
                generation: Generation(0),
                risk: RiskMarkers::default(),
                origin: Origin::default(),
            },
            label: label.into(),
            confidence: 1.0,
        }
    }

    #[test]
    fn region_role_maps_common_layout_labels() {
        assert_eq!(RegionRole::from_label("Table"), RegionRole::Table);
        assert_eq!(RegionRole::from_label("picture"), RegionRole::Figure);
        assert_eq!(RegionRole::from_label("Section-Header"), RegionRole::Text);
        // DocLayout-YOLO's real vocabulary (seen on the sample page)
        assert_eq!(RegionRole::from_label("plain text"), RegionRole::Text);
        assert_eq!(RegionRole::from_label("abandon"), RegionRole::Text);
        assert_eq!(RegionRole::from_label("table_caption"), RegionRole::Caption);
        assert_eq!(RegionRole::from_label("isolate_formula"), RegionRole::Other);
        assert_eq!(RegionRole::from_label("widget"), RegionRole::Other);
        assert!(RegionRole::Figure.extraction_deferred());
        assert!(!RegionRole::Table.extraction_deferred());
    }

    #[test]
    fn figure_markers_records_only_figures_with_extraction_deferred() {
        let regions = vec![
            region("Figure", BBox::new(0.0, 0.0, 100.0, 100.0)),
            region("Table", BBox::new(0.0, 200.0, 100.0, 300.0)),
            region("Text", BBox::new(0.0, 400.0, 100.0, 500.0)),
        ];
        let imgs = figure_markers(&regions);
        assert_eq!(imgs.len(), 1, "only the Figure becomes an ImageRef");
        let img = &imgs[0];
        assert_eq!(img.kind(), ArtifactKind::Image);
        assert_eq!(img.bbox(), BBox::new(0.0, 0.0, 100.0, 100.0), "keeps the region bbox");
        assert!(img.crop.is_none(), "extraction deferred — no crop yet");
        assert!(matches!(img.provenance(), Provenance::Derived { .. }), "derived from its region");
    }

    #[test]
    fn image_ref_round_trips_through_the_store_envelope() {
        let img = ImageRef::from_region(&region("Figure", BBox::new(1.0, 2.0, 3.0, 4.0)));
        let stored = StoredArtifact::from_dyn(&img).expect("downcasts to Image");
        assert!(matches!(stored, StoredArtifact::Image(_)));
        let back = stored.into_dyn();
        assert_eq!(back.kind(), ArtifactKind::Image);
        assert_eq!(back.id(), img.id());
    }
}
