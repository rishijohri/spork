//! Spork P7 baselines & flaky handling (DESIGN.md §8.3, A.2).
//!
//! Regression comparison runs against explicit **baselines**, and flaky handling
//! is first-class so that false-blocking on flakes (or false-greening) never
//! erodes trust. This crate is the additive P7 consumer of the frozen F4
//! [`spork_runner::ResultEnvelope`] / metric contracts — it adds no schema to,
//! and reopens no contract of, the runner (CLAUDE.md C2/C3).
//!
//! # The surface
//!
//! - [`CorrectnessBaseline`] — a pinned expected-pass set; [`CorrectnessBaseline::compare`]
//!   diffs a fresh result into regressed / newly-passing / missing units.
//! - [`PerfBaseline`] — a pinned metric distribution with integer [`Tolerance`]s
//!   (basis points, never floats); [`PerfBaseline::regressions`] flags metrics
//!   that degraded beyond tolerance, and [`PerfBaseline::delta_for`] is the
//!   strict, error-on-missing variant a gate uses for a mandatory metric.
//! - [`Baseline`] — a named, pinnable (GC-protected, DESIGN.md A.2) bundle of
//!   both, content-addressable so it travels with the snapshot it certifies.
//! - [`FlakinessEngine`] — scores a unit's flakiness as its **unchanged-input
//!   flip rate**, the signal that separates a flake from a regression (DESIGN.md
//!   §8.3).
//! - [`RetryPolicy`] / [`RetryDecision`] — bounded auto-retry with quorum.
//! - [`QuarantineList`] — a time-boxed quarantine (excluded from gates, still
//!   recorded and surfaced).
//!
//! # Example: a perf regression caught against a pinned baseline
//!
//! ```
//! use spork_baseline::{Baseline, PerfBaseline, MetricBaseline, Tolerance};
//! use spork_runner::{Direction, Metric, Outcome, ResultEnvelope};
//! use spork_hash::Hash;
//! use ulid::Ulid;
//!
//! // Pin p99 at 1000 with a 10% (1000 bps) tolerance.
//! let perf = PerfBaseline::new().with_metric(
//!     "p99_latency_ms",
//!     MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
//! );
//! let baseline = Baseline::new("release-1").with_perf(perf.clone());
//! assert!(baseline.pinned); // pinned -> GC-protected (DESIGN A.2)
//!
//! // A post-merge re-run measures 1200 (20% worse) -> a regression.
//! let result = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0; 32]))
//!     .with_metric(Metric::new("p99_latency_ms", 1200, Direction::LowerBetter));
//! let regressions = perf.regressions(&result);
//! assert_eq!(regressions.len(), 1);
//! assert!(!regressions[0].within_tolerance);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod baseline;
mod correctness;
mod error;
mod flaky;
mod perf;

pub use baseline::{Baseline, BASELINE_VERSION};
pub use correctness::{CorrectnessBaseline, CorrectnessDelta, CORRECTNESS_BASELINE_VERSION};
pub use error::{BaselineError, Result};
pub use flaky::{
    FlakinessEngine, QuarantineEntry, QuarantineList, RetryDecision, RetryPolicy,
    QUARANTINE_LIST_VERSION,
};
pub use perf::{MetricBaseline, MetricDelta, PerfBaseline, Tolerance, PERF_BASELINE_VERSION};
