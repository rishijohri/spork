//! Flaky-test handling: the flakiness engine, bounded retry with quorum, and a
//! time-boxed quarantine list (DESIGN.md §8.3).
//!
//! False-blocking on flakes (or false-greening) destroys trust faster than
//! anything, so flaky handling is first-class. The signal that separates a flake
//! from a regression is **an outcome flip on unchanged relevant inputs**: a unit
//! that both passed and failed *at the same `input_digest`* flipped without its
//! relevant files changing — likely a flake — whereas a unit that flipped only
//! across *different* digests changed behaviour after a relevant edit — likely a
//! regression (DESIGN.md §8.3). The [`FlakinessEngine`] computes exactly that.
//!
//! Gates then support **bounded auto-retry with quorum** ([`RetryPolicy`]) and
//! can **quarantine** a unit (excluded from gates, still recorded). Quarantine
//! lists are surfaced and **time-boxed** so a genuine concurrency bug cannot hide
//! in an ever-growing quarantine (DESIGN.md §8.3).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use spork_runner::{ResultEnvelope, UnitStatus};

/// Per `input_digest`, whether a unit was ever seen passing and/or failing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OutcomeFlags {
    saw_pass: bool,
    saw_fail: bool,
}

impl OutcomeFlags {
    /// Whether this digest saw the unit flip between pass and fail — the
    /// unchanged-input flip that signals a flake.
    fn flipped(self) -> bool {
        self.saw_pass && self.saw_fail
    }
}

/// Accumulates per-unit outcome history across the DAG and scores flakiness
/// (DESIGN.md §8.3).
///
/// The engine is a pure accumulator: feed it observations (or whole result
/// envelopes tagged by their `input_digest`) and query a unit's flakiness. It
/// holds no clock and makes no I/O, so it is deterministic and replayable.
#[derive(Debug, Clone, Default)]
pub struct FlakinessEngine {
    /// unit -> (input_digest -> flags).
    units: BTreeMap<String, BTreeMap<Hash, OutcomeFlags>>,
}

impl FlakinessEngine {
    /// Construct an empty flakiness engine.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one unit outcome observed at a given input digest.
    pub fn record(&mut self, input_digest: Hash, unit: impl Into<String>, status: UnitStatus) {
        let entry = self
            .units
            .entry(unit.into())
            .or_default()
            .entry(input_digest)
            .or_default();
        match status {
            UnitStatus::Passed => entry.saw_pass = true,
            UnitStatus::Failed => entry.saw_fail = true,
            // Skips carry no pass/fail signal.
            UnitStatus::Skipped => {}
        }
    }

    /// Record every unit of a result envelope under the envelope's
    /// `input_digest` — the common ingestion path from the result store.
    pub fn record_envelope(&mut self, env: &ResultEnvelope) {
        for u in &env.units {
            self.record(env.input_digest, &u.name, u.status);
        }
    }

    /// The flakiness score of a unit, in basis points (0..=10000).
    ///
    /// It is the fraction of distinct input digests at which the unit *flipped*
    /// (saw both a pass and a fail) — the unchanged-input flip rate. `0` means
    /// the unit never flipped on an unchanged input (any failures are likely
    /// real regressions, not flakes); `10000` means it flipped at every digest.
    #[must_use]
    pub fn flakiness_bps(&self, unit: &str) -> u32 {
        let Some(by_digest) = self.units.get(unit) else {
            return 0;
        };
        let total = by_digest.len() as u64;
        if total == 0 {
            return 0;
        }
        let flipped = by_digest.values().filter(|f| f.flipped()).count() as u64;
        u32::try_from(flipped * 10_000 / total).unwrap_or(10_000)
    }

    /// Whether a unit's flakiness is at or above a basis-points threshold.
    #[must_use]
    pub fn is_likely_flake(&self, unit: &str, threshold_bps: u32) -> bool {
        self.flakiness_bps(unit) >= threshold_bps
    }
}

/// A bounded auto-retry policy with quorum (DESIGN.md §8.3).
///
/// A unit may be retried up to `max_attempts` times; its verdict is the majority
/// (`quorum`) outcome across the attempts seen so far. This converts a single
/// flaky failure into a quorum decision rather than an instant block, without
/// retrying forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// The maximum number of attempts allowed for a unit.
    pub max_attempts: u32,
    /// The number of identical outcomes required to reach a verdict.
    pub quorum: u32,
}

impl RetryPolicy {
    /// Construct a retry policy. `quorum` is clamped to at least 1 and at most
    /// `max_attempts`.
    #[must_use]
    pub fn new(max_attempts: u32, quorum: u32) -> Self {
        let max_attempts = max_attempts.max(1);
        RetryPolicy {
            max_attempts,
            quorum: quorum.clamp(1, max_attempts),
        }
    }

    /// Decide whether to keep retrying given the attempts observed so far.
    ///
    /// Returns [`RetryDecision::Pass`] / [`RetryDecision::Fail`] once `quorum`
    /// of one outcome is reached, [`RetryDecision::NeedMore`] while more attempts
    /// are allowed and no quorum is reached yet, and
    /// [`RetryDecision::Exhausted`] once `max_attempts` is reached without a
    /// quorum (the conservative answer — an unresolved unit does not silently
    /// pass).
    #[must_use]
    pub fn decide(&self, attempts: &[UnitStatus]) -> RetryDecision {
        let passes = attempts
            .iter()
            .filter(|s| **s == UnitStatus::Passed)
            .count() as u32;
        let fails = attempts
            .iter()
            .filter(|s| **s == UnitStatus::Failed)
            .count() as u32;
        if passes >= self.quorum {
            return RetryDecision::Pass;
        }
        if fails >= self.quorum {
            return RetryDecision::Fail;
        }
        if (attempts.len() as u32) >= self.max_attempts {
            return RetryDecision::Exhausted;
        }
        RetryDecision::NeedMore
    }
}

/// The outcome of a [`RetryPolicy::decide`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecision {
    /// Quorum of passes reached — the unit passes.
    Pass,
    /// Quorum of fails reached — the unit fails.
    Fail,
    /// More attempts are allowed and no quorum yet — retry.
    NeedMore,
    /// Attempts exhausted without a quorum — unresolved (does not silently pass).
    Exhausted,
}

/// The frozen schema version of the [`QuarantineList`] shape (CLAUDE.md C5).
pub const QUARANTINE_LIST_VERSION: u16 = 1;

/// One quarantined unit: why and until when (a millisecond timestamp).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineEntry {
    /// Why the unit was quarantined (surfaced in the UI — DESIGN.md §8.3).
    pub reason: String,
    /// The wall-clock millisecond after which the quarantine expires (time-boxed
    /// so a real bug cannot hide forever).
    pub until_ms: u64,
}

/// A time-boxed quarantine list (DESIGN.md §8.3).
///
/// A quarantined unit is excluded from gate evaluation but still recorded and
/// surfaced. Every entry carries an expiry so the list cannot grow unbounded.
/// The list holds no clock: callers pass `now_ms` to queries, keeping it
/// deterministic and canonical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineList {
    /// The schema version of this list (CLAUDE.md C5).
    pub schema_version: u16,
    /// Quarantined units, keyed by unit name (sorted/canonical).
    pub entries: BTreeMap<String, QuarantineEntry>,
}

impl Default for QuarantineList {
    fn default() -> Self {
        QuarantineList {
            schema_version: QUARANTINE_LIST_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl QuarantineList {
    /// Construct an empty quarantine list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Quarantine a unit with a reason, expiring at `until_ms`.
    pub fn quarantine(
        &mut self,
        unit: impl Into<String>,
        reason: impl Into<String>,
        until_ms: u64,
    ) {
        self.entries.insert(
            unit.into(),
            QuarantineEntry {
                reason: reason.into(),
                until_ms,
            },
        );
    }

    /// Whether a unit is currently quarantined at `now_ms` (expired entries do
    /// not count).
    #[must_use]
    pub fn is_quarantined(&self, unit: &str, now_ms: u64) -> bool {
        self.entries.get(unit).is_some_and(|e| e.until_ms > now_ms)
    }

    /// Remove expired entries (those whose `until_ms <= now_ms`), returning the
    /// units that were released.
    pub fn prune_expired(&mut self, now_ms: u64) -> Vec<String> {
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.until_ms <= now_ms)
            .map(|(u, _)| u.clone())
            .collect();
        for u in &expired {
            self.entries.remove(u);
        }
        expired
    }

    /// The currently-active (non-expired) quarantined units at `now_ms`, sorted.
    #[must_use]
    pub fn active(&self, now_ms: u64) -> Vec<(&str, &QuarantineEntry)> {
        self.entries
            .iter()
            .filter(|(_, e)| e.until_ms > now_ms)
            .map(|(u, e)| (u.as_str(), e))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    #[test]
    fn unchanged_input_flip_scores_as_flake() {
        let mut eng = FlakinessEngine::new();
        let d = hash_bytes(b"same-tree");
        // Same input digest, flipped pass<->fail: the flake signal.
        eng.record(d, "test::a", UnitStatus::Passed);
        eng.record(d, "test::a", UnitStatus::Failed);
        assert_eq!(eng.flakiness_bps("test::a"), 10_000);
        assert!(eng.is_likely_flake("test::a", 5_000));
    }

    #[test]
    fn fail_after_a_real_edit_is_not_a_flake() {
        let mut eng = FlakinessEngine::new();
        // Passed at one tree, failed at a *different* tree: a regression, not a
        // flake — neither digest flipped on its own.
        eng.record(hash_bytes(b"tree-1"), "test::b", UnitStatus::Passed);
        eng.record(hash_bytes(b"tree-2"), "test::b", UnitStatus::Failed);
        assert_eq!(eng.flakiness_bps("test::b"), 0);
        assert!(!eng.is_likely_flake("test::b", 1));
    }

    #[test]
    fn partial_flakiness_is_a_fraction() {
        let mut eng = FlakinessEngine::new();
        let flip = hash_bytes(b"flip");
        eng.record(flip, "t", UnitStatus::Passed);
        eng.record(flip, "t", UnitStatus::Failed);
        // A second, stable digest.
        eng.record(hash_bytes(b"stable"), "t", UnitStatus::Passed);
        // 1 of 2 digests flipped -> 5000 bps.
        assert_eq!(eng.flakiness_bps("t"), 5_000);
    }

    #[test]
    fn unknown_unit_is_not_flaky() {
        let eng = FlakinessEngine::new();
        assert_eq!(eng.flakiness_bps("nope"), 0);
    }

    #[test]
    fn record_envelope_ingests_all_units() {
        use spork_runner::{Outcome, ResultEnvelope, UnitResult};
        use ulid::Ulid;
        let d = hash_bytes(b"tree");
        let env = ResultEnvelope::new(Outcome::Failed, Ulid::new(), d)
            .with_unit(UnitResult::new("a", UnitStatus::Passed))
            .with_unit(UnitResult::new("a", UnitStatus::Failed));
        let mut eng = FlakinessEngine::new();
        eng.record_envelope(&env);
        assert_eq!(eng.flakiness_bps("a"), 10_000);
    }

    #[test]
    fn retry_reaches_pass_quorum() {
        let pol = RetryPolicy::new(5, 2);
        assert_eq!(pol.decide(&[]), RetryDecision::NeedMore);
        assert_eq!(
            pol.decide(&[UnitStatus::Failed, UnitStatus::Passed]),
            RetryDecision::NeedMore
        );
        assert_eq!(
            pol.decide(&[UnitStatus::Passed, UnitStatus::Passed]),
            RetryDecision::Pass
        );
    }

    #[test]
    fn retry_exhausts_without_quorum() {
        let pol = RetryPolicy::new(2, 2);
        // Two attempts, split — never reaches quorum 2, attempts exhausted.
        assert_eq!(
            pol.decide(&[UnitStatus::Passed, UnitStatus::Failed]),
            RetryDecision::Exhausted
        );
    }

    #[test]
    fn quorum_is_clamped_into_range() {
        let pol = RetryPolicy::new(3, 99);
        assert_eq!(pol.quorum, 3);
        let pol2 = RetryPolicy::new(0, 0);
        assert_eq!(pol2.max_attempts, 1);
        assert_eq!(pol2.quorum, 1);
    }

    #[test]
    fn quarantine_is_time_boxed() {
        let mut q = QuarantineList::new();
        q.quarantine("flappy::test", "flips on CI", 1_000);
        assert!(q.is_quarantined("flappy::test", 500));
        assert!(!q.is_quarantined("flappy::test", 1_000)); // expired at boundary
        assert!(!q.is_quarantined("flappy::test", 2_000));
    }

    #[test]
    fn prune_releases_expired_entries() {
        let mut q = QuarantineList::new();
        q.quarantine("a", "r", 100);
        q.quarantine("b", "r", 10_000);
        let released = q.prune_expired(500);
        assert_eq!(released, vec!["a".to_string()]);
        assert_eq!(q.active(500).len(), 1);
        assert_eq!(q.active(500)[0].0, "b");
    }

    #[test]
    fn quarantine_round_trips_and_canonicalizes() {
        let mut q = QuarantineList::new();
        q.quarantine("a", "reason", 5_000);
        let v = serde_json::to_value(&q).unwrap();
        let back: QuarantineList = serde_json::from_value(v).unwrap();
        assert_eq!(q, back);
        assert!(spork_canon::canonicalize(&q).is_ok());
    }
}
