//! The [`Baseline`] wrapper: a named, pinnable bundle of a correctness and a
//! perf baseline (DESIGN.md §8.3, A.2).
//!
//! A baseline is the explicit reference a gate's regression comparison runs
//! against. It bundles an optional [`CorrectnessBaseline`] (expected-pass set)
//! and an optional [`PerfBaseline`] (metric distribution + tolerances), carries
//! a stable `id`, and records whether it is **pinned** — pinned baselines are
//! GC-protected so a passing baseline is never evicted out from under the gates
//! that reference it (DESIGN.md A.2 root #4). It is content-addressable so it can
//! travel with the snapshot it certifies.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;

use crate::correctness::CorrectnessBaseline;
use crate::error::Result;
use crate::perf::PerfBaseline;

/// The frozen schema version of the [`Baseline`] shape (CLAUDE.md C5).
pub const BASELINE_VERSION: u16 = 1;

/// A named, pinnable bundle of a correctness and a perf baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    /// The schema version of this baseline bundle (CLAUDE.md C5).
    pub schema_version: u16,
    /// A stable identifier for this baseline (referenced by a gate policy).
    pub id: String,
    /// The correctness baseline (expected-pass set), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correctness: Option<CorrectnessBaseline>,
    /// The perf baseline (metric distribution + tolerances), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<PerfBaseline>,
    /// Whether this baseline is pinned (GC-protected — DESIGN.md A.2).
    pub pinned: bool,
}

impl Baseline {
    /// Construct an empty, **pinned** baseline with the given id.
    ///
    /// Pinned is the safe default: a baseline a gate references must survive GC,
    /// and the cost of an over-retained baseline is trivial (DESIGN.md A.2).
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Baseline {
            schema_version: BASELINE_VERSION,
            id: id.into(),
            correctness: None,
            perf: None,
            pinned: true,
        }
    }

    /// Attach a correctness baseline (builder style).
    #[must_use]
    pub fn with_correctness(mut self, correctness: CorrectnessBaseline) -> Self {
        self.correctness = Some(correctness);
        self
    }

    /// Attach a perf baseline (builder style).
    #[must_use]
    pub fn with_perf(mut self, perf: PerfBaseline) -> Self {
        self.perf = Some(perf);
        self
    }

    /// Set the pinned flag (builder style).
    #[must_use]
    pub fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    /// The content hash of this baseline over its canonical bytes — so it dedups
    /// and travels with the snapshot it certifies.
    ///
    /// # Errors
    ///
    /// Returns [`BaselineError::Canon`](crate::BaselineError::Canon) only if the
    /// baseline somehow contains a float (the integer-only schemas prevent it).
    pub fn content_hash(&self) -> Result<Hash> {
        let bytes = spork_canon::canonicalize(self)
            .map_err(|e| crate::error::BaselineError::Canon(e.to_string()))?;
        Ok(spork_hash::hash_bytes(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::{MetricBaseline, Tolerance};
    use spork_runner::Direction;

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(BASELINE_VERSION, 1);
    }

    #[test]
    fn new_baseline_is_pinned_by_default() {
        assert!(Baseline::new("b1").pinned);
    }

    #[test]
    fn baseline_bundles_both_and_round_trips() {
        let b = Baseline::new("release-1")
            .with_correctness(CorrectnessBaseline::new().with_expected("a"))
            .with_perf(PerfBaseline::new().with_metric(
                "p99_latency_ms",
                MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
            ))
            .pinned(false);
        assert!(!b.pinned);
        let v = serde_json::to_value(&b).unwrap();
        let back: Baseline = serde_json::from_value(v).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn content_hash_is_stable_and_dedups() {
        let a = Baseline::new("b").with_correctness(CorrectnessBaseline::new().with_expected("x"));
        let b = Baseline::new("b").with_correctness(CorrectnessBaseline::new().with_expected("x"));
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }
}
