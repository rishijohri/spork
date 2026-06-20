//! The [`CommandHandler`] seam — the single dispatch entry point the daemon
//! implements and the renderer calls.
//!
//! This is the one trait that ties the four contract halves together: a renderer
//! sends a [`Command`], the daemon dispatches it, and the renderer gets back a
//! [`CommandResult`] (an `op_id` for a mutation, inline data for a read) while
//! durable state flows out separately over the
//! [`OpLogEvent`](crate::OpLogEvent) stream.
//!
//! # One seam, one real impl (CLAUDE.md C3)
//!
//! `CommandHandler` is a *seam*, not a plugin point with many implementations.
//! The single production implementor is `spork-daemon`'s `Daemon`, which wires
//! the capability broker, graph service, restore guard, drift capture, and git
//! coexistence behind this one method. The trait lives here, in the contract
//! crate, so the daemon and the future renderer depend on the *interface*, not
//! on each other — exactly the daemon/renderer split this phase freezes
//! (DESIGN.md §5.5, §14.1).
//!
//! # The contract `dispatch` must honor
//!
//! Implementors must preserve the load-bearing rule this crate exists to encode:
//! a mutation returns [`CommandResult::Mutation`] (an `op_id` plus minted ids)
//! and emits its resulting state as [`OpLogEvent`](crate::OpLogEvent)s on the
//! ordered stream; a read returns its data inline and emits nothing. The
//! executable check [`CommandResult::matches_command`] expresses this and is
//! reused by the daemon's tests.
//!
//! Design references: DESIGN.md §5.5, §14.1, §14.4, A.1.

use crate::{Command, CommandResult, IpcError};

/// The typed dispatch seam between renderer and daemon.
///
/// Implemented by `spork-daemon`. A `dispatch` call:
/// 1. authorizes the command through the capability broker (deny-by-default,
///    DESIGN.md §15.1) — a denial is [`IpcError::Capability`];
/// 2. for a **mutation**, appends events through the single-writer actor and
///    returns [`CommandResult::Mutation`] with the `op_id`; the resulting graph
///    state arrives only over the [`OpLogEvent`](crate::OpLogEvent) stream;
/// 3. for a **read**, returns the data inline ([`CommandResult::Diff`] /
///    [`CommandResult::Blob`]) and emits no events.
///
/// `&self` (not `&mut self`): the daemon owns interior mutability (the writer
/// actor, the broker's audit log), so dispatch is shareable across the IPC
/// transport without an exclusive borrow.
pub trait CommandHandler {
    /// Dispatch one command, returning its inline result or an [`IpcError`].
    ///
    /// See the trait docs for the contract this must honor. The returned
    /// [`CommandResult`] must satisfy
    /// [`CommandResult::matches_command`](crate::CommandResult::matches_command)
    /// for `cmd`.
    fn dispatch(&self, cmd: Command) -> Result<CommandResult, IpcError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    /// A minimal in-crate implementor proving the seam is object-safe and
    /// usable, and that an implementation can uphold the load-bearing rule using
    /// only the contract types. (The single *production* implementor is the
    /// daemon; this is a contract-level conformance fixture, not a second real
    /// backend — CLAUDE.md C3.)
    #[derive(Default)]
    struct ContractConformanceHandler;

    impl CommandHandler for ContractConformanceHandler {
        fn dispatch(&self, cmd: Command) -> Result<CommandResult, IpcError> {
            let result = match &cmd {
                // Reads → inline data, no op_id.
                Command::NodeDiff { .. } => CommandResult::Diff {
                    changed_paths: vec!["src/lib.rs".into()],
                },
                Command::BlobRead { .. } => CommandResult::Blob {
                    bytes: b"contents".to_vec(),
                },
                // GC → inline report.
                Command::GcRun { .. } => CommandResult::Gc {
                    reclaimable: vec![],
                    bytes: 0,
                },
                // Every other command → a mutation: only an op_id flows back.
                _ => CommandResult::Mutation {
                    op_id: Ulid::new(),
                    ids: serde_json::json!({}),
                },
            };
            // Enforce the rule the trait promises.
            debug_assert!(result.matches_command(&cmd));
            Ok(result)
        }
    }

    #[test]
    fn trait_is_object_safe() {
        let h: Box<dyn CommandHandler> = Box::<ContractConformanceHandler>::default();
        let r = h
            .dispatch(Command::NodeRestore {
                node_id: Ulid::new(),
            })
            .unwrap();
        assert!(r.is_mutation());
    }

    #[test]
    fn mutation_returns_only_op_id_reads_return_data() {
        let h = ContractConformanceHandler;

        // A mutation: op_id present, no inline graph state.
        let r = h.dispatch(Command::GcRun { dry_run: false });
        // GcRun is the report-bearing case; assert the contract shape holds.
        assert!(matches!(r, Ok(CommandResult::Gc { .. })));

        let r = h
            .dispatch(Command::BranchFork {
                from_node_id: Ulid::new(),
                name: "x".into(),
            })
            .unwrap();
        assert!(r.is_mutation());
        assert!(r.op_id().is_some());

        // A read: inline data, no op_id.
        let r = h
            .dispatch(Command::NodeDiff {
                node_id: Ulid::new(),
                against: None,
            })
            .unwrap();
        assert!(!r.is_mutation());
        assert_eq!(r.op_id(), None);
        assert!(matches!(r, CommandResult::Diff { .. }));
    }

    #[test]
    fn every_dispatch_result_matches_its_command() {
        let h = ContractConformanceHandler;
        let cmds = [
            Command::NodeRestore {
                node_id: Ulid::new(),
            },
            Command::RefMove {
                name: "main".into(),
                to: Ulid::new(),
            },
            Command::NodeDiff {
                node_id: Ulid::new(),
                against: None,
            },
            Command::GcRun { dry_run: true },
            // P5 mutations dispatch through the same seam with zero special
            // casing — they fall into the generic mutation arm.
            Command::NodeRunCheck {
                target_node_id: Ulid::new(),
                spec: serde_json::json!({"check": "sanity"}),
            },
            Command::BranchMerge {
                into_ref: "main".into(),
                from_node_id: Ulid::new(),
                resolution: None,
            },
        ];
        for cmd in cmds {
            let r = h.dispatch(cmd.clone()).unwrap();
            assert!(r.matches_command(&cmd), "result shape wrong for {cmd:?}");
        }
    }
}
