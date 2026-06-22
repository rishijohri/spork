//! The structured [`Predicate`] grammar a gate evaluates (DESIGN.md §8.3).
//!
//! A gate rule is a **structured predicate** over the latest result envelopes and
//! metric deltas vs a baseline — the design's worked example is
//! `all(kind=='sanity').outcome=='passed' AND metric('p99_latency_ms').deltaVsBaseline <= 0.10`.
//! This module freezes that grammar as a serializable AST (the P7 freeze-before
//! requirement) and evaluates it **fail-closed**: a predicate that cannot be
//! evaluated (a missing metric, an absent baseline) is *not satisfied* and
//! records why, so a gate never silently certifies state it could not check
//! (DESIGN.md §8.3, §7.3).

use serde::{Deserialize, Serialize};
use spork_baseline::MetricDelta;
use spork_runner::{MetricId, Outcome};

use crate::input::GateInput;

/// The frozen schema version of the [`Predicate`] grammar (CLAUDE.md C5).
///
/// The predicate is persisted inside a [`GatePolicy`](crate::GatePolicy) and is
/// part of a [`GateVerdict`](crate::GateVerdict)'s provenance, so its shape is
/// versioned; new predicate kinds slot in additively (CLAUDE.md C3).
pub const PREDICATE_SCHEMA_VERSION: u16 = 1;

/// A structured gate predicate (DESIGN.md §8.3).
///
/// The leaf predicates reference the frozen [`spork_runner::ResultEnvelope`] /
/// metric contracts and the [`spork_baseline`] regression comparison; the
/// combinators (`And`/`Or`/`Not`) compose them. New leaves are additive
/// (CLAUDE.md C3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "predicate", content = "args", rename_all = "snake_case")]
pub enum Predicate {
    /// Every result of the given `kind` (or every result when `kind` is `None`)
    /// has [`Outcome::Passed`]. With no matching results this is **vacuously
    /// true** (there was nothing to fail) — the conservative default for an
    /// `all(...)` quantifier.
    AllPassed {
        /// Restrict to results of this check kind, or `None` for all results.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    /// No expected-pass unit (per the baseline's correctness set) regressed in
    /// the results. Quarantined units are excluded. Fail-closed if there is no
    /// correctness baseline.
    NoNewFailingUnits,
    /// The named metric's regression vs the perf baseline is within the
    /// baseline's own tolerance. Fail-closed if the baseline or the metric is
    /// absent.
    MetricWithinTolerance {
        /// The metric id.
        metric: String,
    },
    /// The named metric's regression vs the perf baseline is at most `max_bps`
    /// basis points (overriding the baseline's stored tolerance). Fail-closed if
    /// the baseline or the metric is absent.
    MetricRegressionAtMost {
        /// The metric id.
        metric: String,
        /// The maximum tolerated regression, in basis points.
        max_bps: u32,
    },
    /// The named metric's worst observed absolute value across the results is at
    /// most `max`. Needs no baseline. Fail-closed if the metric is absent.
    MetricAbsoluteAtMost {
        /// The metric id.
        metric: String,
        /// The maximum tolerated absolute value.
        max: i64,
    },
    /// All sub-predicates must hold.
    All(Vec<Predicate>),
    /// At least one sub-predicate must hold.
    Any(Vec<Predicate>),
    /// The sub-predicate must not hold.
    Not(Box<Predicate>),
    /// Always satisfied (identity for `All`).
    Always,
    /// Never satisfied (identity for `Any`).
    Never,
}

/// The outcome of evaluating a [`Predicate`].
///
/// `reasons` explains *why* the predicate did (or did not) hold, so a blocking
/// verdict is never mysterious (DESIGN.md §13.6 auditability stance applied to
/// gates).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredicateEval {
    /// Whether the predicate held.
    pub satisfied: bool,
    /// Human-readable reasons (failure explanations, or the satisfying facts).
    pub reasons: Vec<String>,
}

impl PredicateEval {
    fn ok(reason: impl Into<String>) -> Self {
        PredicateEval {
            satisfied: true,
            reasons: vec![reason.into()],
        }
    }
    fn fail(reason: impl Into<String>) -> Self {
        PredicateEval {
            satisfied: false,
            reasons: vec![reason.into()],
        }
    }
}

impl Predicate {
    /// Evaluate this predicate against the gate input (DESIGN.md §8.3).
    #[must_use]
    pub fn evaluate(&self, input: &GateInput) -> PredicateEval {
        match self {
            Predicate::Always => PredicateEval::ok("always"),
            Predicate::Never => PredicateEval::fail("never"),
            Predicate::AllPassed { kind } => eval_all_passed(input, kind.as_deref()),
            Predicate::NoNewFailingUnits => eval_no_new_failing(input),
            Predicate::MetricWithinTolerance { metric } => {
                eval_metric_within_tolerance(input, metric, None)
            }
            Predicate::MetricRegressionAtMost { metric, max_bps } => {
                eval_metric_within_tolerance(input, metric, Some(*max_bps))
            }
            Predicate::MetricAbsoluteAtMost { metric, max } => {
                eval_metric_absolute(input, metric, *max)
            }
            Predicate::All(subs) => eval_all(input, subs),
            Predicate::Any(subs) => eval_any(input, subs),
            Predicate::Not(inner) => {
                let e = inner.evaluate(input);
                PredicateEval {
                    satisfied: !e.satisfied,
                    reasons: e.reasons.into_iter().map(|r| format!("not({r})")).collect(),
                }
            }
        }
    }
}

fn eval_all(input: &GateInput, subs: &[Predicate]) -> PredicateEval {
    let mut reasons = Vec::new();
    let mut satisfied = true;
    for sub in subs {
        let e = sub.evaluate(input);
        if !e.satisfied {
            satisfied = false;
            reasons.extend(e.reasons);
        }
    }
    if satisfied {
        reasons.push("all sub-predicates held".into());
    }
    PredicateEval { satisfied, reasons }
}

fn eval_any(input: &GateInput, subs: &[Predicate]) -> PredicateEval {
    if subs.is_empty() {
        return PredicateEval::fail("any() over no sub-predicates");
    }
    let mut reasons = Vec::new();
    for sub in subs {
        let e = sub.evaluate(input);
        if e.satisfied {
            return PredicateEval::ok("a sub-predicate held");
        }
        reasons.extend(e.reasons);
    }
    PredicateEval {
        satisfied: false,
        reasons,
    }
}

fn eval_all_passed(input: &GateInput, kind: Option<&str>) -> PredicateEval {
    let matching: Vec<&crate::input::EvaluatedResult> = input
        .results
        .iter()
        .filter(|r| kind.is_none_or(|k| r.kind == k))
        .collect();
    let label = kind.unwrap_or("*");
    if matching.is_empty() {
        // Vacuously true: nothing of this kind to fail.
        return PredicateEval::ok(format!("no results of kind '{label}' (vacuously passed)"));
    }
    let failing: Vec<String> = matching
        .iter()
        .filter(|r| r.envelope.outcome != Outcome::Passed)
        .map(|r| format!("{}={}", r.kind, r.envelope.outcome.label()))
        .collect();
    if failing.is_empty() {
        PredicateEval::ok(format!("all {} '{label}' result(s) passed", matching.len()))
    } else {
        PredicateEval::fail(format!(
            "kind '{label}' not all passed: {}",
            failing.join(", ")
        ))
    }
}

fn eval_no_new_failing(input: &GateInput) -> PredicateEval {
    let Some(correctness) = input.baseline.as_ref().and_then(|b| b.correctness.as_ref()) else {
        return PredicateEval::fail("no correctness baseline to compare against (fail-closed)");
    };
    // Collect the units that failed across all results, excluding quarantined.
    let mut regressed: Vec<String> = Vec::new();
    for unit in &correctness.expected_pass {
        if input.quarantined.contains(unit) {
            continue;
        }
        let failed_somewhere = input.results.iter().any(|r| {
            r.envelope
                .units
                .iter()
                .any(|u| &u.name == unit && u.status == spork_runner::UnitStatus::Failed)
        });
        if failed_somewhere {
            regressed.push(unit.clone());
        }
    }
    if regressed.is_empty() {
        PredicateEval::ok("no expected-pass unit regressed")
    } else {
        PredicateEval::fail(format!("units regressed: {}", regressed.join(", ")))
    }
}

/// Find the worst (most-regressed) delta for a metric across all results.
fn worst_delta(input: &GateInput, metric: &str) -> std::result::Result<MetricDelta, String> {
    let Some(perf) = input.baseline.as_ref().and_then(|b| b.perf.as_ref()) else {
        return Err(format!(
            "no perf baseline for metric '{metric}' (fail-closed)"
        ));
    };
    let id = MetricId::new(metric);
    let mut worst: Option<MetricDelta> = None;
    for r in &input.results {
        if r.envelope.metrics.iter().any(|m| m.id == id) {
            // delta_for reads the metric from this envelope; evaluate per result.
            if let Ok(delta) = perf.delta_for(&r.envelope, &id) {
                worst = Some(match worst {
                    Some(w) if w.regression_bps >= delta.regression_bps => w,
                    _ => delta,
                });
            }
        }
    }
    worst.ok_or_else(|| format!("metric '{metric}' absent from results or baseline (fail-closed)"))
}

fn eval_metric_within_tolerance(
    input: &GateInput,
    metric: &str,
    override_bps: Option<u32>,
) -> PredicateEval {
    match worst_delta(input, metric) {
        Err(why) => PredicateEval::fail(why),
        Ok(delta) => {
            let within = match override_bps {
                Some(max) => delta.regression_bps <= i64::from(max),
                None => delta.within_tolerance,
            };
            if within {
                PredicateEval::ok(format!(
                    "metric '{metric}' regression {}bps within tolerance",
                    delta.regression_bps
                ))
            } else {
                PredicateEval::fail(format!(
                    "metric '{metric}' regressed {}bps (baseline {} -> observed {})",
                    delta.regression_bps, delta.baseline, delta.observed
                ))
            }
        }
    }
}

fn eval_metric_absolute(input: &GateInput, metric: &str, max: i64) -> PredicateEval {
    let id = MetricId::new(metric);
    // The worst absolute observation across results is the largest value (the
    // predicate is an upper bound, so the maximum is the binding one).
    let mut worst: Option<i64> = None;
    for r in &input.results {
        for m in &r.envelope.metrics {
            if m.id == id {
                worst = Some(worst.map_or(m.value, |w: i64| w.max(m.value)));
            }
        }
    }
    match worst {
        None => PredicateEval::fail(format!(
            "metric '{metric}' absent from results (fail-closed)"
        )),
        Some(v) if v <= max => PredicateEval::ok(format!("metric '{metric}'={v} <= {max}")),
        Some(v) => PredicateEval::fail(format!("metric '{metric}'={v} exceeds max {max}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_baseline::{Baseline, MetricBaseline, PerfBaseline, Tolerance};
    use spork_hash::Hash;
    use spork_runner::{Direction, Metric, ResultEnvelope, UnitResult, UnitStatus};
    use ulid::Ulid;

    use crate::input::{EvaluatedResult, GateInput};

    fn lineage() -> Hash {
        Hash::from_bytes([7u8; 32])
    }

    fn result(kind: &str, outcome: Outcome) -> EvaluatedResult {
        EvaluatedResult::new(
            kind,
            ResultEnvelope::new(outcome, Ulid::new(), Hash::from_bytes([0u8; 32])),
        )
    }

    #[test]
    fn schema_version_frozen() {
        assert_eq!(PREDICATE_SCHEMA_VERSION, 1);
    }

    #[test]
    fn all_passed_holds_when_every_matching_result_passed() {
        let input = GateInput::new(
            vec![
                result("sanity", Outcome::Passed),
                result("stress", Outcome::Failed),
            ],
            lineage(),
        );
        let p = Predicate::AllPassed {
            kind: Some("sanity".into()),
        };
        assert!(p.evaluate(&input).satisfied);
        // But "all results" fails because stress failed.
        assert!(
            !Predicate::AllPassed { kind: None }
                .evaluate(&input)
                .satisfied
        );
    }

    #[test]
    fn all_passed_is_vacuously_true_with_no_matching_results() {
        let input = GateInput::new(vec![result("stress", Outcome::Failed)], lineage());
        let p = Predicate::AllPassed {
            kind: Some("sanity".into()),
        };
        assert!(p.evaluate(&input).satisfied);
    }

    #[test]
    fn metric_within_tolerance_uses_baseline_tolerance() {
        let perf = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        );
        let baseline = Baseline::new("b").with_perf(perf);
        // 1200 = 20% worse, tolerance 10% -> regression.
        let env = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0; 32]))
            .with_metric(Metric::new("p99_latency_ms", 1200, Direction::LowerBetter));
        let input = GateInput::new(vec![EvaluatedResult::new("stress", env)], lineage())
            .with_baseline(baseline);
        let p = Predicate::MetricWithinTolerance {
            metric: "p99_latency_ms".into(),
        };
        let e = p.evaluate(&input);
        assert!(!e.satisfied, "20% regression must violate a 10% tolerance");
        assert!(e.reasons[0].contains("regressed"));
    }

    #[test]
    fn metric_missing_is_fail_closed() {
        let baseline = Baseline::new("b").with_perf(PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        ));
        // The result reports no metrics at all.
        let input = GateInput::new(vec![result("stress", Outcome::Passed)], lineage())
            .with_baseline(baseline);
        let p = Predicate::MetricWithinTolerance {
            metric: "p99_latency_ms".into(),
        };
        assert!(
            !p.evaluate(&input).satisfied,
            "absent metric must fail closed"
        );
    }

    #[test]
    fn no_baseline_is_fail_closed() {
        let input = GateInput::new(vec![result("stress", Outcome::Passed)], lineage());
        let p = Predicate::MetricWithinTolerance {
            metric: "p99_latency_ms".into(),
        };
        assert!(!p.evaluate(&input).satisfied);
        let p2 = Predicate::NoNewFailingUnits;
        assert!(!p2.evaluate(&input).satisfied);
    }

    #[test]
    fn no_new_failing_units_excludes_quarantined() {
        let baseline = Baseline::new("b")
            .with_correctness(spork_baseline::CorrectnessBaseline::new().with_expected("flappy"));
        let env = ResultEnvelope::new(Outcome::Failed, Ulid::new(), Hash::from_bytes([0; 32]))
            .with_unit(UnitResult::new("flappy", UnitStatus::Failed));
        // Without quarantine: regression.
        let input = GateInput::new(
            vec![EvaluatedResult::new("validation", env.clone())],
            lineage(),
        )
        .with_baseline(baseline.clone());
        assert!(!Predicate::NoNewFailingUnits.evaluate(&input).satisfied);
        // With "flappy" quarantined: excluded -> holds.
        let input2 = GateInput::new(vec![EvaluatedResult::new("validation", env)], lineage())
            .with_baseline(baseline)
            .with_quarantined("flappy");
        assert!(Predicate::NoNewFailingUnits.evaluate(&input2).satisfied);
    }

    #[test]
    fn and_combines_two_leaves() {
        let perf = PerfBaseline::new().with_metric(
            "p99_latency_ms",
            MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
        );
        let baseline = Baseline::new("b").with_perf(perf);
        let env = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0; 32]))
            .with_metric(Metric::new("p99_latency_ms", 1050, Direction::LowerBetter));
        let input = GateInput::new(vec![EvaluatedResult::new("stress", env)], lineage())
            .with_baseline(baseline);
        let p = Predicate::All(vec![
            Predicate::AllPassed { kind: None },
            Predicate::MetricWithinTolerance {
                metric: "p99_latency_ms".into(),
            },
        ]);
        assert!(p.evaluate(&input).satisfied);
    }

    #[test]
    fn not_inverts() {
        let input = GateInput::new(vec![], lineage());
        assert!(
            Predicate::Not(Box::new(Predicate::Never))
                .evaluate(&input)
                .satisfied
        );
        assert!(
            !Predicate::Not(Box::new(Predicate::Always))
                .evaluate(&input)
                .satisfied
        );
    }

    #[test]
    fn predicate_round_trips_through_serde() {
        let p = Predicate::All(vec![
            Predicate::AllPassed {
                kind: Some("sanity".into()),
            },
            Predicate::MetricRegressionAtMost {
                metric: "p99_latency_ms".into(),
                max_bps: 1000,
            },
        ]);
        let v = serde_json::to_value(&p).unwrap();
        let back: Predicate = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }
}
