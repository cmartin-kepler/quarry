//! Append-only artifact registry (brief §3, §7).
//!
//! Every `write` APPENDS observations, lineage edges, and verdicts to JSONL logs —
//! nothing is overwritten. "Current state" is a QUERY (latest non-superseded
//! observation per element, brief §7's `DISTINCT ON (element_id) ORDER BY
//! generation DESC`), reached through exactly one access function
//! (`current_artifacts`), so the rest of the system never reads the raw log.
//!
//! Cross-generation element *matching* (which keeps `element_id` stable across
//! re-parses) is still deferred (brief §6); until it lands `element_id ==
//! artifact id` and each element has one generation, so the current-view query is
//! trivially correct and ready for when re-parses arrive.
//!
//! Layout under `<dir>`:
//!   observations.jsonl   — append-only registry rows (metadata + payload)
//!   lineage.jsonl        — append-only DAG edges (parent, child, relation)
//!   verdicts.jsonl       — append-only adjudication records
//!   <artifact_id>.html   — rendered HTML for table artifacts
//!   manifest.json        — MATERIALIZED current-view snapshot (a convenience for
//!                          external tools; re-derived from the registry each write)

use crate::adjudicate::AdjudicationRecord;
use crate::artifact::{Artifact, StoredArtifact};
use crate::core::*;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// How a child element relates to its parent (brief §7: same | split | merge).
/// `Derive` is the 1→1 representation change; `Merge` the N→1 fan-in. `Same` and
/// `Split` arrive with cross-generation matching (deferred).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Relation {
    Derive,
    Merge,
    Same,
    Split,
}

/// An append-only DAG edge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LineageEdge {
    pub parent: ArtifactId,
    pub child: ArtifactId,
    pub generation: Generation,
    pub relation: Relation,
}

/// One append-only registry row: the indexable identity fields (brief §7) plus
/// the artifact payload, so the current-view query can return artifacts directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub element_id: ArtifactId,
    pub generation: Generation,
    pub doc: DocHash,
    pub page: u32,
    pub bbox: BBox,
    pub content_hash: DocHash,
    /// `active` now; the slot where a supersede tombstone would go.
    pub status: String,
    /// A parent in the provenance DAG, if this was derived (else `None` = source).
    pub source_artifact_id: Option<ArtifactId>,
    pub artifact: StoredArtifact,
}

/// THE current-view query (brief §7): the latest-generation observation per
/// `element_id`. Pure, so it's tested directly on synthetic re-parses.
pub fn current_view(observations: &[Observation]) -> Vec<&Observation> {
    let mut best: HashMap<&str, &Observation> = HashMap::new();
    for o in observations {
        let key = o.element_id.0.as_str();
        match best.get(key) {
            Some(prev) if prev.generation >= o.generation => {}
            _ => {
                best.insert(key, o);
            }
        }
    }
    let mut winners: Vec<&Observation> = best.into_values().collect();
    winners.sort_by(|a, b| a.element_id.0.cmp(&b.element_id.0)); // deterministic order
    winners
}

fn page_box(anchor: &SourceAnchor) -> (u32, BBox) {
    match anchor {
        SourceAnchor::Pdf { page, bbox, .. } => (*page, *bbox),
        _ => (0, BBox::new(0.0, 0.0, 0.0, 0.0)),
    }
}

pub struct FlatStore {
    root: PathBuf,
}

impl FlatStore {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        FlatStore { root: root.into() }
    }

    /// Append `artifacts`, their lineage edges, and `verdicts` to the registry.
    /// Never overwrites the logs; re-materializes the current-view `manifest.json`.
    pub fn write(
        &self,
        doc_hash: DocHash,
        artifacts: &[Box<dyn Artifact>],
        verdicts: &[AdjudicationRecord],
    ) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;

        let mut obs = Vec::new();
        let mut edges = Vec::new();
        for a in artifacts {
            let Some(stored) = StoredArtifact::from_dyn(a.as_ref()) else { continue };
            let (page, bbox) = page_box(a.anchor());
            let (parents, source): (Vec<ArtifactId>, Option<ArtifactId>) = match a.provenance() {
                Provenance::Source(_) => (vec![], None),
                Provenance::Derived { parents, .. } => (parents.clone(), parents.first().cloned()),
            };
            // sidecar HTML for tables (the primary artifact form, brief §1, §5)
            if let StoredArtifact::HtmlTable(t) = &stored {
                std::fs::write(self.root.join(format!("{}.html", t.meta.id)), &t.html)?;
            }
            // lineage edges: one per parent; >1 parent = a merge fan-in
            let relation = if parents.len() > 1 { Relation::Merge } else { Relation::Derive };
            for p in &parents {
                edges.push(LineageEdge { parent: p.clone(), child: a.id(), generation: a.generation(), relation });
            }
            obs.push(Observation {
                element_id: a.id(),
                generation: a.generation(),
                doc: doc_hash,
                page,
                bbox,
                content_hash: a.content_hash(),
                status: "active".into(),
                source_artifact_id: source,
                artifact: stored,
            });
        }

        append_jsonl(&self.root.join("observations.jsonl"), &obs)?;
        append_jsonl(&self.root.join("lineage.jsonl"), &edges)?;
        append_jsonl(&self.root.join("verdicts.jsonl"), verdicts)?;

        // re-materialize the current-view snapshot for external tooling
        let all = self.observations()?;
        let manifest = Manifest {
            doc_hash,
            artifacts: current_view(&all).into_iter().map(|o| o.artifact.clone()).collect(),
        };
        write_json(&self.root.join("manifest.json"), &manifest)?;
        Ok(())
    }

    /// THE current-view access function (brief §3): the latest observation per
    /// element. Everything that wants "the artifacts" goes through here.
    pub fn current_artifacts(&self) -> Result<Vec<Box<dyn Artifact>>> {
        let all = self.observations()?;
        Ok(current_view(&all).into_iter().map(|o| o.artifact.clone().into_dyn()).collect())
    }

    /// The full append-only registry (history, not just current view).
    pub fn observations(&self) -> Result<Vec<Observation>> {
        read_jsonl(&self.root.join("observations.jsonl"))
    }

    /// The append-only lineage DAG.
    pub fn lineage(&self) -> Result<Vec<LineageEdge>> {
        read_jsonl(&self.root.join("lineage.jsonl"))
    }

    /// The materialized current-view snapshot (kept for external tools).
    pub fn manifest(&self) -> Result<Manifest> {
        read_json(&self.root.join("manifest.json"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub doc_hash: DocHash,
    pub artifacts: Vec<StoredArtifact>,
}

fn append_jsonl<T: Serialize>(path: &Path, items: &[T]) -> Result<()> {
    if items.is_empty() {
        // ensure the file exists so reads don't error on a never-written log
        if !path.exists() {
            std::fs::write(path, b"")?;
        }
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("appending {}", path.display()))?;
    for it in items {
        writeln!(f, "{}", serde_json::to_string(it)?)?;
    }
    Ok(())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("parsing a row of {}", path.display())))
        .collect()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(value)?;
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{HtmlTable, Meta, Region};

    fn obs(element: &str, g: u32) -> Observation {
        let dh = DocHash::of(element.as_bytes());
        Observation {
            element_id: ArtifactId(element.into()),
            generation: Generation(g),
            doc: dh,
            page: 1,
            bbox: BBox::new(0.0, 0.0, 1.0, 1.0),
            content_hash: dh,
            status: "active".into(),
            source_artifact_id: None,
            artifact: StoredArtifact::Region(Region {
                meta: Meta {
                    id: ArtifactId(element.into()),
                    content_hash: dh,
                    provenance: Provenance::Source(SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) }),
                    generation: Generation(g),
                    risk: RiskMarkers::default(),
                },
                label: format!("gen{g}"),
                confidence: 1.0,
            }),
        }
    }

    #[test]
    fn current_view_keeps_the_latest_generation_per_element() {
        // element "a" re-parsed (gen 0 then gen 1) + a distinct element "b".
        let rows = vec![obs("a", 0), obs("b", 0), obs("a", 1)];
        let view = current_view(&rows);
        assert_eq!(view.len(), 2, "one row per element");
        let a = view.iter().find(|o| o.element_id.0 == "a").unwrap();
        assert_eq!(a.generation, Generation(1), "the newer generation supersedes");
    }

    fn dummy_table(id: &str, prov: Provenance, g: u32) -> Box<dyn Artifact> {
        let dh = DocHash::of(id.as_bytes());
        Box::new(HtmlTable {
            meta: Meta { id: ArtifactId(id.into()), content_hash: dh, provenance: prov, generation: Generation(g), risk: RiskMarkers::default() },
            n_rows: 0,
            n_cols: 0,
            cells: vec![],
            html: format!("<table data-id={id}></table>"),
        })
    }

    #[test]
    fn write_is_append_only_and_records_lineage() {
        let dir = std::env::temp_dir().join(format!("quarry_store_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FlatStore::open(&dir);
        let dh = DocHash::of(b"doc");
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 10.0, 10.0) };

        let parent = dummy_table("parent", Provenance::Source(anchor.clone()), 0);
        let child = dummy_table(
            "child",
            Provenance::Derived { parents: vec![ArtifactId("parent".into())], anchor },
            0,
        );
        store.write(dh, &[parent, child], &[]).unwrap();

        // two observations, one lineage edge (child←parent, Derive), current view = 2
        assert_eq!(store.observations().unwrap().len(), 2);
        let edges = store.lineage().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].parent, ArtifactId("parent".into()));
        assert_eq!(edges[0].child, ArtifactId("child".into()));
        assert_eq!(edges[0].relation, Relation::Derive);
        assert_eq!(store.current_artifacts().unwrap().len(), 2);

        // writing again APPENDS (history grows) but the current view dedups by id
        let again = dummy_table("parent", Provenance::Source(SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 10.0, 10.0) }), 0);
        store.write(dh, &[again], &[]).unwrap();
        assert_eq!(store.observations().unwrap().len(), 3, "log grew (append-only)");
        assert_eq!(store.current_artifacts().unwrap().len(), 2, "current view still 2");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_artifact_records_a_merge_relation() {
        let dir = std::env::temp_dir().join(format!("quarry_merge_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FlatStore::open(&dir);
        let dh = DocHash::of(b"doc");
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) };
        let merged = dummy_table(
            "merged",
            Provenance::Derived { parents: vec![ArtifactId("a".into()), ArtifactId("b".into())], anchor },
            0,
        );
        store.write(dh, &[merged], &[]).unwrap();
        let edges = store.lineage().unwrap();
        assert_eq!(edges.len(), 2, "one edge per parent");
        assert!(edges.iter().all(|e| e.relation == Relation::Merge), "N→1 is a merge");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
