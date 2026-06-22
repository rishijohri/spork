//! Spork F3 headless daemon — the single integration point and read surface.
//!
//! The daemon is where every F3 capability meets the rest of the system. It owns
//! the F2 [`GraphService`](spork_graph::GraphService), the ordered
//! [`EventStream`](spork_stream::EventStream) and ephemeral
//! [`EphemeralBus`](spork_stream::EphemeralBus), the
//! [`CapabilityBroker`](spork_broker::CapabilityBroker), the
//! [`FileVault`](spork_vault::FileVault), the
//! [`DriftCapture`](spork_drift::DriftCapture) pipeline, the restore guard, and
//! the git bridge — and exposes them all behind the single
//! [`spork_ipc::CommandHandler`] seam. Dispatching a command authorizes the
//! required capability through the broker, performs the operation (mutations are
//! appended as events through the F1 single-writer actor by way of the graph
//! service or the restore guard), and returns an `op_id`; the resulting state
//! then flows out over the ordered [`EventStream`](spork_stream::EventStream),
//! never on the command's own return value.
//!
//! The daemon also publishes the frozen read/view-model boundary
//! ([`Daemon::graph_view`] → [`GraphView`]): a denormalized snapshot of nodes,
//! edges, and refs that the deferred F3-UI renderer will consume, plus an
//! [`OpLogEvent`](spork_ipc::OpLogEvent) subscription
//! ([`Daemon::subscribe_events`]). Freezing this read surface (alongside the
//! typed IPC envelope) is what keeps the renderer a purely additive later slice
//! (CLAUDE.md C2/C3).
//!
//! # The load-bearing rule the daemon honors
//!
//! Every **mutation** returns only an `op_id`; its resulting graph state arrives
//! **exclusively** over the ordered event stream. Every **read**
//! ([`Command::NodeDiff`](spork_ipc::Command::NodeDiff),
//! [`Command::BlobRead`](spork_ipc::Command::BlobRead)) returns its data inline
//! and emits no events. The daemon `debug_assert!`s this with
//! [`CommandResult::matches_command`](spork_ipc::CommandResult::matches_command)
//! on every dispatch (DESIGN.md §5.5, §14.4).
//!
//! # Durable vs. ephemeral non-interference
//!
//! The ordered rail and the node-keyed ephemeral side-channels are separate
//! transports kept outside the daemon's critical-section lock, so a flood of chat
//! tokens / run stdout on [`Daemon::subscribe_node`] can never delay ordered
//! op-log delivery on [`Daemon::subscribe_events`] (DESIGN.md §5.5, §14.4).
//!
//! # Security boundary
//!
//! All privileged operations live behind the broker (deny-by-default), the vault
//! resolves secrets only inside the daemon (never hashed into the CAS, never
//! returned in serialized form), and drift capture secret-scans every file so a
//! key never enters the content store (DESIGN.md §15.1, §15.4, §15.5).
//!
//! Design references: DESIGN.md §5.5 (real-time data flow & security boundary),
//! §14.1 (process topology), §14.3 (the three-layer DAG pipeline / view-model
//! boundary frozen for F3-UI), §14.4 (real-time updates), §15.1–§15.4 (security),
//! A.1 (the IPC contract these commands realize).
//!
//! # Example
//!
//! ```no_run
//! use spork_daemon::Daemon;
//! use spork_ipc::{Command, CommandHandler};
//! use spork_hash::hash_bytes;
//!
//! let dir = tempfile::tempdir().unwrap();
//! let daemon = Daemon::open(dir.path()).unwrap();
//! let events = daemon.subscribe_events();
//!
//! // A mutation returns an op_id; the created node arrives over the stream.
//! let result = daemon
//!     .dispatch(Command::NodeCreate {
//!         kind: "snapshot".into(),
//!         type_version: "1.0.0".into(),
//!         parent_ids: vec![],
//!         branch_id: "main".into(),
//!         payload: serde_json::json!({ "origin": "manual" }),
//!         owns_snapshot: true,
//!         snapshot_hash: Some(hash_bytes(b"tree")),
//!     })
//!     .unwrap();
//! assert!(result.op_id().is_some());
//! let event = events.recv().unwrap(); // NodeCreated arrives via events, not the return
//! assert_eq!(event.seq(), 1);
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent;
mod checkout;
mod core;
mod ctx;
mod dispatch;
mod edit;
mod error;
mod feature;
mod gate;
mod history;
mod import;
mod mutate;
mod nodes;
mod read;
mod view;

pub use crate::agent::{AgentConfig, AGENT_CONTEXT_KIND};
pub use crate::core::{Daemon, DaemonBuilder, DAEMON_SCHEMA_VERSION, STATE_SUBDIR, WORKTREE_GLOB};
pub use crate::error::DaemonError;
pub use crate::feature::EventReceiver;
pub use crate::gate::GATE_KIND;
pub use crate::view::{
    CostView, EdgeView, GateVerdictView, GraphView, NodeView, RefView, GRAPH_VIEW_SCHEMA_VERSION,
};

// Re-export the contract vocabulary a headless client binds to, so it depends on
// one crate (the daemon) rather than reaching into every leaf crate.
pub use spork_graph::{EdgeType, Family, Lifecycle, RefKind};
pub use spork_ipc::{
    AgentRunIntent, Command, CommandHandler, CommandResult, EphemeralChannel, EphemeralFrame,
    IpcError, OpLogEvent,
};
