//! The persisted result records of the restore guard: [`RestoreOutcome`],
//! [`ExternalEffect`], and [`RefId`].
//!
//! [`RestoreOutcome`] is what a successful [`restore`](crate::RestoreGuard::restore)
//! returns: the node that was restored, the code snapshot it materialized, the
//! conversation it resolved (or `None` if the node carried no bound
//! conversation), and a **shaped-but-empty** `external_effects` slot.
//!
//! That slot is the declared-additive **effects-log seam** (DESIGN §11.4 /
//! §18.3 Q5): restore moves *code + conversation*, never the world, so external
//! side effects — a row written to a shared DB, a pushed remote, an outbound
//! API call, paid-API spend — are *not* undoable. The seam is frozen here so a
//! v1 recorder (F4) can populate it without changing the outcome's shape; until
//! then the vector is always empty, and the schema-versioned record means older
//! outcomes stay readable as the seam grows (CLAUDE.md C5).
//!
//! Design references: DESIGN.md §6.4, §10.3, §11.4, §18.3 Q5.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use ulid::Ulid;

/// The schema version of [`RestoreOutcome`]. Bumped (with a registered
/// migration) whenever the persisted shape changes (CLAUDE.md C5).
pub const RESTORE_OUTCOME_SCHEMA_VERSION: u16 = 1;

/// The schema version of [`ExternalEffect`]. Independent of the outcome's own
/// version so the effects-log entry shape can evolve on its own cadence.
pub const EXTERNAL_EFFECT_SCHEMA_VERSION: u16 = 1;

/// The class of an out-of-restore-reach external side effect.
///
/// These mirror DESIGN §11.4's enumerated irreversible-state classes. The set is
/// `#[non_exhaustive]` so the v1 effects recorder (F4) can add classes additively
/// (CLAUDE.md C2) without breaking older serialized outcomes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEffectKind {
    /// A write to a shared/durable database that restore cannot roll back.
    SharedDbWrite,
    /// A branch/commit pushed to a remote that restore cannot un-push.
    RemotePush,
    /// An outbound network/API call whose effect persists outside the sandbox.
    OutboundApiCall,
    /// Paid-API spend (tokens, compute) that restore cannot refund.
    PaidApiSpend,
}

/// One recorded external side effect that a node performed and that restore
/// **cannot** undo (DESIGN §11.4).
///
/// This is the unit the effects-log seam carries. It exists so restore can warn
/// *truthfully* ("these effects were not and cannot be reverted") rather than
/// implying it rewound the world. The v1 recorder lands in F4; today the
/// [`RestoreOutcome::external_effects`] vector is always empty, but the type is
/// frozen now so that population is purely additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEffect {
    /// Schema version of this record (CLAUDE.md C5).
    pub schema_version: u16,
    /// The class of irreversible effect.
    pub kind: ExternalEffectKind,
    /// A human-readable description of the effect (e.g. the target URL or DB).
    pub detail: String,
}

impl ExternalEffect {
    /// Construct an effect record stamped at the current schema version.
    #[must_use]
    pub fn new(kind: ExternalEffectKind, detail: impl Into<String>) -> Self {
        ExternalEffect {
            schema_version: EXTERNAL_EFFECT_SCHEMA_VERSION,
            kind,
            detail: detail.into(),
        }
    }
}

/// The identity of a graph ref (a named pointer into the immutable DAG).
///
/// Refs are addressed by name in the F2 graph (`"HEAD"`, `"branch/feature-x"`,
/// tags); a `RefId` is that name as a distinct, typed handle so a ref name is not
/// confused with an arbitrary string. [`branch_fork`](crate::RestoreGuard::branch_fork)
/// returns one — the new branch ref it created without copying a byte.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RefId(pub String);

impl RefId {
    /// Borrow the ref name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RefId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for RefId {
    fn from(s: String) -> Self {
        RefId(s)
    }
}

impl From<&str> for RefId {
    fn from(s: &str) -> Self {
        RefId(s.to_string())
    }
}

/// The result of a successful atomic dual-restore.
///
/// A restore is transactional across **both** the code snapshot and the bound
/// conversation: this record is produced only after both were verified and the
/// snapshot materialized under one lock (DESIGN §10.3). It carries the
/// shaped-but-empty `external_effects` effects-log slot (DESIGN §11.4) so the
/// outcome can faithfully report — once the F4 recorder lands — any irreversible
/// side effects restore did *not* undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOutcome {
    /// Schema version of this outcome record (CLAUDE.md C5).
    pub schema_version: u16,
    /// The node that was restored.
    pub node_id: Ulid,
    /// The code snapshot that was materialized (the node's `snapshot_hash`).
    pub restored_snapshot: Hash,
    /// The bound conversation ref that was resolved, or `None` if the node
    /// carried no conversation (a snapshot/import node, say).
    pub restored_conversation: Option<Hash>,
    /// The effects-log slot (DESIGN §11.4 / §18.3 Q5): external side effects this
    /// node performed that restore could **not** undo. Shaped-but-empty in F3;
    /// the v1 recorder lands in F4 (CLAUDE.md C1 — a declared, documented seam,
    /// not a silent stub).
    pub external_effects: Vec<ExternalEffect>,
}

impl RestoreOutcome {
    /// Construct an outcome for a successful restore with an empty effects log.
    ///
    /// The effects-log slot is intentionally empty in F3 (the recorder is F4);
    /// it is part of the frozen shape so population is additive.
    #[must_use]
    pub fn new(
        node_id: Ulid,
        restored_snapshot: Hash,
        restored_conversation: Option<Hash>,
    ) -> Self {
        RestoreOutcome {
            schema_version: RESTORE_OUTCOME_SCHEMA_VERSION,
            node_id,
            restored_snapshot,
            restored_conversation,
            external_effects: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    #[test]
    fn outcome_new_stamps_version_and_empty_effects_log() {
        let id = Ulid::new();
        let snap = hash_bytes(b"code");
        let out = RestoreOutcome::new(id, snap, None);
        // The effects-log slot is shaped-but-empty in F3 (DESIGN §11.4).
        assert!(out.external_effects.is_empty());
        assert_eq!(out.schema_version, RESTORE_OUTCOME_SCHEMA_VERSION);
        assert_eq!(out.node_id, id);
        assert_eq!(out.restored_snapshot, snap);
        assert_eq!(out.restored_conversation, None);
    }

    #[test]
    fn outcome_serde_round_trips_with_and_without_conversation() {
        let id = Ulid::new();
        let snap = hash_bytes(b"code");
        let conv = hash_bytes(b"conv");

        for restored_conversation in [None, Some(conv)] {
            let out = RestoreOutcome::new(id, snap, restored_conversation);
            let json = serde_json::to_string(&out).unwrap();
            let back: RestoreOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(out, back);
        }
    }

    #[test]
    fn outcome_with_populated_effects_log_round_trips() {
        // Forward-compat: even though F3 never populates the slot, the shape must
        // serialize/deserialize so the F4 recorder is purely additive.
        let mut out = RestoreOutcome::new(Ulid::new(), hash_bytes(b"c"), None);
        out.external_effects.push(ExternalEffect::new(
            ExternalEffectKind::PaidApiSpend,
            "anthropic: 1200 tokens",
        ));
        out.external_effects.push(ExternalEffect::new(
            ExternalEffectKind::RemotePush,
            "origin/feature-x",
        ));
        let json = serde_json::to_string(&out).unwrap();
        let back: RestoreOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
        assert_eq!(back.external_effects.len(), 2);
    }

    #[test]
    fn external_effect_new_stamps_version() {
        let e = ExternalEffect::new(ExternalEffectKind::SharedDbWrite, "users table");
        assert_eq!(e.schema_version, EXTERNAL_EFFECT_SCHEMA_VERSION);
        assert_eq!(e.kind, ExternalEffectKind::SharedDbWrite);
        assert_eq!(e.detail, "users table");
    }

    #[test]
    fn external_effect_kind_serializes_snake_case() {
        let json = serde_json::to_string(&ExternalEffectKind::OutboundApiCall).unwrap();
        assert_eq!(json, "\"outbound_api_call\"");
        let back: ExternalEffectKind = serde_json::from_str("\"shared_db_write\"").unwrap();
        assert_eq!(back, ExternalEffectKind::SharedDbWrite);
    }

    #[test]
    fn ref_id_display_and_conversions() {
        let r = RefId::from("branch/feature-x");
        assert_eq!(r.as_str(), "branch/feature-x");
        assert_eq!(r.to_string(), "branch/feature-x");
        assert_eq!(RefId::from("a".to_string()), RefId("a".to_string()));
        // Serde round-trip (it is part of a persisted contract surface).
        let json = serde_json::to_string(&r).unwrap();
        let back: RefId = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
