//! Spork typed graph: envelope, events, projection, and command layer.
//!
//! This is the integrative core of the F2 typed-graph contract. The typed graph
//! (nodes, edges, refs) is a *pure projection* of the F1 event log: graph
//! mutations are events appended through the F1 single-writer actor, and the
//! projection folds them — nothing writes the projection directly. A command /
//! service layer validates each mutation (the node-type registry contract plus
//! acyclicity) and only then appends, which preserves the F1 soundness
//! guarantee that dropping and rebuilding the projection yields an identical
//! result.
//!
//! The [`NodeEnvelope`] is the stable contract every later consumer dispatches
//! on; its hot fields (status, staleness, snapshot hash) are materialized as
//! indexed SQLite columns in the projection rather than buried inside payload
//! JSON. Stored payloads are upgraded lazily on read via the migration registry,
//! so a node written under an older payload schema reads back current without
//! rewriting storage.
//!
//! This realizes the node envelope in DESIGN.md §6.2 ("Nodes: envelope plus
//! payload"), the edge/acyclicity/lineage model in §6.3 ("Edges, acyclicity, and
//! lineage"), the extensibility model in §6.5 ("Merge and extensibility"), the
//! lifecycle, taxonomy, and envelope split across §7.1–§7.3, the executor and
//! typed-ports model across §9.1–§9.2, the schema-driven node types in §14.5
//! ("Selection, Diffs & Schema-Driven Node Types"), and the IPC contract and
//! core schemas in A.1 ("IPC Contract & Core Schemas").
//!
//! # Architecture
//!
//! ```text
//!   GraphService  (validate-then-append; the ONLY mutation path)
//!        │  append(NewEvent)
//!        ▼
//!   spork-log WriterHandle  ──►  append-only hash-chained event log (truth)
//!        │                              │  Reader::iter_from
//!        │  apply(&event) (keep current)▼
//!        └────────────────────►  GraphProjection (SQLite, rebuildable)
//! ```
//!
//! # The seam (CLAUDE.md C3)
//!
//! The graph is mutated only through [`GraphService`] and observed only through
//! the SQLite-backed [`GraphProjection`]; both sit behind the F1 log, so new
//! node kinds and new graph events are added additively without a flag day.
//!
//! # Evolution safety (CLAUDE.md C5)
//!
//! Every persisted shape — the graph events and the projected node payload —
//! carries an explicit schema version; old payloads are migrated forward on read
//! via `spork-migrate` and stored events are never edited.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use serde_json::json;
//! use spork_hash::hash_bytes;
//! use spork_log::EventLog;
//! use spork_graph::GraphService;
//!
//! let dir = tempfile::tempdir().unwrap();
//! let log = EventLog::open(&dir.path().join("log.db")).unwrap();
//!
//! let mut svc = GraphService::open_in_memory(log.writer()).unwrap();
//! svc.register_builtin_snapshot().unwrap();
//!
//! // A snapshot node owns a snapshot, so a snapshot_hash is required.
//! let root = svc
//!     .create_node(
//!         "snapshot",
//!         None,
//!         vec![],
//!         "main",
//!         json!({ "origin": "manual" }),
//!         true,
//!         Some(hash_bytes(b"tree-root")),
//!     )
//!     .unwrap();
//! assert!(root.owns_snapshot);
//!
//! // The graph is a pure projection of the log: rebuild yields the same digest.
//! let reader = log.reader().unwrap();
//! let rebuilt = spork_graph::GraphProjection::rebuild_from_log(&reader).unwrap();
//! assert_eq!(
//!     rebuilt.canonical_digest().unwrap(),
//!     svc.projection().canonical_digest().unwrap()
//! );
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod envelope;
mod error;
mod events;
mod lineage;
mod projection;
mod service;

pub use envelope::{CostRecord, NodeEnvelope, ENVELOPE_SCHEMA_VERSION};
pub use error::{GraphError, Result};
pub use events::{
    EdgeAddedPayload, NodeCreatedPayload, NodeStatusChangedPayload, RefCreatedPayload,
    RefMovedPayload, EVENT_EDGE_ADDED, EVENT_NODE_CREATED, EVENT_NODE_STATUS_CHANGED,
    EVENT_REF_CREATED, EVENT_REF_MOVED, GRAPH_EVENT_SCHEMA_VERSION,
};
pub use lineage::lineage_hash;
pub use projection::{
    EdgeRow, GraphProjection, ProjectionState, RefRow, PROJECTION_SCHEMA_VERSION,
};
pub use service::{GraphService, BUILTIN_SNAPSHOT_KIND};

// Re-export the most-used neighbouring contracts so a consumer can dispatch on
// the envelope without importing every leaf crate by hand. These are the stable
// vocabulary the envelope is expressed in (DESIGN §6.2, §6.3, §7.3).
pub use spork_edges::{EdgeType, RefKind};
pub use spork_registry::Family;
pub use spork_status::{effective_status, EffectiveStatus, Lifecycle};
