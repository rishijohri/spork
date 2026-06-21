//! The declarative [`GatePolicy`] and its [`Severity`] / [`FlakyStrategy`]
//! (DESIGN.md §8.3).
//!
//! A gate is a declarative `GatePolicy`: a [`Transition`] it guards, a
//! structured [`Predicate`], a [`Severity`] (`block` | `warn`), and an
//! `on_flaky` strategy. Declarative policy is auditable and branch-scoped, and
//! the same policy can guard different transitions (DESIGN.md §8.3). Evaluating
//! it against a [`GateInput`] yields an immutable [`GateVerdict`].

use serde::{Deserialize, Serialize};

use crate::input::GateInput;
use crate::predicate::Predicate;
use crate::transition::Transition;
use crate::verdict::{Decision, GateVerdict};

/// The frozen schema version of the [`GatePolicy`] shape (CLAUDE.md C5).
pub const GATE_POLICY_SCHEMA_VERSION: u16 = 1;

/// How hard a failing predicate bites (DESIGN.md §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A failing predicate **blocks** the transition.
    Block,
    /// A failing predicate is surfaced as a **warning** but does not block.
    Warn,
}

/// How the gate treats a unit flagged as flaky (DESIGN.md §8.3).
///
/// The strategy is recorded on the policy; the daemon acts on it (quarantining
/// a unit, scheduling bounded retries via [`spork_baseline::RetryPolicy`]). It is
/// carried here so a policy fully describes its flaky behaviour. `#[non_exhaustive]`
/// so additional strategies are additive (CLAUDE.md C2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FlakyStrategy {
    /// Treat a flaky failure like any other failure (no special handling).
    Block,
    /// Quarantine the flaky unit (excluded from this gate, still recorded).
    Quarantine,
    /// Retry the unit under a bounded quorum policy before deciding.
    Retry,
}

/// A declarative quality gate (DESIGN.md §8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatePolicy {
    /// The schema version of this policy (CLAUDE.md C5).
    pub schema_version: u16,
    /// A stable identifier (recorded on the verdict).
    pub id: String,
    /// The transition this policy guards.
    pub transition: Transition,
    /// The structured predicate that must hold for the transition to pass.
    pub predicate: Predicate,
    /// How hard a failing predicate bites.
    pub severity: Severity,
    /// How a flaky unit is handled.
    pub on_flaky: FlakyStrategy,
}

impl GatePolicy {
    /// Construct a blocking gate policy with the default `Block` flaky strategy.
    #[must_use]
    pub fn new(id: impl Into<String>, transition: Transition, predicate: Predicate) -> Self {
        GatePolicy {
            schema_version: GATE_POLICY_SCHEMA_VERSION,
            id: id.into(),
            transition,
            predicate,
            severity: Severity::Block,
            on_flaky: FlakyStrategy::Block,
        }
    }

    /// Set the severity (builder style).
    #[must_use]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// Set the flaky strategy (builder style).
    #[must_use]
    pub fn with_on_flaky(mut self, on_flaky: FlakyStrategy) -> Self {
        self.on_flaky = on_flaky;
        self
    }

    /// Evaluate this policy against the input and produce an immutable verdict
    /// (DESIGN.md §8.3).
    ///
    /// A held predicate yields [`Decision::Pass`]. A failed predicate yields
    /// [`Decision::Blocked`] under [`Severity::Block`] or [`Decision::Warn`]
    /// under [`Severity::Warn`]. The verdict records the lineage hash of the
    /// snapshot the input was computed against, so it travels with the snapshot.
    #[must_use]
    pub fn evaluate(&self, input: &GateInput) -> GateVerdict {
        let eval = self.predicate.evaluate(input);
        let decision = if eval.satisfied {
            Decision::Pass
        } else {
            match self.severity {
                Severity::Block => Decision::Blocked,
                Severity::Warn => Decision::Warn,
            }
        };
        GateVerdict::new(
            &self.id,
            self.transition,
            decision,
            self.severity,
            eval.reasons,
            input.lineage_hash,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{EvaluatedResult, GateInput};
    use spork_baseline::{Baseline, MetricBaseline, PerfBaseline, Tolerance};
    use spork_hash::Hash;
    use spork_runner::{Direction, Metric, Outcome, ResultEnvelope};
    use ulid::Ulid;

    fn merge_gate() -> GatePolicy {
        GatePolicy::new(
            "merge-perf",
            Transition::Merge,
            Predicate::MetricWithinTolerance {
                metric: "p99_latency_ms".into(),
            },
        )
    }

    fn input_with_p99(value: i64) -> GateInput {
        let baseline = Baseline::new("rel-1").with_perf(PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        ));
        let env = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0; 32]))
            .with_metric(Metric::new("p99_latency_ms", value, Direction::LowerBetter));
        GateInput::new(
            vec![EvaluatedResult::new("stress", env)],
            Hash::from_bytes([9u8; 32]),
        )
        .with_baseline(baseline)
    }

    #[test]
    fn schema_version_frozen() {
        assert_eq!(GATE_POLICY_SCHEMA_VERSION, 1);
    }

    #[test]
    fn merge_passing_perf_yields_pass() {
        let v = merge_gate().evaluate(&input_with_p99(1050)); // 5% worse, within 10%
        assert_eq!(v.decision, Decision::Pass);
        assert!(v.allows_transition());
    }

    #[test]
    fn merge_regressing_p99_is_blocked() {
        // This is the P7 DoD: a merge that regresses p99 vs a pinned baseline is
        // blocked by a gate evaluating post-merge re-run results.
        let v = merge_gate().evaluate(&input_with_p99(1200)); // 20% worse
        assert_eq!(v.decision, Decision::Blocked);
        assert!(v.is_blocked());
        assert!(!v.allows_transition());
        assert_eq!(v.lineage_hash, Hash::from_bytes([9u8; 32]));
        assert!(v.reasons.iter().any(|r| r.contains("regressed")));
    }

    #[test]
    fn warn_severity_does_not_block() {
        let v = merge_gate()
            .with_severity(Severity::Warn)
            .evaluate(&input_with_p99(1200));
        assert_eq!(v.decision, Decision::Warn);
        assert!(v.allows_transition());
    }

    #[test]
    fn policy_round_trips_through_serde() {
        let p = merge_gate()
            .with_severity(Severity::Warn)
            .with_on_flaky(FlakyStrategy::Quarantine);
        let v = serde_json::to_value(&p).unwrap();
        let back: GatePolicy = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
        assert!(spork_canon::canonicalize(&p).is_ok());
    }
}
