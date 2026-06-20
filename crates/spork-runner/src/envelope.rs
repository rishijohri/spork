//! The normalization seam: [`ResultEnvelope`] and its parts — [`Outcome`],
//! [`UnitResult`], [`Violation`], [`ArtifactManifest`], [`ArtifactRef`].
//!
//! This is the load-bearing contract of the crate (DESIGN.md §8.1). Tests, perf
//! probes, and lint/sanity checks are not bespoke pipelines: every check kind is
//! a [`Runner`](crate::Runner) adapter that *normalizes* its raw output into one
//! `ResultEnvelope`, and then diffing, baselining, gates, and caching are
//! implemented once against that single shape — the core never branches on
//! whether a run was a test, a perf run, or a lint. The research locates the
//! real engineering cost exactly here: "the hard problem is not connectivity but
//! normalization."
//!
//! To carry all three kinds without leaking type specifics, the envelope is the
//! union of what each kind needs:
//!
//! - [`outcome`](ResultEnvelope::outcome) — the single pass/fail/error/skip
//!   verdict (every kind).
//! - [`units`](ResultEnvelope::units) — per-unit results (a test case, a lint
//!   file, a benchmark case): the test kind's pass/fail-per-unit lives here.
//! - [`metrics`](ResultEnvelope::metrics) — typed measurements with an
//!   optimization direction, each keyed by a registered metric id: the perf
//!   kind's p50/p99/throughput and a test kind's coverage live here.
//! - [`violations`](ResultEnvelope::violations) — `{rule_id, file, line,
//!   fixable}`: the lint/sanity kind's findings live here.
//! - [`artifact_manifest`](ResultEnvelope::artifact_manifest) — content-addressed
//!   references to bulky outputs (logs, coverage, perf traces, fuzz corpora)
//!   that are stored separately and lazily loaded (DESIGN.md §8.2).
//! - [`run_id`](ResultEnvelope::run_id) + [`input_digest`](ResultEnvelope::input_digest)
//!   — the append-only-history key: results are keyed by `(nodeId, runId,
//!   inputDigest)`, a re-run produces a new `run_id` rather than overwriting
//!   (DESIGN.md §7, §8.2), and the `input_digest` is what the derivation cache
//!   keys off (DESIGN.md §4.4).
//!
//! The struct carries its own [`schema_version`](ResultEnvelope::schema_version)
//! (constraint C5) and is canonically hashable so it content-addresses
//! identically on every machine, which is what lets two branches share a cached
//! result computed on a shared subtree.
//!
//! Design references: DESIGN.md §8.1 (one Runner SPI; the load-bearing
//! `ResultEnvelope`), §8.2 (small queryable envelope + bulky blob-addressed
//! artifacts), §7 (append-only `ResultArtifact`s keyed by `(nodeId, runId,
//! inputDigest)`), §4.4 (`inputDigest` / `derivationKey`).

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use spork_canon::canonicalize;
use spork_hash::{hash_bytes, Hash};

use crate::error::Result;
use crate::metric::Metric;

/// The current schema version stamped on a freshly constructed
/// [`ResultEnvelope`].
///
/// Persisted/hashed envelopes carry this so a future change to the result shape
/// is a loud, additive version bump rather than a silent reinterpretation
/// (constraint C5). The version participates in the envelope's content hash.
pub const RESULT_ENVELOPE_VERSION: u16 = 1;

/// The single pass/fail verdict every check kind reports.
///
/// Distinct from per-unit results: a test suite whose every case passed but
/// whose runner crashed collecting coverage is an [`Error`](Outcome::Error), not
/// a [`Passed`](Outcome::Passed) — the outcome is the gate-facing summary
/// (DESIGN.md §8.1, §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    /// The check ran and met its success condition.
    Passed,
    /// The check ran and did *not* meet its success condition (failing tests,
    /// lint violations present, perf regression).
    Failed,
    /// The check could not produce a verdict (the runner itself failed: a tool
    /// crashed, output was unparseable). Distinct from [`Failed`](Outcome::Failed).
    Error,
    /// The check was intentionally not run (e.g. no inputs intersected the
    /// change scope, or a precondition excluded it).
    Skipped,
}

impl Outcome {
    /// A short, stable label for diagnostics and logging.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Error => "error",
            Outcome::Skipped => "skipped",
        }
    }

    /// Whether this outcome should satisfy a "must pass" gate.
    #[must_use]
    pub const fn is_passing(&self) -> bool {
        matches!(self, Outcome::Passed)
    }
}

/// The status of a single unit within a check (a test case, a benchmark case, a
/// scanned file).
///
/// Mirrors [`Outcome`] at the unit granularity so the test kind's
/// pass/fail/skip-per-unit fits the same envelope without a test-specific
/// branch (DESIGN.md §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnitStatus {
    /// This unit passed.
    Passed,
    /// This unit failed.
    Failed,
    /// This unit was skipped.
    Skipped,
}

/// One per-unit result inside a check.
///
/// `name` is the unit's stable identifier (a fully-qualified test name, a file
/// path for a lint, a benchmark case). `detail` carries an optional short
/// human-readable note (a failure message, a skip reason); bulky output goes to
/// the [`ArtifactManifest`](ArtifactManifest), not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitResult {
    /// The unit's stable identifier.
    pub name: String,
    /// This unit's status.
    pub status: UnitStatus,
    /// An optional short note (failure message / skip reason). `None` when there
    /// is nothing to add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl UnitResult {
    /// Construct a unit result with no detail note.
    #[must_use]
    pub fn new(name: impl Into<String>, status: UnitStatus) -> Self {
        UnitResult {
            name: name.into(),
            status,
            detail: None,
        }
    }

    /// Attach a short detail note (builder style).
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A single rule violation found by a lint/sanity check.
///
/// The canonical lint finding shape `{rule_id, file, line, fixable}` from
/// DESIGN.md §8.1. `line` is 1-based; `None` means the violation is not tied to
/// a specific line (a whole-file or project-level rule). `fixable` records
/// whether an autofixer could repair it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    /// The id of the rule that was violated (e.g. `"forbid-pattern:FIXME"`,
    /// `"max-line-length"`).
    pub rule_id: String,
    /// The file the violation occurred in, relative to the worktree root.
    pub file: String,
    /// The 1-based line number, or `None` for a non-line-specific violation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Whether an autofixer could repair this violation.
    pub fixable: bool,
}

impl Violation {
    /// Construct a line-scoped violation.
    #[must_use]
    pub fn new(
        rule_id: impl Into<String>,
        file: impl Into<String>,
        line: u32,
        fixable: bool,
    ) -> Self {
        Violation {
            rule_id: rule_id.into(),
            file: file.into(),
            line: Some(line),
            fixable,
        }
    }

    /// Construct a violation not tied to a specific line.
    #[must_use]
    pub fn file_scoped(rule_id: impl Into<String>, file: impl Into<String>, fixable: bool) -> Self {
        Violation {
            rule_id: rule_id.into(),
            file: file.into(),
            line: None,
            fixable,
        }
    }
}

/// A content-addressed reference to one bulky artifact produced by a check.
///
/// The small queryable result lives in the [`ResultEnvelope`]; bulky outputs
/// (logs, coverage reports, perf traces, fuzz corpora) are stored separately,
/// content-addressed for cross-branch dedup, and referenced here by [`Hash`] so
/// they load lazily (DESIGN.md §8.2). `kind` is a free-form label
/// (`"stdout"`, `"junit-xml"`, `"coverage"`, `"perf-trace"`) and `bytes` records
/// the artifact's size for display without fetching it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// A label describing what this artifact is.
    pub kind: String,
    /// The content hash of the artifact's bytes in the object store.
    pub content_ref: Hash,
    /// The artifact's size in bytes (for display without fetching it).
    pub bytes: u64,
}

impl ArtifactRef {
    /// Construct an artifact reference.
    #[must_use]
    pub fn new(kind: impl Into<String>, content_ref: Hash, bytes: u64) -> Self {
        ArtifactRef {
            kind: kind.into(),
            content_ref,
            bytes,
        }
    }
}

/// The set of content-addressed artifacts a check produced.
///
/// Carries its own [`schema_version`](ArtifactManifest::schema_version) because
/// it is hashed into the envelope and may be persisted standalone (constraint
/// C5). Empty is a legitimate value — a sanity check with nothing bulky to
/// retain has an empty manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    /// The schema version of this manifest ([`ARTIFACT_MANIFEST_VERSION`] for
    /// fresh values).
    pub schema_version: u16,
    /// The artifacts, in a stable order.
    pub artifacts: Vec<ArtifactRef>,
}

/// The current schema version stamped on a freshly constructed
/// [`ArtifactManifest`].
pub const ARTIFACT_MANIFEST_VERSION: u16 = 1;

impl Default for ArtifactManifest {
    fn default() -> Self {
        ArtifactManifest {
            schema_version: ARTIFACT_MANIFEST_VERSION,
            artifacts: Vec::new(),
        }
    }
}

impl ArtifactManifest {
    /// Construct an empty manifest stamped with the current version.
    #[must_use]
    pub fn new() -> Self {
        ArtifactManifest::default()
    }

    /// Append an artifact reference (builder style).
    #[must_use]
    pub fn with_artifact(mut self, artifact: ArtifactRef) -> Self {
        self.artifacts.push(artifact);
        self
    }

    /// Whether the manifest has no artifacts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    /// The number of artifacts in the manifest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.artifacts.len()
    }
}

/// The one normalized result every check kind round-trips through.
///
/// See the [module docs](crate::envelope) for the full rationale. This is the
/// shape diffing, baselining, gates, and the derivation cache all key off, so
/// the core never branches on runner type. It is canonically hashable
/// ([`content_hash`](ResultEnvelope::content_hash)) and append-only by design:
/// a re-run yields a new [`run_id`](ResultEnvelope::run_id) rather than mutating
/// a stored envelope (DESIGN.md §7, §8.2).
///
/// # Example: the same struct expresses a lint, a test, and a perf result
/// ```
/// use spork_runner::{
///     ResultEnvelope, Outcome, UnitResult, UnitStatus, Violation, Metric,
///     Direction, ArtifactManifest,
/// };
/// use ulid::Ulid;
/// use spork_hash::Hash;
///
/// let digest = Hash::from_bytes([0u8; 32]);
///
/// // A lint-shaped result: violations populated, units/metrics light.
/// let lint = ResultEnvelope::new(Outcome::Failed, Ulid::new(), digest)
///     .with_violation(Violation::new("max-line-length", "src/a.rs", 12, false));
///
/// // A test-shaped result: per-unit pass/fail.
/// let tests = ResultEnvelope::new(Outcome::Passed, Ulid::new(), digest)
///     .with_unit(UnitResult::new("suite::case_a", UnitStatus::Passed));
///
/// // A perf-shaped result: typed metrics with a direction.
/// let perf = ResultEnvelope::new(Outcome::Passed, Ulid::new(), digest)
///     .with_metric(Metric::new("p99_latency_ms", 1200, Direction::LowerBetter));
///
/// // No core code branched on kind — they are the same type.
/// for env in [lint, tests, perf] {
///     let _ = env.content_hash().unwrap();
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultEnvelope {
    /// The schema version of this envelope ([`RESULT_ENVELOPE_VERSION`] for
    /// fresh values). Participates in the content hash.
    pub schema_version: u16,
    /// The single pass/fail/error/skip verdict.
    pub outcome: Outcome,
    /// Per-unit results (test cases, benchmark cases, scanned files).
    pub units: Vec<UnitResult>,
    /// Typed metrics with optimization directions, each keyed by a registered
    /// metric id.
    pub metrics: Vec<Metric>,
    /// Lint/sanity violations.
    pub violations: Vec<Violation>,
    /// Content-addressed references to bulky artifacts.
    pub artifact_manifest: ArtifactManifest,
    /// The id of *this* run. A re-run produces a new id (append-only history).
    pub run_id: Ulid,
    /// The derivation/cache key: a deterministic hash of the input tree, config,
    /// and runner version (DESIGN.md §4.4). Two runs with the same digest
    /// validated the same thing.
    pub input_digest: Hash,
}

impl ResultEnvelope {
    /// Construct an envelope with the given outcome, run id, and input digest,
    /// and empty unit/metric/violation/artifact collections.
    ///
    /// Stamps the current [`RESULT_ENVELOPE_VERSION`].
    #[must_use]
    pub fn new(outcome: Outcome, run_id: Ulid, input_digest: Hash) -> Self {
        ResultEnvelope {
            schema_version: RESULT_ENVELOPE_VERSION,
            outcome,
            units: Vec::new(),
            metrics: Vec::new(),
            violations: Vec::new(),
            artifact_manifest: ArtifactManifest::new(),
            run_id,
            input_digest,
        }
    }

    /// Append a per-unit result (builder style).
    #[must_use]
    pub fn with_unit(mut self, unit: UnitResult) -> Self {
        self.units.push(unit);
        self
    }

    /// Append a metric (builder style).
    #[must_use]
    pub fn with_metric(mut self, metric: Metric) -> Self {
        self.metrics.push(metric);
        self
    }

    /// Append a violation (builder style).
    #[must_use]
    pub fn with_violation(mut self, violation: Violation) -> Self {
        self.violations.push(violation);
        self
    }

    /// Replace the artifact manifest (builder style).
    #[must_use]
    pub fn with_artifact_manifest(mut self, manifest: ArtifactManifest) -> Self {
        self.artifact_manifest = manifest;
        self
    }

    /// Compute the content hash of this envelope **excluding the volatile
    /// `run_id`**.
    ///
    /// The cache and the dedup story compare results by *what they validated*,
    /// not by which run happened to produce them: two runs of the same
    /// `(spec, input_tree, runner)` differ only in their `run_id`, so the
    /// identity of a *result* must exclude it. This hashes a canonical view that
    /// omits `run_id`, so it is stable across re-runs and byte-identical on
    /// every machine.
    ///
    /// # Errors
    /// [`RunnerError::Canon`](crate::RunnerError::Canon) if canonicalization
    /// fails (only possible if a float entered the envelope, which the
    /// integer-only metric shape prevents).
    pub fn content_hash(&self) -> Result<Hash> {
        // Serialize without `run_id` so the result's identity is the *thing
        // validated*, not the run instance.
        #[derive(Serialize)]
        struct HashView<'a> {
            schema_version: u16,
            outcome: Outcome,
            units: &'a [UnitResult],
            metrics: &'a [Metric],
            violations: &'a [Violation],
            artifact_manifest: &'a ArtifactManifest,
            input_digest: &'a Hash,
        }
        let view = HashView {
            schema_version: self.schema_version,
            outcome: self.outcome,
            units: &self.units,
            metrics: &self.metrics,
            violations: &self.violations,
            artifact_manifest: &self.artifact_manifest,
            input_digest: &self.input_digest,
        };
        let bytes = canonicalize(&view)?;
        Ok(hash_bytes(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::Direction;

    fn digest() -> Hash {
        Hash::from_bytes([7u8; 32])
    }

    #[test]
    fn outcome_labels_and_passing() {
        assert_eq!(Outcome::Passed.label(), "passed");
        assert_eq!(Outcome::Failed.label(), "failed");
        assert_eq!(Outcome::Error.label(), "error");
        assert_eq!(Outcome::Skipped.label(), "skipped");
        assert!(Outcome::Passed.is_passing());
        assert!(!Outcome::Failed.is_passing());
        assert!(!Outcome::Error.is_passing());
        assert!(!Outcome::Skipped.is_passing());
    }

    #[test]
    fn empty_envelope_stamps_version() {
        let env = ResultEnvelope::new(Outcome::Passed, Ulid::new(), digest());
        assert_eq!(env.schema_version, RESULT_ENVELOPE_VERSION);
        assert!(env.units.is_empty());
        assert!(env.metrics.is_empty());
        assert!(env.violations.is_empty());
        assert!(env.artifact_manifest.is_empty());
    }

    #[test]
    fn lint_shaped_round_trips() {
        let env = ResultEnvelope::new(Outcome::Failed, Ulid::new(), digest())
            .with_violation(Violation::new("max-line-length", "src/a.rs", 12, false))
            .with_violation(Violation::file_scoped("no-todo", "README.md", false));
        let json = serde_json::to_string(&env).unwrap();
        let back: ResultEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.violations.len(), 2);
        assert_eq!(back.violations[1].line, None);
    }

    #[test]
    fn test_shaped_round_trips() {
        let env = ResultEnvelope::new(Outcome::Failed, Ulid::new(), digest())
            .with_unit(UnitResult::new("suite::a", UnitStatus::Passed))
            .with_unit(
                UnitResult::new("suite::b", UnitStatus::Failed).with_detail("expected 2 got 3"),
            )
            .with_unit(UnitResult::new("suite::c", UnitStatus::Skipped));
        let json = serde_json::to_string(&env).unwrap();
        let back: ResultEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.units.len(), 3);
        assert_eq!(back.units[1].detail.as_deref(), Some("expected 2 got 3"));
    }

    #[test]
    fn perf_shaped_round_trips() {
        let env = ResultEnvelope::new(Outcome::Passed, Ulid::new(), digest())
            .with_metric(Metric::new("p50_latency_ms", 40, Direction::LowerBetter))
            .with_metric(Metric::new("p99_latency_ms", 120, Direction::LowerBetter))
            .with_metric(Metric::new("throughput_rps", 9000, Direction::HigherBetter));
        let json = serde_json::to_string(&env).unwrap();
        let back: ResultEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.metrics.len(), 3);
    }

    #[test]
    fn all_three_kinds_are_the_same_type() {
        // The definition-of-done: a lint, a test, and a perf result are *one*
        // struct — this function takes `Vec<ResultEnvelope>` with no kind branch.
        fn summarize(envs: Vec<ResultEnvelope>) -> usize {
            envs.iter().filter(|e| e.outcome.is_passing()).count()
        }
        let d = digest();
        let lint = ResultEnvelope::new(Outcome::Failed, Ulid::new(), d)
            .with_violation(Violation::new("r", "f", 1, true));
        let tests = ResultEnvelope::new(Outcome::Passed, Ulid::new(), d)
            .with_unit(UnitResult::new("u", UnitStatus::Passed));
        let perf = ResultEnvelope::new(Outcome::Passed, Ulid::new(), d).with_metric(Metric::new(
            "p99_latency_ms",
            1,
            Direction::LowerBetter,
        ));
        assert_eq!(summarize(vec![lint, tests, perf]), 2);
    }

    #[test]
    fn content_hash_excludes_run_id() {
        let d = digest();
        let a = ResultEnvelope::new(Outcome::Passed, Ulid::new(), d);
        let b = ResultEnvelope::new(Outcome::Passed, Ulid::new(), d);
        // Different run ids...
        assert_ne!(a.run_id, b.run_id);
        // ...but identical *result identity*.
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn content_hash_changes_with_outcome() {
        let d = digest();
        let run = Ulid::new();
        let passed = ResultEnvelope::new(Outcome::Passed, run, d);
        let failed = ResultEnvelope::new(Outcome::Failed, run, d);
        assert_ne!(
            passed.content_hash().unwrap(),
            failed.content_hash().unwrap()
        );
    }

    #[test]
    fn artifact_manifest_builds() {
        let m = ArtifactManifest::new().with_artifact(ArtifactRef::new(
            "stdout",
            Hash::from_bytes([1; 32]),
            42,
        ));
        assert_eq!(m.len(), 1);
        assert!(!m.is_empty());
        assert_eq!(m.schema_version, ARTIFACT_MANIFEST_VERSION);
        let json = serde_json::to_string(&m).unwrap();
        let back: ArtifactManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
