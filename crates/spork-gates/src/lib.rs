//! Spork P7 quality gates (DESIGN.md §8.3, A.4).
//!
//! Gates are declarative [`GatePolicy`] objects evaluated by an engine that
//! writes an immutable [`GateVerdict`] onto a node or a transition. A rule is a
//! structured [`Predicate`] over the latest result envelopes and metric deltas
//! vs a [`spork_baseline::Baseline`], with a [`Severity`] (`block` | `warn`) and
//! an `on_flaky` strategy. Overrides are permitted but recorded as an audit node
//! so a failing gate is never silently bypassed. This crate is a pure, additive
//! P7 consumer of the frozen F4 [`spork_runner`] result contract and the P7
//! [`spork_baseline`] comparison — it reopens no contract (CLAUDE.md C2/C3).
//!
//! # The freeze-before surface (PLAN §9)
//!
//! - The [`Predicate`] grammar (the gate-predicate-grammar freeze) — a versioned,
//!   serializable AST evaluated **fail-closed** (an un-evaluable predicate blocks,
//!   never silently passes).
//! - [`GateVerdict`]-travels-with-snapshot: the verdict carries the
//!   `lineage_hash` of the snapshot it was computed against (needed by P9 graft,
//!   PLAN §9 D-12).
//!
//! # Example: a merge gate blocks a post-merge p99 regression (the P7 DoD)
//!
//! ```
//! use spork_gates::{GatePolicy, Transition, Predicate, Decision, GateOverride};
//! use spork_gates::{GateInput, EvaluatedResult};
//! use spork_baseline::{Baseline, PerfBaseline, MetricBaseline, Tolerance};
//! use spork_runner::{Direction, Metric, Outcome, ResultEnvelope};
//! use spork_hash::Hash;
//! use ulid::Ulid;
//!
//! // A merge gate: p99 must stay within the pinned baseline's tolerance.
//! let gate = GatePolicy::new(
//!     "merge-perf",
//!     Transition::Merge,
//!     Predicate::MetricWithinTolerance { metric: "p99_latency_ms".into() },
//! );
//!
//! // Post-merge stress re-run measures a 20% regression vs a 10%-tolerance baseline.
//! let baseline = Baseline::new("rel-1").with_perf(PerfBaseline::new().with_metric(
//!     "p99_latency_ms",
//!     MetricBaseline::new(1000, Direction::LowerBetter, Tolerance::bps(1000)),
//! ));
//! let post_merge = ResultEnvelope::new(Outcome::Passed, Ulid::new(), Hash::from_bytes([0; 32]))
//!     .with_metric(Metric::new("p99_latency_ms", 1200, Direction::LowerBetter));
//! let input = GateInput::new(vec![EvaluatedResult::new("stress", post_merge)], Hash::from_bytes([9; 32]))
//!     .with_baseline(baseline);
//!
//! let verdict = gate.evaluate(&input);
//! assert_eq!(verdict.decision, Decision::Blocked);   // the merge is blocked
//! assert!(!verdict.allows_transition());
//!
//! // An override is permitted but recorded (the daemon makes it an audit node).
//! let overridden = verdict.overridden(GateOverride::new("urgent hotfix", "alice"));
//! assert_eq!(overridden.decision, Decision::Overridden);
//! assert!(overridden.allows_transition());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod input;
mod policy;
mod predicate;
mod transition;
mod verdict;

pub use error::{GateError, Result};
pub use input::{EvaluatedResult, GateInput};
pub use policy::{FlakyStrategy, GatePolicy, Severity, GATE_POLICY_SCHEMA_VERSION};
pub use predicate::{Predicate, PredicateEval, PREDICATE_SCHEMA_VERSION};
pub use transition::Transition;
pub use verdict::{Decision, GateOverride, GateVerdict, GATE_VERDICT_SCHEMA_VERSION};
