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

use crate::adjudicate::{AdjudicationRecord, Verdict};
use crate::artifact::{Artifact, RegionRole, StoredArtifact};
use crate::core::*;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// IoU at/above which a region is recognized as an already-registered slot
/// (invariant 3). 0.7 is the standard "same box" cutoff: robust to model jitter,
/// strict enough that two distinct tables on a page don't collide.
pub const ELEMENT_MATCH_IOU: f32 = 0.7;

/// A previously-registered region slot — the geometry `register_or_match`
/// compares against. Derive the `prior` set from the log via [`registered_regions`].
#[derive(Clone, Debug)]
pub struct RegisteredRegion {
    pub element_id: ArtifactId,
    pub page: u32,
    pub role: RegionRole,
    pub bbox: BBox,
}

/// THE identity seam (invariant 3). Recognize an existing source slot by geometry
/// and reuse its `element_id`; otherwise take the freshly-minted id for a new
/// slot. Recognition is by IoU over same-`(page, role)` priors — **never** by
/// re-deriving a hash, so a layout model that nudges the bbox reuses the slot
/// instead of orphaning the corrections bound to it.
///
/// Easy-path degenerate form: callers pass an empty `prior` ⇒ always-mint.
/// Enabling matching later is additive (same fn, same id space) — zero migration.
pub fn register_or_match(
    page: u32,
    role: RegionRole,
    bbox: BBox,
    mint: ArtifactId,
    prior: &[RegisteredRegion],
) -> ArtifactId {
    let best = prior
        .iter()
        .filter(|p| p.page == page && p.role == role)
        .map(|p| (&p.element_id, bbox.iou(&p.bbox)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    match best {
        Some((id, iou)) if iou >= ELEMENT_MATCH_IOU => id.clone(), // recognize → reuse
        _ => mint,                                                 // new slot → name it
    }
}

/// The registered region slots in the current view — the `prior` set for
/// [`register_or_match`] once cross-generation matching is enabled. (The easy
/// path passes an empty slice and always mints; this documents how matching
/// turns on without any change to the seam.)
pub fn registered_regions(observations: &[Observation]) -> Vec<RegisteredRegion> {
    current_view(observations)
        .into_iter()
        .filter_map(|o| match &o.artifact {
            StoredArtifact::Region(r) => Some(RegisteredRegion {
                element_id: o.element_id.clone(),
                page: o.page,
                role: r.role(),
                bbox: o.bbox,
            }),
            _ => None,
        })
        .collect()
}

/// THE resolution seam (invariant 4), generalizing [`current_view`] into a
/// verdict-aware pick. For each element slot, choose the winning observation:
///   1. an explicit verdict `Winner` (adjudicated), if one names a candidate;
///   2. else the newest `Manual` correction — a correction beats any parse;
///   3. else the newest-generation artifact.
///
/// `verdicts` is consulted even when empty, so the day an adjudicator writes one,
/// resolution already honors it with no call-site change (invariant 9). The easy
/// path passes no verdicts and all-`Parser` origins, so this collapses to exactly
/// `current_view` (newest generation wins) — proven by test.
pub fn resolve<'a>(
    observations: &'a [Observation],
    verdicts: &[AdjudicationRecord],
) -> Vec<&'a Observation> {
    let winners: HashSet<&str> = verdicts
        .iter()
        .filter_map(|v| match &v.verdict {
            Verdict::Winner(id) => Some(id.0.as_str()),
            _ => None,
        })
        .collect();

    let mut by_elem: HashMap<&str, Vec<&Observation>> = HashMap::new();
    for o in observations {
        by_elem.entry(o.element_id.0.as_str()).or_default().push(o);
    }

    let mut picked: Vec<&Observation> = by_elem
        .into_values()
        .filter_map(|cands| pick_one(&cands, &winners))
        .collect();
    picked.sort_by(|a, b| a.element_id.0.cmp(&b.element_id.0)); // deterministic
    picked
}

/// The per-slot pick used by [`resolve`]. Candidates all share one `element_id`.
fn pick_one<'a>(cands: &[&'a Observation], winners: &HashSet<&str>) -> Option<&'a Observation> {
    // 1. an explicit adjudicated winner (matched by artifact id)
    if let Some(w) = cands.iter().find(|o| winners.contains(o.artifact.meta().id.0.as_str())) {
        return Some(*w);
    }
    // 2. the newest Manual correction beats any Parser output
    if let Some(m) = cands
        .iter()
        .filter(|o| o.artifact.meta().origin.is_manual())
        .max_by_key(|o| o.generation)
    {
        return Some(*m);
    }
    // 3. else the newest generation
    cands.iter().max_by_key(|o| o.generation).copied()
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

        // re-materialize the resolved snapshot for external tooling, through the
        // one resolution seam (invariant 4) — consulting the full verdict log.
        let all = self.observations()?;
        let all_verdicts = self.verdicts()?;
        let manifest = Manifest {
            doc_hash,
            artifacts: resolve(&all, &all_verdicts).into_iter().map(|o| o.artifact.clone()).collect(),
        };
        write_json(&self.root.join("manifest.json"), &manifest)?;
        Ok(())
    }

    /// THE current-view access function (brief §3): the resolved observation per
    /// element. Everything that wants "the artifacts" goes through here, and it
    /// goes through `resolve` (invariant 4) — so a future verdict or Manual
    /// correction just appears, with no change here.
    pub fn current_artifacts(&self) -> Result<Vec<Box<dyn Artifact>>> {
        let all = self.observations()?;
        let verdicts = self.verdicts()?;
        Ok(resolve(&all, &verdicts).into_iter().map(|o| o.artifact.clone().into_dyn()).collect())
    }

    /// The append-only adjudication log (consulted by `resolve`, even when empty).
    pub fn verdicts(&self) -> Result<Vec<AdjudicationRecord>> {
        read_jsonl(&self.root.join("verdicts.jsonl"))
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
                    origin: Origin::default(),
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
            meta: Meta { id: ArtifactId(id.into()), content_hash: dh, provenance: prov, generation: Generation(g), risk: RiskMarkers::default(), origin: Origin::default() },
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

    fn prior(id: &str, page: u32, role: RegionRole, bbox: BBox) -> RegisteredRegion {
        RegisteredRegion { element_id: ArtifactId(id.into()), page, role, bbox }
    }

    #[test]
    fn register_or_match_mints_when_no_prior() {
        // Easy-path degenerate form: empty prior ⇒ always-mint.
        let mint = ArtifactId("fresh".into());
        let got = register_or_match(1, RegionRole::Table, BBox::new(0.0, 0.0, 10.0, 10.0), mint.clone(), &[]);
        assert_eq!(got, mint, "no prior ⇒ take the freshly-minted id");
    }

    #[test]
    fn register_or_match_reuses_a_high_iou_slot() {
        // A box nudged a few units still overlaps ~well above τ ⇒ reuse the slot,
        // so a correction bound to "slot7" survives the jitter (invariant 3).
        let priors = vec![prior("slot7", 1, RegionRole::Table, BBox::new(0.0, 0.0, 10.0, 10.0))];
        let nudged = BBox::new(0.2, 0.1, 10.1, 9.9);
        let got = register_or_match(1, RegionRole::Table, nudged, ArtifactId("fresh".into()), &priors);
        assert_eq!(got, ArtifactId("slot7".into()), "geometry recognizes the existing slot");
    }

    #[test]
    fn register_or_match_mints_on_low_overlap() {
        let priors = vec![prior("slot7", 1, RegionRole::Table, BBox::new(0.0, 0.0, 10.0, 10.0))];
        let elsewhere = BBox::new(50.0, 50.0, 60.0, 60.0);
        let mint = ArtifactId("fresh".into());
        let got = register_or_match(1, RegionRole::Table, elsewhere, mint.clone(), &priors);
        assert_eq!(got, mint, "a genuinely new region is a new slot");
    }

    #[test]
    fn register_or_match_does_not_cross_role_or_page() {
        // Same geometry, different role or page ⇒ a different slot, never a match.
        let priors = vec![prior("slot7", 1, RegionRole::Table, BBox::new(0.0, 0.0, 10.0, 10.0))];
        let same_box = BBox::new(0.0, 0.0, 10.0, 10.0);
        let mint = ArtifactId("fresh".into());
        assert_eq!(
            register_or_match(1, RegionRole::Figure, same_box, mint.clone(), &priors),
            mint,
            "different role is a different slot"
        );
        assert_eq!(
            register_or_match(2, RegionRole::Table, same_box, mint.clone(), &priors),
            mint,
            "different page is a different slot"
        );
    }

    /// An observation with an explicit (element_id, artifact_id, generation, origin)
    /// — for resolution tests where one slot has competing candidates.
    fn obs_full(element: &str, art_id: &str, g: u32, origin: Origin) -> Observation {
        let dh = DocHash::of(art_id.as_bytes());
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) };
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
                    id: ArtifactId(art_id.into()),
                    content_hash: dh,
                    provenance: Provenance::Source(anchor),
                    generation: Generation(g),
                    risk: RiskMarkers::default(),
                    origin,
                },
                label: "Table".into(),
                confidence: 1.0,
            }),
        }
    }

    #[test]
    fn resolve_degenerate_matches_current_view() {
        // No verdicts + all Parser origins ⇒ resolve collapses to current_view.
        let rows = vec![obs("a", 0), obs("b", 0), obs("a", 1)];
        let r: Vec<_> = resolve(&rows, &[]).iter().map(|o| (o.element_id.0.clone(), o.generation)).collect();
        let cv: Vec<_> = current_view(&rows).iter().map(|o| (o.element_id.0.clone(), o.generation)).collect();
        assert_eq!(r, cv, "the degenerate resolver IS current_view");
    }

    #[test]
    fn resolve_prefers_a_manual_correction_over_a_newer_parse() {
        // Slot "a": a newer Parser parse (gen 1) AND an older Manual fix (gen 0).
        let rows = vec![
            obs_full("a", "parse_v1", 1, Origin::default()),
            obs_full("a", "manual_fix", 0, Origin::Manual { author: "alice".into() }),
        ];
        let r = resolve(&rows, &[]);
        assert_eq!(r.len(), 1, "one winner per slot");
        assert_eq!(r[0].artifact.meta().id, ArtifactId("manual_fix".into()), "Manual beats a newer Parser");
    }

    #[test]
    fn resolve_honors_an_explicit_verdict_winner() {
        let rows = vec![
            obs_full("a", "parse_v1", 1, Origin::default()),
            obs_full("a", "manual_fix", 0, Origin::Manual { author: "alice".into() }),
        ];
        // An adjudicator explicitly crowns the parse, overriding the Manual rule.
        let v = AdjudicationRecord {
            candidates: vec![ArtifactId("parse_v1".into()), ArtifactId("manual_fix".into())],
            flagged: vec![],
            verdict: Verdict::Winner(ArtifactId("parse_v1".into())),
            confidence: 1.0,
            rationale: "test".into(),
        };
        let r = resolve(&rows, &[v]);
        assert_eq!(r[0].artifact.meta().id, ArtifactId("parse_v1".into()), "an explicit verdict wins");
    }
}
