//! Flat artifact store for Phase 0. The append-only registry + current-view
//! machinery is deferred (brief §6), but the brief's day-one rule still holds:
//! "current state" goes through exactly ONE access function so the eventual swap
//! to the append-only registry touches one call site (brief §3).
//!
//! Layout under `<dir>`:
//!   manifest.json            — list of artifact records (metadata + payload)
//!   <artifact_id>.html       — rendered HTML for table artifacts
//!   verdicts.json            — append-only adjudication records

use crate::adjudicate::AdjudicationRecord;
use crate::artifact::{Artifact, StoredArtifact};
use crate::core::DocHash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub doc_hash: DocHash,
    pub artifacts: Vec<StoredArtifact>,
}

pub struct FlatStore {
    root: PathBuf,
}

impl FlatStore {
    pub fn open(root: impl Into<PathBuf>) -> Self {
        FlatStore { root: root.into() }
    }

    pub fn write(
        &self,
        doc_hash: DocHash,
        artifacts: &[Box<dyn Artifact>],
        verdicts: &[AdjudicationRecord],
    ) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;

        let stored: Vec<StoredArtifact> = artifacts
            .iter()
            .filter_map(|a| StoredArtifact::from_dyn(a.as_ref()))
            .collect();

        // Sidecar HTML for tables (the primary artifact form, brief §1, §5).
        for s in &stored {
            if let StoredArtifact::HtmlTable(t) = s {
                let path = self.root.join(format!("{}.html", t.meta.id));
                std::fs::write(&path, &t.html)
                    .with_context(|| format!("writing {}", path.display()))?;
            }
        }

        let manifest = Manifest {
            doc_hash,
            artifacts: stored,
        };
        write_json(&self.root.join("manifest.json"), &manifest)?;
        write_json(&self.root.join("verdicts.json"), &verdicts.to_vec())?;
        Ok(())
    }

    /// THE current-view access function (brief §3). Everything that wants "the
    /// artifacts" goes through here — never reads manifest.json directly — so
    /// swapping in the append-only `DISTINCT ON (element_id)` query later is a
    /// one-site change.
    pub fn current_artifacts(&self) -> Result<Vec<Box<dyn Artifact>>> {
        let manifest: Manifest = read_json(&self.root.join("manifest.json"))?;
        Ok(manifest
            .artifacts
            .into_iter()
            .map(StoredArtifact::into_dyn)
            .collect())
    }

    pub fn manifest(&self) -> Result<Manifest> {
        read_json(&self.root.join("manifest.json"))
    }
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
