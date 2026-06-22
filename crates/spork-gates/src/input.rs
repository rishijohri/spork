//! The [`GateInput`]: the latest results, baseline, and lineage a gate evaluates
//! against (DESIGN.md §8.3, A.4).

use std::collections::BTreeSet;

use spork_baseline::Baseline;
use spork_hash::Hash;
use spork_runner::ResultEnvelope;

/// One normalized result the gate sees, tagged with the check kind that produced
/// it.
///
/// The `kind` lets a predicate scope to a check family (e.g. `all(kind=='sanity')`
/// — DESIGN.md §8.3) without the gate engine knowing anything about runners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedResult {
    /// The check kind that produced this result (e.g. `"sanity"`, `"stress"`).
    pub kind: String,
    /// The normalized result envelope.
    pub envelope: ResultEnvelope,
}

impl EvaluatedResult {
    /// Construct an evaluated result.
    #[must_use]
    pub fn new(kind: impl Into<String>, envelope: ResultEnvelope) -> Self {
        EvaluatedResult {
            kind: kind.into(),
            envelope,
        }
    }
}

/// The full input to [`GatePolicy::evaluate`](crate::GatePolicy::evaluate).
///
/// For a merge gate these are the **post-merge re-run** results (DESIGN.md A.4),
/// the pinned [`Baseline`] regression comparison runs against, the set of units
/// currently quarantined (excluded from gate evaluation — DESIGN.md §8.3), and
/// the `lineage_hash` of the snapshot they were computed against — which the
/// resulting [`GateVerdict`](crate::GateVerdict) records so the verdict
/// **travels with the snapshot** (DESIGN.md §8.3, needed by P9 graft).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateInput {
    /// The latest results to evaluate (post-merge re-run for a merge gate).
    pub results: Vec<EvaluatedResult>,
    /// The pinned baseline regression comparison runs against, if any.
    pub baseline: Option<Baseline>,
    /// Units currently quarantined — excluded from gate evaluation (DESIGN.md
    /// §8.3).
    pub quarantined: BTreeSet<String>,
    /// The lineage hash of the snapshot the results were computed against.
    pub lineage_hash: Hash,
}

impl GateInput {
    /// Construct an input from results and the lineage hash they were computed
    /// against, with no baseline and nothing quarantined.
    #[must_use]
    pub fn new(results: Vec<EvaluatedResult>, lineage_hash: Hash) -> Self {
        GateInput {
            results,
            baseline: None,
            quarantined: BTreeSet::new(),
            lineage_hash,
        }
    }

    /// Attach a baseline (builder style).
    #[must_use]
    pub fn with_baseline(mut self, baseline: Baseline) -> Self {
        self.baseline = Some(baseline);
        self
    }

    /// Mark a unit as quarantined (builder style).
    #[must_use]
    pub fn with_quarantined(mut self, unit: impl Into<String>) -> Self {
        self.quarantined.insert(unit.into());
        self
    }
}
