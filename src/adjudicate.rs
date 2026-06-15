//! Adjudication: quality is NOT a total order (brief §4). Pick a default at
//! parse time; surface only genuine ambiguity to query-time agents. Verdicts are
//! append-only records so "why did the system prefer reading A" is auditable.

use crate::artifact::Artifact;
use crate::check::CheckOutcome;
use crate::core::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Verdict {
    /// Clear best → becomes the default.
    Winner(ArtifactId),
    /// Agreement → confidence boost, either works.
    Equivalent(Vec<ArtifactId>),
    /// ONLY this reaches agents, with alternatives attached.
    Ambiguous(Vec<ArtifactId>),
}

/// Append-only adjudication record (brief §4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdjudicationRecord {
    pub candidates: Vec<ArtifactId>,
    pub flagged: Vec<ArtifactId>,
    pub verdict: Verdict,
    pub confidence: f32,
    pub rationale: String,
}

pub trait Adjudicator: Send + Sync {
    fn adjudicate(
        &self,
        candidates: &[&dyn Artifact],
        checks: &[CheckOutcome],
    ) -> AdjudicationRecord;
}

/// Phase-0 adjudicator: one candidate per anchor (no re-parses yet), so the job
/// is mostly to record whether checks flagged it. A clean candidate wins; a
/// flagged sole candidate is Ambiguous (the agent should be told).
pub struct DefaultAdjudicator;

impl Adjudicator for DefaultAdjudicator {
    fn adjudicate(
        &self,
        candidates: &[&dyn Artifact],
        checks: &[CheckOutcome],
    ) -> AdjudicationRecord {
        let ids: Vec<ArtifactId> = candidates.iter().map(|a| a.id()).collect();
        let any_flag = checks.iter().any(|c| c.is_flag());
        let confidence = checks
            .iter()
            .map(|c| match c {
                CheckOutcome::Pass { confidence } => *confidence,
                CheckOutcome::Flag { .. } => 0.0,
            })
            .fold(1.0f32, f32::min);

        let (verdict, rationale) = match (candidates.len(), any_flag) {
            (0, _) => (
                Verdict::Equivalent(vec![]),
                "no candidates".to_string(),
            ),
            (1, false) => (
                Verdict::Winner(ids[0].clone()),
                "sole candidate, all checks pass".to_string(),
            ),
            (1, true) => (
                Verdict::Ambiguous(ids.clone()),
                "sole candidate but a check flagged it — surface to agent".to_string(),
            ),
            (_, false) => (
                Verdict::Equivalent(ids.clone()),
                "multiple candidates, all clean".to_string(),
            ),
            (_, true) => (
                Verdict::Ambiguous(ids.clone()),
                "multiple candidates with disagreement/flags".to_string(),
            ),
        };

        AdjudicationRecord {
            candidates: ids.clone(),
            flagged: if any_flag { ids } else { vec![] },
            verdict,
            confidence,
            rationale,
        }
    }
}
