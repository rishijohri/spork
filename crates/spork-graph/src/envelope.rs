//! The stable node contract: [`NodeEnvelope`] (plus [`CostRecord`]).
//!
//! Every node in the timeline DAG is a common **envelope** plus a
//! type-specific, JSON-schema-validated **payload** (DESIGN §6.2, §7.2). The
//! envelope is the *stable contract independent of payload* that the graph
//! engine, layout, restore, and handoff generation dispatch on: it carries the
//! identity, lineage, lifecycle, and snapshot fields that are generic over every
//! kind, built-in or custom. The merged authoritative field set is the union of
//! DESIGN §6.2 and §7.2 fixed in Appendix A.1 ("NodeEnvelope (normative union of
//! §6.2 and §7.2)").
//!
//! Hot fields (`status`, `is_stale`, `snapshot_hash`) are promoted to **indexed
//! columns** in the [`GraphProjection`](crate::GraphProjection) rather than
//! queried inside JSON (DESIGN §6.2); this in-memory struct is the
//! materialized-row view a consumer reads back.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use spork_registry::Family;
use spork_status::Lifecycle;
use ulid::Ulid;

/// A per-node cost record (model spend / token accounting).
///
/// The envelope carries an optional `cost` so per-node model attribution and
/// budget surfacing are first-class (DESIGN §6.2, §12.5). Token counts are
/// integers; the monetary amount is recorded in **micro-USD** (millionths of a
/// dollar) as an integer so the value is byte-stable under canonical
/// serialization — `spork-canon` forbids floats in identity-bearing data, and a
/// fixed-point integer round-trips identically on every machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostRecord {
    /// Prompt/input tokens consumed.
    pub input_tokens: u64,
    /// Completion/output tokens produced.
    pub output_tokens: u64,
    /// Total spend in micro-USD (millionths of a dollar), as an integer.
    pub micro_usd: u64,
}

/// The schema version of the [`NodeEnvelope`] materialized form (CLAUDE.md C5).
///
/// This is distinct from a node's `payload_schema_version` (which versions the
/// *payload*) and from a type's semver `type_version`. It versions the column
/// layout the projection materializes the envelope into; a future change to that
/// layout is an additive generation, never an in-place edit.
pub const ENVELOPE_SCHEMA_VERSION: u16 = 1;

/// The common, payload-independent contract for a timeline node.
///
/// This is the union of the two overlapping envelope field lists in DESIGN §6.2
/// and §7.2, fixed normatively in Appendix A.1. It is what every later consumer
/// dispatches on, so its field set is frozen for the F2 generation.
///
/// # Field groups
///
/// * **Identity** — `id` (a time-sortable ULID), `kind` (the registry-resolved
///   discriminator), `family`, `op_log_id` (the ULID of the
///   `graph.node_created` event that minted this node).
/// * **Lineage** — `parent_ids`, `child_ids`, `branch_id`, and `lineage_hash`
///   (a content hash over the parents' lineage plus this node's identity, used
///   for handoff dedup and cache keys, DESIGN §6.3, A.1).
/// * **Lifecycle** — `status` plus the orthogonal staleness axis (`is_stale`,
///   `stale_since`, `stale_reason`); see DESIGN §7.3 and `spork-status`.
/// * **Snapshot ownership** — `owns_snapshot` and `snapshot_hash`, present
///   together iff the node owns a snapshot (DESIGN §6.2, §7.2).
/// * **Attribution** — optional `model` and `cost` (DESIGN §6.2, §12.5).
/// * **Versioning** — `payload_schema_version`, the version the payload bytes
///   conform to *after* any lazy upgrade-on-read (CLAUDE.md C5).
///
/// The struct derives serde so a row reads back as a value and so the projection
/// can canonically hash the materialized graph for its drop-and-rebuild
/// identity check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEnvelope {
    /// The node's time-sortable unique id.
    pub id: Ulid,
    /// The node `kind` — the registry-resolved type discriminator.
    pub kind: String,
    /// The node [`Family`] (derived from the type descriptor).
    pub family: Family,
    /// Whether the node owns a content-addressed snapshot.
    pub owns_snapshot: bool,
    /// The direct parents (timeline + merge parents), in creation order.
    pub parent_ids: Vec<Ulid>,
    /// The direct children, in creation order. Materialized by the projection as
    /// the exact inverse of `parent_ids`: a node `c` is a child of `n` iff `n` is
    /// in `c.parent_ids` (via a `PARENT_CHILD` or `MERGE_PARENT` edge), so a merge
    /// node appears in each of its merge-parents' `child_ids`.
    pub child_ids: Vec<Ulid>,
    /// The branch this node was created on.
    pub branch_id: String,
    /// The lifecycle status (DESIGN §7.3).
    pub status: Lifecycle,
    /// Whether the node's result is stale (the orthogonal staleness axis).
    pub is_stale: bool,
    /// When the node became stale (a millisecond timestamp), if stale.
    pub stale_since: Option<u64>,
    /// Why the node became stale, if stale.
    pub stale_reason: Option<String>,
    /// The content-addressed snapshot hash — present iff `owns_snapshot`.
    pub snapshot_hash: Option<Hash>,
    /// The model used to produce this node, if any (per-node attribution).
    pub model: Option<String>,
    /// The per-node cost record, if any.
    pub cost: Option<CostRecord>,
    /// The lineage hash: a content hash over the parents' lineage hashes plus
    /// this node's id and kind (DESIGN §6.3, A.1).
    pub lineage_hash: Hash,
    /// The schema version the stored payload conforms to after lazy upgrade.
    pub payload_schema_version: u16,
    /// The ULID of the `graph.node_created` event that created this node.
    pub op_log_id: Ulid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    fn sample() -> NodeEnvelope {
        NodeEnvelope {
            id: Ulid::new(),
            kind: "snapshot".into(),
            family: Family::Mutating,
            owns_snapshot: true,
            parent_ids: vec![Ulid::new()],
            child_ids: vec![],
            branch_id: "main".into(),
            status: Lifecycle::Passed,
            is_stale: false,
            stale_since: None,
            stale_reason: None,
            snapshot_hash: Some(hash_bytes(b"t")),
            model: Some("gpt".into()),
            cost: Some(CostRecord {
                input_tokens: 10,
                output_tokens: 20,
                micro_usd: 1500,
            }),
            lineage_hash: hash_bytes(b"l"),
            payload_schema_version: 1,
            op_log_id: Ulid::new(),
        }
    }

    #[test]
    fn envelope_round_trips_through_serde() {
        let env = sample();
        let v = serde_json::to_value(&env).unwrap();
        let back: NodeEnvelope = serde_json::from_value(v).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn cost_is_integer_only_and_canonicalizes() {
        // Integer-only cost survives canonical encoding (floats are forbidden).
        let env = sample();
        let bytes = spork_canon::canonicalize(&env);
        assert!(bytes.is_ok(), "envelope must canonicalize (integers only)");
    }
}
