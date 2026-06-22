//! Spork F3 typed IPC contract — the frozen command/event envelope.
//!
//! This crate defines the typed boundary the headless daemon implements and the
//! future DAG-canvas renderer (the separate, deferred F3-UI slice) will bind to.
//! It carries the four halves of that boundary:
//!
//! - [`Command`] — the typed request set the renderer sends (mutations + reads).
//! - [`CommandResult`] — the inline reply set (an `op_id` for a mutation; data
//!   for a read).
//! - [`OpLogEvent`] — the durable, ordered event stream the renderer reduces
//!   over to learn the *result* of a mutation.
//! - [`EphemeralFrame`] / [`EphemeralChannel`] — the high-frequency side-channels
//!   (chat tokens, run stdout) kept strictly off the ordered stream.
//!
//! plus the [`CommandHandler`] dispatch seam (implemented by `spork-daemon`) and
//! the [`IpcError`] failure vocabulary. Freezing these shapes here is what lets
//! the renderer arrive later as a purely additive layer (CLAUDE.md C2/C3).
//!
//! # The load-bearing rule (frozen)
//!
//! **Every mutation returns only an `op_id`, and the resulting state arrives
//! exclusively over the ordered event stream**; reads return their data inline.
//! This single reconciliation path is what makes the renderer's optimistic UI
//! sound: fire a mutation, get a correlation `op_id` back, render optimistically,
//! reconcile when the matching [`OpLogEvent`]s tail in (DESIGN.md §5.5, §14.4).
//! The rule is encoded as types — [`Command::is_mutation`],
//! [`CommandResult::matches_command`] — and exercised in tests, not left as
//! prose.
//!
//! # The durable / ephemeral channel split (frozen)
//!
//! Durable, graph-shaped transitions ride [`OpLogEvent`] in `seq` order; the
//! high-frequency ephemeral streams ride [`EphemeralFrame`] keyed by node id and
//! carry no `seq`. Keeping them separate means a flood of chat tokens or stdout
//! can never delay ordered op-log delivery (DESIGN.md §5.5, §14.4). This crate
//! fixes the *shapes*; `spork-stream` provides the transport that enforces the
//! non-interference guarantee.
//!
//! # Additive forward-tolerance (CLAUDE.md C2/C5)
//!
//! [`OpLogEvent`] is additive: new variants are appended over time and older
//! reducers ignore variants they do not recognize, so the durable stream evolves
//! without a flag day. The read-side [`MaybeEvent`] wrapper makes that property a
//! type — it decodes a recognized variant into [`MaybeEvent::Known`] and any
//! future variant into [`MaybeEvent::Unknown`] (preserving its `type`/`seq`/raw
//! body), so an old reducer keeps ordering and folds only what it understands.
//! Every persisted struct in this contract carries an explicit schema-version
//! constant (CLAUDE.md C5): [`COMMAND_SCHEMA_VERSION`],
//! [`COMMAND_RESULT_SCHEMA_VERSION`], [`OP_LOG_EVENT_SCHEMA_VERSION`],
//! [`EPHEMERAL_FRAME_SCHEMA_VERSION`].
//!
//! This realizes the IPC contract in DESIGN.md §5.5 ("Real-time data flow and
//! security boundary"), the command/event surface in §14.1 ("Process Topology &
//! Stack"), the dual-delivery model in §14.4 ("Real-Time Updates & Optimistic
//! UI"), and the core schemas in A.1 ("IPC Contract & Core Schemas").
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod command;
mod ephemeral;
mod error;
mod event;
mod handler;
mod result;

pub use command::{AgentRunIntent, Command, COMMAND_SCHEMA_VERSION};
pub use ephemeral::{EphemeralChannel, EphemeralFrame, EPHEMERAL_FRAME_SCHEMA_VERSION};
pub use error::IpcError;
pub use event::{MaybeEvent, OpLogEvent, OP_LOG_EVENT_SCHEMA_VERSION};
pub use handler::CommandHandler;
pub use result::{CommandResult, COMMAND_RESULT_SCHEMA_VERSION};

// Re-export the neighbouring contract vocabulary the IPC types are expressed in,
// so a consumer binds to one crate rather than reaching into every leaf crate.
// These are the stable shared types (DESIGN.md §6.3, A.1).
pub use spork_graph::{EdgeType, RefKind};
pub use spork_hash::Hash;
pub use ulid::Ulid;

#[cfg(test)]
mod contract_tests {
    //! Cross-cutting tests that span the four contract halves: the end-to-end
    //! "mutation returns op_id, state arrives via events" round-trip and the
    //! durable-vs-ephemeral separation at the type level.

    use super::*;

    #[test]
    fn schema_versions_are_pinned() {
        // Every persisted contract struct carries a schema version (CLAUDE.md
        // C5). They start at 1 and only move additively.
        assert_eq!(COMMAND_SCHEMA_VERSION, 1);
        assert_eq!(COMMAND_RESULT_SCHEMA_VERSION, 1);
        assert_eq!(OP_LOG_EVENT_SCHEMA_VERSION, 1);
        assert_eq!(EPHEMERAL_FRAME_SCHEMA_VERSION, 1);
    }

    #[test]
    fn mutation_flow_returns_op_id_then_state_via_events() {
        // Model the full frozen flow for a node.create: the command's reply is
        // ONLY an op_id (+ minted ids); the resulting node/edge state is carried
        // by separate ordered events, never by the reply.
        let parent = Ulid::new();
        let cmd = Command::NodeCreate {
            kind: "codebase-edit".into(),
            type_version: "1.0.0".into(),
            parent_ids: vec![parent],
            branch_id: "main".into(),
            payload: serde_json::json!({"prompt": "edit"}),
            owns_snapshot: true,
            snapshot_hash: Some(spork_hash::hash_bytes(b"tree")),
        };
        assert!(cmd.is_mutation());

        let new_node = Ulid::new();
        let reply = CommandResult::Mutation {
            op_id: Ulid::new(),
            ids: serde_json::json!({"nodeId": new_node.to_string()}),
        };
        assert!(reply.matches_command(&cmd));
        // The reply carries no resulting graph state — only correlation + ids.
        let reply_json = serde_json::to_value(&reply).unwrap();
        assert!(reply_json.get("nodeCreated").is_none());

        // The resulting state arrives separately, in order, on the event stream.
        let events = vec![
            OpLogEvent::NodeCreated {
                seq: 1,
                node_id: new_node,
                schema_version: 1,
            },
            OpLogEvent::EdgeAdded {
                seq: 2,
                from: parent,
                to: new_node,
                edge: EdgeType::ParentChild,
            },
        ];
        let seqs: Vec<u64> = events.iter().map(OpLogEvent::seq).collect();
        assert_eq!(seqs, vec![1, 2], "events arrive ordered with no gaps");

        // The whole flow survives a serde round-trip (the wire boundary).
        for e in &events {
            let s = serde_json::to_string(e).unwrap();
            assert_eq!(&serde_json::from_str::<OpLogEvent>(&s).unwrap(), e);
        }
    }

    #[test]
    fn read_flow_returns_data_inline_with_no_events() {
        // A read returns its data inline and (by contract) emits no events; its
        // reply carries no op_id to reconcile.
        let cmd = Command::NodeDiff {
            node_id: Ulid::new(),
            against: None,
        };
        assert!(!cmd.is_mutation());
        let reply = CommandResult::Diff {
            changed_paths: vec!["src/main.rs".into()],
        };
        assert!(reply.matches_command(&cmd));
        assert_eq!(reply.op_id(), None);
    }

    #[test]
    fn durable_and_ephemeral_are_distinct_channels() {
        // Durable events carry an ordering seq; ephemeral frames are keyed by
        // node and carry none — the two channels are structurally separate so a
        // token flood cannot stall ordered delivery (enforced in spork-stream).
        let node = Ulid::new();
        let durable = OpLogEvent::RestorePerformed {
            seq: 99,
            node_id: node,
        };
        assert_eq!(durable.seq(), 99);

        let frame = EphemeralFrame::new(node, EphemeralChannel::ChatTokens, "tok");
        let frame_json = serde_json::to_value(&frame).unwrap();
        assert!(
            frame_json.get("seq").is_none(),
            "ephemeral frames are off the ordered stream"
        );
        // Both still address the same node, so a renderer can correlate them.
        assert_eq!(frame.node_id, node);
    }
}
