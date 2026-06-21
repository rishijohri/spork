//! The frozen read / view-model boundary the future DAG-canvas renderer binds
//! to (DESIGN.md §14.3).
//!
//! The renderer never binds to the daemon's live [`GraphService`] /
//! [`GraphProjection`](spork_graph::GraphProjection) directly. Instead the daemon
//! publishes a **denormalized read snapshot** — [`GraphView`] — of exactly the
//! nodes, edges, and refs a canvas needs to lay out and render, plus the
//! ordered-event subscription it reduces over for live updates. This is the
//! middle "view-model" layer of the three-layer DAG pipeline (DESIGN.md §14.3):
//! the daemon source-of-truth on one side, the React Flow presentation on the
//! other, and this stable, serializable shape between them.
//!
//! Freezing this shape now (alongside the typed IPC envelope) is what makes the
//! deferred F3-UI slice purely additive: a renderer can be written against
//! [`GraphView`] / [`NodeView`] / [`EdgeView`] / [`RefView`] today and bind to
//! the same fields whenever the canvas lands (CLAUDE.md C2/C3). The view is a
//! *read snapshot*, not a live handle — calling [`Daemon::graph_view`] again
//! after a mutation returns the new state; live deltas arrive over the event
//! stream.
//!
//! Every struct carries a `schema_version` (CLAUDE.md C5) so the shape can grow
//! additive fields without a flag day.

use serde::{Deserialize, Serialize};
use spork_graph::{EdgeType, Family, Lifecycle, RefKind};
use ulid::Ulid;

/// The schema version of the [`GraphView`] read snapshot (CLAUDE.md C5).
pub const GRAPH_VIEW_SCHEMA_VERSION: u16 = 1;

/// A denormalized read snapshot of the whole work-DAG for the renderer.
///
/// Produced by [`Daemon::graph_view`](crate::Daemon::graph_view). Nodes are
/// ordered by id (which is a ULID, so creation-ordered), edges by
/// `(from, to, type)`, refs by name — a deterministic order so two views of the
/// same state serialize identically (useful for caching and tests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphView {
    /// The view-model schema version.
    pub schema_version: u16,
    /// Every node, ordered by id.
    pub nodes: Vec<NodeView>,
    /// Every edge, ordered by `(from, to, edge_type)`.
    pub edges: Vec<EdgeView>,
    /// Every ref (HEAD, branches, tags), ordered by name.
    pub refs: Vec<RefView>,
}

/// The renderer-facing projection of one node (a card on the canvas).
///
/// A flattened subset of the F2 [`NodeEnvelope`](spork_graph::NodeEnvelope): the
/// identity and lineage a layout needs, the discriminators that pick the card's
/// legend color/shape (`kind`, `family`), the lifecycle/staleness that pick its
/// status badge, and the snapshot binding that gates the "restore" action. Heavy
/// payloads are fetched lazily by id (DESIGN.md §14.5), never inlined here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    /// The node id (ULID, time-sortable).
    pub id: Ulid,
    /// The node kind discriminator (registry-resolved; drives the legend).
    pub kind: String,
    /// The node family (mutating / observing / context).
    pub family: Family,
    /// The lifecycle status token.
    pub status: Lifecycle,
    /// Whether the node's result/state is stale against its inputs.
    pub is_stale: bool,
    /// Whether the node owns a restorable snapshot (gates the restore action).
    pub owns_snapshot: bool,
    /// The owned snapshot tree hash, present iff `owns_snapshot` (hex string so
    /// the wire shape is renderer-portable).
    pub snapshot_hash: Option<String>,
    /// The branch this node was created on.
    pub branch_id: String,
    /// The lineage parents (drive the DAG edges layout uses).
    pub parent_ids: Vec<Ulid>,
    /// The model attributed to this node, if any.
    pub model: Option<String>,
    /// The per-node cost record, if any (P6: an agent-run attaches its priced
    /// cost; the renderer sums these for the per-branch ledger, DESIGN §12.5).
    /// Additive view field (CLAUDE.md C5) — `None` for every node that carries
    /// no cost, exactly as before.
    pub cost: Option<CostView>,
}

/// The renderer-facing projection of a node's cost (P6, DESIGN §12.5).
///
/// A camelCase mirror of [`spork_graph::CostRecord`] for the TypeScript binding;
/// every figure is an integer (tokens, micro-USD) so it round-trips through the
/// canonical encoder that forbids floats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostView {
    /// Total prompt/input tokens (uncached + cache read + write).
    pub input_tokens: u64,
    /// Completion/output tokens produced.
    pub output_tokens: u64,
    /// Total spend in micro-USD (millionths of a dollar), as an integer.
    pub micro_usd: u64,
}

impl From<spork_graph::CostRecord> for CostView {
    fn from(c: spork_graph::CostRecord) -> Self {
        CostView {
            input_tokens: c.input_tokens,
            output_tokens: c.output_tokens,
            micro_usd: c.micro_usd,
        }
    }
}

/// The renderer-facing projection of one typed edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeView {
    /// The tail node.
    pub from: Ulid,
    /// The head node.
    pub to: Ulid,
    /// The typed relation (drives the edge's style).
    pub edge_type: EdgeType,
}

/// The renderer-facing projection of one ref (HEAD / branch / tag pointer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefView {
    /// The ref name (`HEAD`, `branch/<name>`, a tag name).
    pub name: String,
    /// The ref kind.
    pub kind: RefKind,
    /// The node the ref points at.
    pub target: Ulid,
}

impl GraphView {
    /// Look up a node view by id (linear over the ordered list).
    #[must_use]
    pub fn node(&self, id: Ulid) -> Option<&NodeView> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Look up a ref view by name.
    #[must_use]
    pub fn ref_named(&self, name: &str) -> Option<&RefView> {
        self.refs.iter().find(|r| r.name == name)
    }

    /// The node a ref points at, if the ref exists.
    #[must_use]
    pub fn ref_target(&self, name: &str) -> Option<Ulid> {
        self.ref_named(name).map(|r| r.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_view() -> GraphView {
        let a = Ulid::new();
        let b = Ulid::new();
        GraphView {
            schema_version: GRAPH_VIEW_SCHEMA_VERSION,
            nodes: vec![
                NodeView {
                    id: a,
                    kind: "snapshot".into(),
                    family: Family::Mutating,
                    status: Lifecycle::Passed,
                    is_stale: false,
                    owns_snapshot: true,
                    snapshot_hash: Some(spork_hash::hash_bytes(b"t").to_hex()),
                    branch_id: "main".into(),
                    parent_ids: vec![],
                    model: None,
                    cost: None,
                },
                NodeView {
                    id: b,
                    kind: "snapshot".into(),
                    family: Family::Mutating,
                    status: Lifecycle::Passed,
                    is_stale: false,
                    owns_snapshot: true,
                    snapshot_hash: Some(spork_hash::hash_bytes(b"t2").to_hex()),
                    branch_id: "main".into(),
                    parent_ids: vec![a],
                    model: Some("gpt".into()),
                    cost: Some(CostView {
                        input_tokens: 1_000,
                        output_tokens: 200,
                        micro_usd: 4_500,
                    }),
                },
            ],
            edges: vec![EdgeView {
                from: a,
                to: b,
                edge_type: EdgeType::ParentChild,
            }],
            refs: vec![RefView {
                name: "HEAD".into(),
                kind: RefKind::Head,
                target: b,
            }],
        }
    }

    #[test]
    fn graph_view_round_trips_through_json() {
        let view = sample_view();
        let json = serde_json::to_string(&view).unwrap();
        let back: GraphView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
    }

    #[test]
    fn graph_view_wire_form_is_camel_cased() {
        let view = sample_view();
        let v = serde_json::to_value(&view).unwrap();
        assert_eq!(v["schemaVersion"], u64::from(GRAPH_VIEW_SCHEMA_VERSION));
        let node = &v["nodes"][0];
        assert!(node.get("ownsSnapshot").is_some());
        assert!(node.get("snapshotHash").is_some());
        assert!(node.get("parentIds").is_some());
        assert!(v["edges"][0].get("edgeType").is_some());
        // The P6 cost view serializes camelCase under each node.
        let priced = &v["nodes"][1]["cost"];
        assert_eq!(priced["inputTokens"], 1_000);
        assert_eq!(priced["outputTokens"], 200);
        assert_eq!(priced["microUsd"], 4_500);
    }

    #[test]
    fn lookups_resolve_by_id_and_name() {
        let view = sample_view();
        let a = view.nodes[0].id;
        let b = view.nodes[1].id;
        assert_eq!(view.node(a).unwrap().id, a);
        assert!(view.node(Ulid::new()).is_none());
        assert_eq!(view.ref_target("HEAD"), Some(b));
        assert_eq!(view.ref_target("missing"), None);
    }
}
