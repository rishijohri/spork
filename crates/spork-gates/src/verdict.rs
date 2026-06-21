//! The immutable, versioned [`GateVerdict`] that travels with the snapshot
//! (DESIGN.md §8.3; needed by P9 graft per PLAN §9 D-12).
//!
//! A gate engine writes an **immutable** `GateVerdict` onto the node/transition
//! it evaluated. The verdict records the policy, the transition, the decision,
//! the explaining reasons, and — crucially — the `lineage_hash` of the snapshot
//! it was computed against, so it **travels with that snapshot** into bundles
//! (P9) and a recipient can trust a verdict was computed against the exact state
//! it is attached to. Overrides are permitted but are themselves recorded (as a
//! [`GateOverride`] on the verdict, surfaced by the daemon as an audit node) so a
//! failing gate is never silently bypassed (DESIGN.md §8.3).

use serde::{Deserialize, Serialize};
use spork_hash::Hash;

use crate::error::{GateError, Result};
use crate::policy::Severity;
use crate::transition::Transition;

/// The frozen schema version of the [`GateVerdict`] shape (CLAUDE.md C5).
///
/// The verdict is content-addressable and travels in P9 bundles; versioning it
/// lets the verdict shape evolve additively without reinterpreting a verdict that
/// already traveled with a snapshot (CLAUDE.md C5, PLAN §9 D-12).
pub const GATE_VERDICT_SCHEMA_VERSION: u16 = 1;

/// The decision a gate reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// The predicate held — the transition is allowed.
    Pass,
    /// The predicate failed at `warn` severity — surfaced but not blocking.
    Warn,
    /// The predicate failed at `block` severity — the transition is blocked.
    Blocked,
    /// A blocking verdict that was explicitly overridden (the override is
    /// recorded on the verdict and surfaced as an audit node).
    Overridden,
}

impl Decision {
    /// Whether this decision permits the transition to proceed.
    #[must_use]
    pub const fn allows_transition(self) -> bool {
        matches!(self, Decision::Pass | Decision::Warn | Decision::Overridden)
    }
}

/// A recorded override of a blocking gate (DESIGN.md §8.3).
///
/// Permitted, but never silent: the daemon materializes this as a visible audit
/// node so the bypass is auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOverride {
    /// Why the gate was overridden.
    pub reason: String,
    /// Who/what overrode it (an operator id, or `"user"`).
    pub actor: String,
}

impl GateOverride {
    /// Construct an override record.
    #[must_use]
    pub fn new(reason: impl Into<String>, actor: impl Into<String>) -> Self {
        GateOverride {
            reason: reason.into(),
            actor: actor.into(),
        }
    }
}

/// An immutable, versioned gate verdict bound to the snapshot it evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateVerdict {
    /// The schema version of this verdict (CLAUDE.md C5).
    pub schema_version: u16,
    /// The id of the policy that produced this verdict.
    pub policy_id: String,
    /// The transition that was gated.
    pub transition: Transition,
    /// The decision reached.
    pub decision: Decision,
    /// The policy's severity (recorded so an overridden verdict still shows it
    /// *would* have blocked).
    pub severity: Severity,
    /// The explaining reasons from predicate evaluation.
    pub reasons: Vec<String>,
    /// The lineage hash of the snapshot this verdict was computed against — what
    /// makes the verdict travel with the snapshot (DESIGN.md §8.3, D-12).
    pub lineage_hash: Hash,
    /// The override record, if this verdict was overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_audit: Option<GateOverride>,
}

impl GateVerdict {
    /// Construct a verdict (used by [`GatePolicy::evaluate`](crate::GatePolicy::evaluate)).
    #[must_use]
    pub fn new(
        policy_id: impl Into<String>,
        transition: Transition,
        decision: Decision,
        severity: Severity,
        reasons: Vec<String>,
        lineage_hash: Hash,
    ) -> Self {
        GateVerdict {
            schema_version: GATE_VERDICT_SCHEMA_VERSION,
            policy_id: policy_id.into(),
            transition,
            decision,
            severity,
            reasons,
            lineage_hash,
            override_audit: None,
        }
    }

    /// Whether this verdict permits the transition to proceed.
    #[must_use]
    pub fn allows_transition(&self) -> bool {
        self.decision.allows_transition()
    }

    /// Whether this verdict blocks (a `Blocked` decision that has not been
    /// overridden).
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.decision == Decision::Blocked
    }

    /// Produce a new, overridden verdict from a blocking one (DESIGN.md §8.3).
    ///
    /// The original verdict is immutable; this returns a fresh verdict with
    /// [`Decision::Overridden`] and the override recorded. Overriding a
    /// non-blocked verdict is a no-op clone (there is nothing to override).
    #[must_use]
    pub fn overridden(&self, over: GateOverride) -> GateVerdict {
        if !self.is_blocked() {
            return self.clone();
        }
        let mut v = self.clone();
        v.decision = Decision::Overridden;
        v.reasons
            .push(format!("overridden by {}: {}", over.actor, over.reason));
        v.override_audit = Some(over);
        v
    }

    /// The content hash of this verdict over its canonical bytes (for dedup and
    /// to bind it immutably to its snapshot).
    ///
    /// # Errors
    ///
    /// Returns [`GateError::Canon`] only if the verdict somehow contains a float
    /// (the schema prevents it).
    pub fn content_hash(&self) -> Result<Hash> {
        let bytes = spork_canon::canonicalize(self).map_err(|e| GateError::Canon(e.to_string()))?;
        Ok(spork_hash::hash_bytes(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked() -> GateVerdict {
        GateVerdict::new(
            "p1",
            Transition::Merge,
            Decision::Blocked,
            Severity::Block,
            vec!["p99 regressed".into()],
            Hash::from_bytes([1u8; 32]),
        )
    }

    #[test]
    fn schema_version_frozen() {
        assert_eq!(GATE_VERDICT_SCHEMA_VERSION, 1);
    }

    #[test]
    fn blocked_does_not_allow_transition() {
        let v = blocked();
        assert!(v.is_blocked());
        assert!(!v.allows_transition());
    }

    #[test]
    fn override_produces_a_new_allowing_verdict_and_records_audit() {
        let v = blocked();
        let o = v.overridden(GateOverride::new("hotfix needed", "alice"));
        // Original is unchanged (immutable).
        assert_eq!(v.decision, Decision::Blocked);
        // New verdict allows the transition and records the override.
        assert_eq!(o.decision, Decision::Overridden);
        assert!(o.allows_transition());
        assert!(!o.is_blocked());
        assert_eq!(o.override_audit.as_ref().unwrap().actor, "alice");
        assert!(o.reasons.iter().any(|r| r.contains("overridden by alice")));
    }

    #[test]
    fn override_of_non_blocked_is_a_noop() {
        let pass = GateVerdict::new(
            "p1",
            Transition::Merge,
            Decision::Pass,
            Severity::Block,
            vec![],
            Hash::from_bytes([0u8; 32]),
        );
        let o = pass.overridden(GateOverride::new("x", "y"));
        assert_eq!(o.decision, Decision::Pass);
        assert!(o.override_audit.is_none());
    }

    #[test]
    fn verdict_carries_lineage_and_hashes_stably() {
        let v = blocked();
        assert_eq!(v.lineage_hash, Hash::from_bytes([1u8; 32]));
        assert_eq!(v.content_hash().unwrap(), blocked().content_hash().unwrap());
    }

    #[test]
    fn verdict_round_trips_through_serde() {
        let v = blocked().overridden(GateOverride::new("r", "a"));
        let val = serde_json::to_value(&v).unwrap();
        let back: GateVerdict = serde_json::from_value(val).unwrap();
        assert_eq!(v, back);
    }
}
