//! The correctness baseline: a pinned expected-pass set and the delta a result
//! diffs to against it (DESIGN.md §8.3).
//!
//! A correctness baseline pins the set of unit names (test cases, scanned
//! files) that are expected to pass. Comparing a fresh
//! [`ResultEnvelope`](spork_runner::ResultEnvelope) to it surfaces the four
//! states the §8.3 Diff engine names — newly-failing (a *regression*),
//! newly-passing (a candidate addition), still-passing, and *missing* (an
//! expected unit the result never reported, so the gate could not verify it).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use spork_runner::{ResultEnvelope, UnitStatus};

/// The frozen schema version of the correctness-baseline shape (CLAUDE.md C5).
pub const CORRECTNESS_BASELINE_VERSION: u16 = 1;

/// A pinned set of unit names expected to pass (DESIGN.md §8.3).
///
/// The baseline is intentionally just the expected-pass *set*: it is small,
/// canonical (a sorted set), and content-addressable, so it travels with the
/// snapshot it certifies and dedups by hash. Whether it is pinned/GC-protected
/// is recorded on the wrapping [`Baseline`](crate::Baseline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectnessBaseline {
    /// The schema version of this baseline shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// The unit names expected to pass.
    pub expected_pass: BTreeSet<String>,
}

impl Default for CorrectnessBaseline {
    fn default() -> Self {
        CorrectnessBaseline {
            schema_version: CORRECTNESS_BASELINE_VERSION,
            expected_pass: BTreeSet::new(),
        }
    }
}

impl CorrectnessBaseline {
    /// Construct an empty correctness baseline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a baseline from the passing units of a result envelope.
    ///
    /// This is how a baseline is *pinned*: take a known-good run and record the
    /// units it passed as the expectation going forward (DESIGN.md §8.3).
    #[must_use]
    pub fn from_passing(env: &ResultEnvelope) -> Self {
        let expected_pass = env
            .units
            .iter()
            .filter(|u| u.status == UnitStatus::Passed)
            .map(|u| u.name.clone())
            .collect();
        CorrectnessBaseline {
            schema_version: CORRECTNESS_BASELINE_VERSION,
            expected_pass,
        }
    }

    /// Add a unit to the expected-pass set (builder style).
    #[must_use]
    pub fn with_expected(mut self, unit: impl Into<String>) -> Self {
        self.expected_pass.insert(unit.into());
        self
    }

    /// Compare a fresh result against this baseline (DESIGN.md §8.3).
    #[must_use]
    pub fn compare(&self, env: &ResultEnvelope) -> CorrectnessDelta {
        let mut passed_now: BTreeSet<&str> = BTreeSet::new();
        let mut failed_now: BTreeSet<&str> = BTreeSet::new();
        for u in &env.units {
            match u.status {
                UnitStatus::Passed => {
                    passed_now.insert(u.name.as_str());
                }
                // A skipped unit that was expected to pass is treated as not
                // verified (it lands in `missing`, below) — never as a silent
                // pass.
                UnitStatus::Failed => {
                    failed_now.insert(u.name.as_str());
                }
                UnitStatus::Skipped => {}
            }
        }

        let mut regressed = Vec::new();
        let mut missing = Vec::new();
        for unit in &self.expected_pass {
            if failed_now.contains(unit.as_str()) {
                regressed.push(unit.clone());
            } else if !passed_now.contains(unit.as_str()) {
                missing.push(unit.clone());
            }
        }
        let newly_passing = passed_now
            .iter()
            .filter(|u| !self.expected_pass.contains(**u))
            .map(|u| (*u).to_string())
            .collect();

        CorrectnessDelta {
            regressed,
            newly_passing,
            missing,
        }
    }
}

/// The result of comparing a fresh envelope to a [`CorrectnessBaseline`].
///
/// Vectors are sorted (the baseline iterates a `BTreeSet`) so the delta is
/// deterministic and canonical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CorrectnessDelta {
    /// Units that were expected to pass but now **failed** — the regressions a
    /// gate blocks on.
    pub regressed: Vec<String>,
    /// Units that passed but were not in the baseline — candidate additions.
    pub newly_passing: Vec<String>,
    /// Expected-pass units the result never reported as passing or failing — an
    /// evaluation gap (the gate could not verify them).
    pub missing: Vec<String>,
}

impl CorrectnessDelta {
    /// Whether the result regressed against the baseline (any expected-pass unit
    /// now fails). `missing` is reported separately so the caller can decide
    /// whether an unverified unit blocks.
    #[must_use]
    pub fn has_regression(&self) -> bool {
        !self.regressed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::Hash;
    use spork_runner::{Outcome, UnitResult};
    use ulid::Ulid;

    fn env(units: &[(&str, UnitStatus)]) -> ResultEnvelope {
        let mut e = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0u8; 32]));
        for (name, status) in units {
            e = e.with_unit(UnitResult::new(*name, *status));
        }
        e
    }

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(CORRECTNESS_BASELINE_VERSION, 1);
    }

    #[test]
    fn from_passing_records_only_passes() {
        let e = env(&[
            ("a", UnitStatus::Passed),
            ("b", UnitStatus::Failed),
            ("c", UnitStatus::Passed),
        ]);
        let b = CorrectnessBaseline::from_passing(&e);
        assert!(b.expected_pass.contains("a"));
        assert!(b.expected_pass.contains("c"));
        assert!(!b.expected_pass.contains("b"));
    }

    #[test]
    fn regression_is_an_expected_unit_now_failing() {
        let baseline = CorrectnessBaseline::new()
            .with_expected("a")
            .with_expected("b");
        let now = env(&[("a", UnitStatus::Passed), ("b", UnitStatus::Failed)]);
        let delta = baseline.compare(&now);
        assert!(delta.has_regression());
        assert_eq!(delta.regressed, vec!["b".to_string()]);
        assert!(delta.newly_passing.is_empty());
        assert!(delta.missing.is_empty());
    }

    #[test]
    fn newly_passing_unit_not_in_baseline_is_surfaced() {
        let baseline = CorrectnessBaseline::new().with_expected("a");
        let now = env(&[("a", UnitStatus::Passed), ("z", UnitStatus::Passed)]);
        let delta = baseline.compare(&now);
        assert!(!delta.has_regression());
        assert_eq!(delta.newly_passing, vec!["z".to_string()]);
    }

    #[test]
    fn expected_unit_absent_from_result_is_missing_not_regressed() {
        let baseline = CorrectnessBaseline::new()
            .with_expected("a")
            .with_expected("gone");
        let now = env(&[("a", UnitStatus::Passed)]);
        let delta = baseline.compare(&now);
        assert!(!delta.has_regression());
        assert_eq!(delta.missing, vec!["gone".to_string()]);
    }

    #[test]
    fn skipped_expected_unit_is_missing() {
        let baseline = CorrectnessBaseline::new().with_expected("a");
        let now = env(&[("a", UnitStatus::Skipped)]);
        let delta = baseline.compare(&now);
        assert_eq!(delta.missing, vec!["a".to_string()]);
        assert!(!delta.has_regression());
    }

    #[test]
    fn baseline_round_trips_through_serde_and_canon() {
        let b = CorrectnessBaseline::new()
            .with_expected("a")
            .with_expected("b");
        let v = serde_json::to_value(&b).unwrap();
        let back: CorrectnessBaseline = serde_json::from_value(v).unwrap();
        assert_eq!(b, back);
        assert!(spork_canon::canonicalize(&b).is_ok());
    }
}
