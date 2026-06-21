//! The performance baseline: a pinned metric distribution with integer
//! tolerances, and the metric deltas a result diffs to against it (DESIGN.md
//! §8.3).
//!
//! A perf baseline pins, per metric, an expected value, its optimization
//! [`Direction`](spork_runner::Direction), and a [`Tolerance`] — the allowed
//! regression expressed in **basis points** (1 bps = 0.01 %), an integer so the
//! whole baseline canonicalizes (the canonical encoder forbids floats). A fresh
//! result is a regression on a metric when its degradation exceeds the
//! tolerance (e.g. p99 latency more than 10 % worse than baseline → 1000 bps).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use spork_runner::{Direction, MetricId, ResultEnvelope};

use crate::error::{BaselineError, Result};

/// The frozen schema version of the perf-baseline shape (CLAUDE.md C5).
pub const PERF_BASELINE_VERSION: u16 = 1;

/// The allowed regression for a metric, in basis points (1 bps = 0.01 %).
///
/// Integer-only so it canonicalizes; `1000` means "up to 10 % worse than
/// baseline is tolerated". A tolerance of `0` blocks on any regression at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tolerance {
    /// The largest regression tolerated, in basis points (relative to baseline).
    pub max_regression_bps: u32,
}

impl Tolerance {
    /// Construct a tolerance from a basis-points allowance.
    #[must_use]
    pub fn bps(max_regression_bps: u32) -> Self {
        Tolerance { max_regression_bps }
    }

    /// A zero-tolerance: any regression at all is out of tolerance.
    #[must_use]
    pub fn strict() -> Self {
        Tolerance {
            max_regression_bps: 0,
        }
    }
}

/// One metric's pinned baseline: its expected value, direction, and tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricBaseline {
    /// The pinned baseline value (integer, with the metric's defined scale).
    pub value: i64,
    /// Which way this metric improves.
    pub direction: Direction,
    /// The regression tolerance.
    pub tolerance: Tolerance,
}

impl MetricBaseline {
    /// Construct a metric baseline.
    #[must_use]
    pub fn new(value: i64, direction: Direction, tolerance: Tolerance) -> Self {
        MetricBaseline {
            value,
            direction,
            tolerance,
        }
    }
}

/// A pinned perf distribution: per-metric baselines with tolerances (DESIGN.md
/// §8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerfBaseline {
    /// The schema version of this baseline shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// The pinned metric baselines, keyed by metric id (sorted/canonical).
    pub metrics: BTreeMap<MetricId, MetricBaseline>,
}

impl Default for PerfBaseline {
    fn default() -> Self {
        PerfBaseline {
            schema_version: PERF_BASELINE_VERSION,
            metrics: BTreeMap::new(),
        }
    }
}

impl PerfBaseline {
    /// Construct an empty perf baseline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin a metric baseline (builder style).
    #[must_use]
    pub fn with_metric(mut self, id: impl Into<MetricId>, baseline: MetricBaseline) -> Self {
        self.metrics.insert(id.into(), baseline);
        self
    }

    /// Compute the delta for one pinned metric against a fresh result.
    ///
    /// # Errors
    ///
    /// Returns [`BaselineError::MissingMetric`] if the result does not report the
    /// metric — surfaced rather than silently treated as "no regression".
    pub fn delta_for(&self, env: &ResultEnvelope, id: &MetricId) -> Result<MetricDelta> {
        let baseline = self
            .metrics
            .get(id)
            .ok_or_else(|| BaselineError::MissingMetric(id.as_str().to_string()))?;
        let observed = env
            .metrics
            .iter()
            .find(|m| &m.id == id)
            .ok_or_else(|| BaselineError::MissingMetric(id.as_str().to_string()))?;
        Ok(MetricDelta::compute(id.clone(), baseline, observed.value))
    }

    /// Every pinned metric present in the result that regressed beyond its
    /// tolerance, sorted by metric id.
    ///
    /// Metrics the result does not report are skipped here (see
    /// [`delta_for`](PerfBaseline::delta_for) for the strict, error-on-missing
    /// variant a gate uses when a specific metric is mandatory).
    #[must_use]
    pub fn regressions(&self, env: &ResultEnvelope) -> Vec<MetricDelta> {
        self.metrics
            .iter()
            .filter_map(|(id, baseline)| {
                env.metrics
                    .iter()
                    .find(|m| &m.id == id)
                    .map(|m| MetricDelta::compute(id.clone(), baseline, m.value))
            })
            .filter(|d| !d.within_tolerance)
            .collect()
    }
}

/// The result of comparing one observed metric to its pinned baseline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricDelta {
    /// Which metric this delta is for.
    pub metric: MetricId,
    /// The pinned baseline value.
    pub baseline: i64,
    /// The freshly observed value.
    pub observed: i64,
    /// The regression magnitude in basis points (0 = improvement or no change,
    /// positive = degradation relative to baseline). Saturates at [`i64::MAX`]
    /// when the baseline is zero and the metric degraded.
    pub regression_bps: i64,
    /// Whether the regression is within the metric's tolerance.
    pub within_tolerance: bool,
}

impl MetricDelta {
    /// Compute a delta from a baseline and an observed value.
    #[must_use]
    pub fn compute(metric: MetricId, baseline: &MetricBaseline, observed: i64) -> Self {
        let regression_bps = regression_bps(baseline.value, observed, baseline.direction);
        let within_tolerance = regression_bps <= i64::from(baseline.tolerance.max_regression_bps);
        MetricDelta {
            metric,
            baseline: baseline.value,
            observed,
            regression_bps,
            within_tolerance,
        }
    }
}

/// The relative regression of `observed` vs `baseline`, in basis points.
///
/// Returns `0` for an improvement or no change. For a degradation it is the
/// relative magnitude `degraded / |baseline| * 10000`; when the baseline is zero
/// any degradation is treated as an unbounded regression ([`i64::MAX`]).
/// Computed in `i128` so it never overflows for `i64` inputs.
fn regression_bps(baseline: i64, observed: i64, direction: Direction) -> i64 {
    let degraded: i128 = match direction {
        // Lower is better: a larger observed value is worse.
        Direction::LowerBetter => i128::from(observed) - i128::from(baseline),
        // Higher is better: a smaller observed value is worse.
        Direction::HigherBetter => i128::from(baseline) - i128::from(observed),
    };
    if degraded <= 0 {
        return 0;
    }
    let denom = i128::from(baseline).abs();
    if denom == 0 {
        return i64::MAX;
    }
    let bps = degraded * 10_000 / denom;
    bps.min(i128::from(i64::MAX)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::Hash;
    use spork_runner::{Metric, Outcome};
    use ulid::Ulid;

    fn env_with(id: &str, value: i64, dir: Direction) -> ResultEnvelope {
        ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0u8; 32]))
            .with_metric(Metric::new(id, value, dir))
    }

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(PERF_BASELINE_VERSION, 1);
    }

    #[test]
    fn latency_within_tolerance_is_not_a_regression() {
        // baseline 1000, observed 1050 = 5% worse, tolerance 10% (1000 bps).
        let baseline = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        );
        let env = env_with("p99_latency_ms", 1050, Direction::LowerBetter);
        let delta = delta_id(&baseline, &env, "p99_latency_ms");
        assert_eq!(delta.regression_bps, 500);
        assert!(delta.within_tolerance);
        assert!(baseline.regressions(&env).is_empty());
    }

    #[test]
    fn latency_beyond_tolerance_is_a_regression() {
        // baseline 1000, observed 1200 = 20% worse, tolerance 10%.
        let baseline = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        );
        let env = env_with("p99_latency_ms", 1200, Direction::LowerBetter);
        let delta = delta_id(&baseline, &env, "p99_latency_ms");
        assert_eq!(delta.regression_bps, 2000);
        assert!(!delta.within_tolerance);
        let regs = baseline.regressions(&env);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].metric.as_str(), "p99_latency_ms");
    }

    #[test]
    fn improvement_is_never_a_regression() {
        let baseline = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::strict()),
        );
        let env = env_with("p99_latency_ms", 800, Direction::LowerBetter);
        let delta = delta_id(&baseline, &env, "p99_latency_ms");
        assert_eq!(delta.regression_bps, 0);
        assert!(delta.within_tolerance);
    }

    #[test]
    fn higher_better_metric_regresses_when_it_drops() {
        // throughput baseline 5000, observed 4000 = 20% lower (worse).
        let baseline = PerfBaseline::new().with_metric(
            "throughput_rps",
            MetricBaseline::new(5000, Direction::HigherBetter, Tolerance::bps(500)),
        );
        let env = env_with("throughput_rps", 4000, Direction::HigherBetter);
        let delta = delta_id(&baseline, &env, "throughput_rps");
        assert_eq!(delta.regression_bps, 2000);
        assert!(!delta.within_tolerance);
    }

    #[test]
    fn zero_baseline_with_degradation_is_unbounded() {
        let baseline = PerfBaseline::new().with_metric(
            "errors",
            MetricBaseline::new(0, Direction::LowerBetter, Tolerance::bps(10_000)),
        );
        let env = env_with("errors", 5, Direction::LowerBetter);
        let delta = delta_id(&baseline, &env, "errors");
        assert_eq!(delta.regression_bps, i64::MAX);
        assert!(!delta.within_tolerance);
    }

    #[test]
    fn missing_metric_is_an_error() {
        let baseline = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        );
        let env = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0u8; 32]));
        let err = baseline
            .delta_for(&env, &MetricId::new("p99_latency_ms"))
            .unwrap_err();
        assert!(matches!(err, BaselineError::MissingMetric(_)));
    }

    #[test]
    fn baseline_round_trips_and_canonicalizes() {
        let baseline = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        );
        let v = serde_json::to_value(&baseline).unwrap();
        let back: PerfBaseline = serde_json::from_value(v).unwrap();
        assert_eq!(baseline, back);
        assert!(spork_canon::canonicalize(&baseline).is_ok());
    }

    fn delta_id(baseline: &PerfBaseline, env: &ResultEnvelope, id: &str) -> MetricDelta {
        baseline.delta_for(env, &MetricId::new(id)).unwrap()
    }
}
