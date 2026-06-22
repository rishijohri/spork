//! P7 quality-gate node type + helpers (DESIGN.md §8.3, A.4).
//!
//! A gated merge ([`Command::BranchMergeGated`](spork_ipc::Command::BranchMergeGated))
//! evaluates a [`GatePolicy`](spork_gates::GatePolicy) against the **post-merge**
//! re-run results and attaches an immutable **gate-verdict node** — a
//! [`Family::Observing`] node carrying the serialized
//! [`GateVerdict`](spork_gates::GateVerdict) in its payload, linked to the merge
//! node by a dotted [`DerivedFrom`](spork_graph::EdgeType::DerivedFrom) edge. The
//! verdict carries the merge snapshot's `lineage_hash`, so it **travels with the
//! snapshot** (DESIGN.md §8.3, needed by P9 graft). An override does not edit the
//! verdict in place — it produces a fresh `Overridden` verdict (recorded on the
//! same node payload), which is the visible audit (DESIGN.md §8.3). The node type
//! registers through the *same* public registry the P5 built-ins use (DESIGN.md
//! §7.1, §9).

use spork_graph::EdgeType;
use spork_registry::{Family, NodeTypeDescriptor, StalenessRule};

/// The built-in node kind a gate verdict attaches as.
pub const GATE_KIND: &str = "gate";

/// The semver the daemon stamps on the built-in gate node it creates.
pub(crate) const GATE_VERSION: &str = "1.0.0";

/// The built-in **gate** node type (DESIGN.md §8.3). A [`Family::Observing`] node
/// that owns **no** snapshot and observes the node it gated by a dotted
/// [`DerivedFrom`](EdgeType::DerivedFrom) edge.
///
/// Registered through the *public* registry path the P5 built-ins (and a P8
/// plugin) use — no built-in-only side door (DESIGN.md §7.1, §9).
pub(crate) fn gate_descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: GATE_KIND.to_string(),
        type_version: semver::Version::new(1, 0, 0),
        family: Family::Observing,
        owns_snapshot: false,
        payload_schema: serde_json::json!({ "type": "object" }),
        result_schema: None,
        // A gate verdict observes the node it gated (dotted), and may chain under
        // a parent for a multi-gate transition.
        allowed_edges: vec![EdgeType::DerivedFrom, EdgeType::ParentChild],
        ports: vec![],
        // A verdict is immutable and bound to the snapshot it was computed
        // against; it is never marked stale (a new state gets a new verdict).
        staleness_rule: StalenessRule::Never,
        capabilities_required: vec![],
        ui_contributions: serde_json::json!({
            "color": "#f59e0b", "icon": "gate", "displayName": "Gate"
        }),
        revoked_provenance: None,
    }
}

/// Parse a [`GateVerdict`](spork_gates::GateVerdict) out of a gate node's payload,
/// if present and well-formed.
///
/// The payload stores the verdict under a `"verdict"` key; a malformed or absent
/// verdict yields `None` (the view simply shows no gate badge) rather than an
/// error — surfacing the verdict is best-effort UI enrichment.
#[must_use]
pub(crate) fn verdict_from_payload(
    payload: &serde_json::Value,
) -> Option<spork_gates::GateVerdict> {
    let v = payload.get("verdict")?;
    serde_json::from_value(v.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_is_observing_and_owns_no_snapshot() {
        let d = gate_descriptor();
        assert_eq!(d.id, GATE_KIND);
        assert_eq!(d.family, Family::Observing);
        assert!(!d.owns_snapshot);
        assert!(d.allowed_edges.contains(&EdgeType::DerivedFrom));
    }

    #[test]
    fn verdict_round_trips_through_payload() {
        use spork_gates::{Decision, GateVerdict, Severity, Transition};
        let verdict = GateVerdict::new(
            "p1",
            Transition::Merge,
            Decision::Blocked,
            Severity::Block,
            vec!["p99 regressed".into()],
            spork_hash::Hash::from_bytes([3u8; 32]),
        );
        let payload = serde_json::json!({ "verdict": verdict });
        let back = verdict_from_payload(&payload).unwrap();
        assert_eq!(back.decision, Decision::Blocked);
        assert_eq!(back.policy_id, "p1");
    }

    #[test]
    fn missing_verdict_is_none() {
        assert!(verdict_from_payload(&serde_json::json!({})).is_none());
        assert!(verdict_from_payload(&serde_json::json!({ "verdict": 5 })).is_none());
    }
}
