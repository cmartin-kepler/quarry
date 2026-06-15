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

/// Serializable, kind-tagged envelope for persistence and round-tripping the
/// open payload set through the flat store. The live pipeline uses
/// `Box<dyn Artifact>`; the store speaks `StoredArtifact`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StoredArtifact {
    Text(ExtractedText),
    HtmlTable(HtmlTable),
}

impl StoredArtifact {
    pub fn from_dyn(a: &dyn Artifact) -> Option<StoredArtifact> {
        if let Some(t) = a.as_any().downcast_ref::<ExtractedText>() {
            Some(StoredArtifact::Text(t.clone()))
        } else {
            a.as_any()
                .downcast_ref::<HtmlTable>()
                .map(|h| StoredArtifact::HtmlTable(h.clone()))
        }
    }

    pub fn into_dyn(self) -> Box<dyn Artifact> {
        match self {
            StoredArtifact::Text(t) => Box::new(t),
            StoredArtifact::HtmlTable(h) => Box::new(h),
        }
    }

    pub fn meta(&self) -> &Meta {
        match self {
            StoredArtifact::Text(t) => &t.meta,
            StoredArtifact::HtmlTable(h) => &h.meta,
        }
    }
}
