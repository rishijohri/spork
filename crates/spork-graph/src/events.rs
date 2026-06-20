//! The frozen graph **event** vocabulary and its typed payloads.
//!
//! The typed graph is a *pure projection* of the F1 event log: every graph
//! mutation is an immutable [`Event`](spork_log::Event) appended through the F1
//! single-writer actor, and the [`GraphProjection`](crate::GraphProjection)
//! folds those events (DESIGN §6.1, §6.2). This module owns the **frozen**
//! `event_type` strings and the strongly-typed payload shapes that serialize
//! into each event's JSON body.
//!
//! # Frozen `event_type` strings (CLAUDE.md C2 — no domino)
//!
//! The five event-type discriminators are part of the persisted contract and are
//! never changed in place:
//!
//! | constant | `event_type` | payload |
//! |---|---|---|
//! | [`EVENT_NODE_CREATED`] | `graph.node_created` | [`NodeCreatedPayload`] |
//! | [`EVENT_EDGE_ADDED`] | `graph.edge_added` | [`EdgeAddedPayload`] |
//! | [`EVENT_REF_CREATED`] | `graph.ref_created` | [`RefCreatedPayload`] |
//! | [`EVENT_REF_MOVED`] | `graph.ref_moved` | [`RefMovedPayload`] |
//! | [`EVENT_NODE_STATUS_CHANGED`] | `graph.node_status_changed` | [`NodeStatusChangedPayload`] |
//!
//! # Schema versioning (CLAUDE.md C5)
//!
//! Each payload is appended at [`GRAPH_EVENT_SCHEMA_VERSION`]. A node payload
//! additionally carries its own `payload_schema_version` so a *node type's*
//! payload can evolve independently of the envelope and be upgraded lazily on
//! read via `spork-migrate` (DESIGN §7.2).
//!
//! ULIDs and hashes serialize through their own stable string forms (`ulid`'s
//! Crockford base32 and `spork-hash`'s lowercase hex), so the canonical bytes
//! the F1 hash chain commits to are byte-stable across machines.

use serde::{Deserialize, Serialize};
use spork_edges::{EdgeType, RefKind};
use spork_hash::Hash;
use spork_registry::Family;
use spork_status::Lifecycle;
use ulid::Ulid;

/// The schema version every graph event payload is appended under (CLAUDE.md
/// C5). Distinct from a node's `payload_schema_version`, which versions the
/// type-specific payload nested inside a `graph.node_created` event.
pub const GRAPH_EVENT_SCHEMA_VERSION: u16 = 1;

/// `event_type` for a node creation. Frozen (CLAUDE.md C2).
pub const EVENT_NODE_CREATED: &str = "graph.node_created";
/// `event_type` for an edge addition. Frozen (CLAUDE.md C2).
pub const EVENT_EDGE_ADDED: &str = "graph.edge_added";
/// `event_type` for a ref creation. Frozen (CLAUDE.md C2).
pub const EVENT_REF_CREATED: &str = "graph.ref_created";
/// `event_type` for a ref move. Frozen (CLAUDE.md C2).
pub const EVENT_REF_MOVED: &str = "graph.ref_moved";
/// `event_type` for a node status / staleness change. Frozen (CLAUDE.md C2).
pub const EVENT_NODE_STATUS_CHANGED: &str = "graph.node_status_changed";

/// Payload of a `graph.node_created` event (DESIGN §6.2, §7.2, A.1).
///
/// Carries everything the projection needs to materialize a new node row: its
/// identity and kind, the resolved type version, its [`Family`], its parents and
/// branch, snapshot ownership and (if owned) the snapshot hash, the type-specific
/// payload, the payload schema version, the computed lineage hash, and optional
/// model attribution. `family` is the value the command layer resolved from the
/// node-type descriptor at create time, so the projection folds the *authoritative*
/// family rather than re-deriving it from the kind string (which would diverge for
/// any custom kind). The projection derives `child_ids` (from the inverse parent
/// relation) rather than storing it redundantly here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCreatedPayload {
    /// The new node's id.
    pub node_id: Ulid,
    /// The node `kind` (registry type id).
    pub kind: String,
    /// The node [`Family`], as resolved from the type descriptor by the command
    /// layer. Persisted so the projection materializes the authoritative family
    /// without a fold-time registry lookup or a kind-string heuristic.
    pub family: Family,
    /// The resolved semver type version (rendered as its string form).
    pub type_version: String,
    /// The direct parents, in order.
    pub parent_ids: Vec<Ulid>,
    /// The branch the node is created on.
    pub branch_id: String,
    /// Whether this node owns a snapshot.
    pub owns_snapshot: bool,
    /// The snapshot hash — present iff `owns_snapshot`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub snapshot_hash: Option<Hash>,
    /// The type-specific payload (JSON-schema-validated per type).
    pub payload: serde_json::Value,
    /// The schema version the `payload` conforms to.
    pub payload_schema_version: u16,
    /// The lineage hash computed by the command layer (DESIGN §6.3).
    pub lineage_hash: Hash,
    /// The model used to produce the node, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub model: Option<String>,
}

/// Payload of a `graph.edge_added` event (DESIGN §6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeAddedPayload {
    /// The tail node (the would-be descendant for `PARENT_CHILD`).
    pub from: Ulid,
    /// The head node (the would-be ancestor for `PARENT_CHILD`).
    pub to: Ulid,
    /// The typed edge relation.
    pub edge_type: EdgeType,
}

/// Payload of a `graph.ref_created` event (DESIGN §6.3).
///
/// Refs (`HEAD`, `branch/*`, tags) are the mutable GC roots that point into the
/// immutable graph; creating one is itself an event so ref history is
/// recoverable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefCreatedPayload {
    /// The ref name (e.g. `"HEAD"`, `"branch/feature-x"`).
    #[serde(rename = "ref")]
    pub name: String,
    /// The ref kind.
    pub kind: RefKind,
    /// The node the ref initially points at.
    pub to: Ulid,
}

/// Payload of a `graph.ref_moved` event (DESIGN §6.3).
///
/// A ref move is recorded as a distinct event so pointer history is recoverable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefMovedPayload {
    /// The ref name being moved.
    #[serde(rename = "ref")]
    pub name: String,
    /// The node the ref now points at.
    pub to: Ulid,
}

/// Payload of a `graph.node_status_changed` event (DESIGN §7.3).
///
/// Carries the new lifecycle status and the orthogonal staleness flag (with an
/// optional reason). Status and staleness move on separate axes, so a single
/// event can update either or both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeStatusChangedPayload {
    /// The node whose status/staleness changed.
    pub node_id: Ulid,
    /// The new lifecycle status.
    pub status: Lifecycle,
    /// The new staleness flag.
    pub is_stale: bool,
    /// The reason for staleness, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stale_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frozen_event_type_strings() {
        // These strings are part of the persisted contract (CLAUDE.md C2). A
        // change here is a deliberate generation bump, never a silent edit.
        assert_eq!(EVENT_NODE_CREATED, "graph.node_created");
        assert_eq!(EVENT_EDGE_ADDED, "graph.edge_added");
        assert_eq!(EVENT_REF_CREATED, "graph.ref_created");
        assert_eq!(EVENT_REF_MOVED, "graph.ref_moved");
        assert_eq!(EVENT_NODE_STATUS_CHANGED, "graph.node_status_changed");
    }

    #[test]
    fn node_created_payload_round_trips() {
        let id = Ulid::new();
        let p = NodeCreatedPayload {
            node_id: id,
            kind: "snapshot".into(),
            family: Family::Mutating,
            type_version: "1.0.0".into(),
            parent_ids: vec![Ulid::new()],
            branch_id: "main".into(),
            owns_snapshot: true,
            snapshot_hash: Some(spork_hash::hash_bytes(b"t")),
            payload: json!({ "origin": "manual" }),
            payload_schema_version: 1,
            lineage_hash: spork_hash::hash_bytes(b"l"),
            model: Some("gpt".into()),
        };
        let v = serde_json::to_value(&p).unwrap();
        let back: NodeCreatedPayload = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn node_created_omits_none_snapshot_and_model() {
        let p = NodeCreatedPayload {
            node_id: Ulid::new(),
            kind: "plan".into(),
            family: Family::Context,
            type_version: "1.0.0".into(),
            parent_ids: vec![],
            branch_id: "main".into(),
            owns_snapshot: false,
            snapshot_hash: None,
            payload: json!({}),
            payload_schema_version: 1,
            lineage_hash: spork_hash::hash_bytes(b"l"),
            model: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("snapshot_hash"));
        assert!(!obj.contains_key("model"));
    }

    #[test]
    fn ref_payloads_use_ref_key() {
        let p = RefCreatedPayload {
            name: "HEAD".into(),
            kind: RefKind::Head,
            to: Ulid::new(),
        };
        let v = serde_json::to_value(&p).unwrap();
        // The field serializes under the frozen `ref` key (a Rust keyword).
        assert!(v.as_object().unwrap().contains_key("ref"));
        let back: RefCreatedPayload = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);

        let m = RefMovedPayload {
            name: "HEAD".into(),
            to: Ulid::new(),
        };
        let mv = serde_json::to_value(&m).unwrap();
        assert!(mv.as_object().unwrap().contains_key("ref"));
        let mback: RefMovedPayload = serde_json::from_value(mv).unwrap();
        assert_eq!(m, mback);
    }

    #[test]
    fn edge_and_status_payloads_round_trip() {
        let e = EdgeAddedPayload {
            from: Ulid::new(),
            to: Ulid::new(),
            edge_type: EdgeType::Validates,
        };
        let eback: EdgeAddedPayload =
            serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(e, eback);

        let s = NodeStatusChangedPayload {
            node_id: Ulid::new(),
            status: Lifecycle::Failed,
            is_stale: true,
            stale_reason: Some("parent changed".into()),
        };
        let sback: NodeStatusChangedPayload =
            serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(s, sback);
    }
}
