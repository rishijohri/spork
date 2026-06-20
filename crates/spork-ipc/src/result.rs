//! The [`CommandResult`] reply set — the inline response to a [`Command`].
//!
//! [`CommandResult`] is the response half of the request/response channel
//! (DESIGN.md A.1, "Command channel"). It encodes the single most important rule
//! of the whole IPC contract, the one this crate exists to freeze:
//!
//! > **Every mutation returns only an `op_id`. Its resulting state arrives
//! > exclusively over the ordered [`OpLogEvent`](crate::OpLogEvent) stream.
//! > Reads return their data inline.**
//!
//! This single reconciliation path is what makes optimistic UI sound (DESIGN.md
//! §5.5, §14.4): the renderer fires a mutation, gets back a correlation `op_id`,
//! renders optimistically, and reconciles when the matching events tail in over
//! the durable stream. There is never a second, divergent source of truth in the
//! mutation's return value — [`CommandResult::Mutation`] carries an `op_id` and
//! a small bag of freshly-minted ids (`ids`), nothing that describes resulting
//! graph state.
//!
//! The two read variants ([`CommandResult::Diff`], [`CommandResult::Blob`]) and
//! the GC report ([`CommandResult::Gc`]) *do* carry data inline, because reads
//! have no event to reconcile against and the GC report is a computed summary,
//! not graph state. The mutation-side of GC still flows as
//! [`OpLogEvent::GcPerformed`](crate::OpLogEvent::GcPerformed).
//!
//! Design references: DESIGN.md §5.5, §14.1, §14.4, A.1.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::Command;

/// The schema version of the [`CommandResult`] envelope. See
/// [`COMMAND_SCHEMA_VERSION`](crate::COMMAND_SCHEMA_VERSION) for the evolution
/// discipline.
pub const COMMAND_RESULT_SCHEMA_VERSION: u16 = 1;

/// The typed reply to a [`Command`].
///
/// Internally tagged on a `"result"` field with `SCREAMING_SNAKE_CASE` variant
/// names and `camelCase` fields, matching the [`Command`] wire conventions.
///
/// The variants partition by command kind:
/// - [`CommandResult::Mutation`] — the reply to *every* mutation command. The
///   `op_id` is the correlation handle; `ids` carries any freshly-minted ids the
///   renderer needs immediately (e.g. the new `nodeId`/`refId`). State arrives
///   via events, never here.
/// - [`CommandResult::Diff`] — the reply to [`Command::NodeDiff`].
/// - [`CommandResult::Blob`] — the reply to [`Command::BlobRead`].
/// - [`CommandResult::Gc`] — the reply to [`Command::GcRun`] (the reclaimable
///   report; the durable effect, if any, still rides
///   [`OpLogEvent::GcPerformed`](crate::OpLogEvent::GcPerformed)).
/// - [`CommandResult::Git`] — the reply to [`Command::GitExport`] /
///   [`Command::GitPush`] (the resulting branch/commit/pushed triple; these are
///   action-shaped and emit no event — DESIGN.md §10.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all_fields = "camelCase")]
pub enum CommandResult {
    /// The reply to any mutation command. Carries the correlation `op_id` and a
    /// small JSON bag of freshly-minted ids (e.g. `{"nodeId": "..."}` or
    /// `{"refId": "..."}`). Deliberately carries **no** resulting graph state —
    /// that arrives over the event stream (DESIGN.md §5.5, A.1).
    Mutation {
        /// The correlation id for this operation. The renderer keys its
        /// optimistic-UI reconciliation on this and matches it against tailing
        /// [`OpLogEvent`](crate::OpLogEvent)s.
        op_id: Ulid,
        /// Freshly-minted ids the renderer needs synchronously (the new
        /// `nodeId`, `refId`, etc.). A loose JSON object so different mutations
        /// can return their own minimal id set without a per-command result
        /// variant (CLAUDE.md C3). Empty object when there are none.
        ids: serde_json::Value,
    },

    /// The reply to [`Command::NodeDiff`]: the list of changed paths between the
    /// node tree and its baseline (DESIGN.md §14.5).
    Diff {
        /// Paths that differ, relative to the tree root.
        changed_paths: Vec<String>,
    },

    /// The reply to [`Command::BlobRead`]: the raw bytes of the requested blob
    /// (DESIGN.md §14.5, fetched lazily as files open).
    Blob {
        /// The blob contents.
        bytes: Vec<u8>,
    },

    /// The reply to [`Command::GcRun`]: the reclaimable object set and the byte
    /// total that would be (or was) freed (DESIGN.md A.1 `gc.run`).
    Gc {
        /// Object ids that are reclaimable.
        reclaimable: Vec<String>,
        /// Total bytes reclaimable across `reclaimable`.
        bytes: u64,
    },

    /// The reply to [`Command::GitExport`] / [`Command::GitPush`]: the git branch
    /// the node's snapshot was projected onto, the resulting commit SHA, and
    /// whether it was pushed to a remote (DESIGN.md §10.4).
    ///
    /// These are **action-shaped** results returned inline: the git operations
    /// change no work-DAG state, so there is no [`OpLogEvent`](crate::OpLogEvent)
    /// to reconcile against — the renderer just shows the resulting branch/commit.
    /// `pushed` is `false` for a plain export and `true` once a push succeeds.
    Git {
        /// The branch the snapshot was exported to (e.g. `spork/<nodeId>`).
        branch: String,
        /// The exported commit's SHA-1 as a 40-char lowercase hex string.
        commit_sha: String,
        /// Whether the branch was pushed to a remote.
        pushed: bool,
    },
}

impl CommandResult {
    /// Whether this result is the reply to a mutation (carries only an `op_id`)
    /// rather than inline read data.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        matches!(self, CommandResult::Mutation { .. })
    }

    /// The correlation `op_id`, present only for [`CommandResult::Mutation`].
    ///
    /// Reads have nothing to reconcile, so they have no `op_id`.
    #[must_use]
    pub fn op_id(&self) -> Option<Ulid> {
        match self {
            CommandResult::Mutation { op_id, .. } => Some(*op_id),
            _ => None,
        }
    }

    /// Whether this result is a valid reply *shape* for the given command, i.e.
    /// it honors the load-bearing rule: mutations reply with
    /// [`CommandResult::Mutation`]; reads reply with their matching inline
    /// variant.
    ///
    /// This is the executable form of the rule frozen by this crate; the daemon
    /// and its tests use it to assert that no read ever leaks an `op_id` and no
    /// mutation ever leaks graph state in its return value.
    #[must_use]
    pub fn matches_command(&self, cmd: &Command) -> bool {
        match cmd {
            Command::NodeDiff { .. } => matches!(self, CommandResult::Diff { .. }),
            Command::BlobRead { .. } => matches!(self, CommandResult::Blob { .. }),
            Command::GcRun { .. } => {
                // GC is a mutation that also returns an inline report; the
                // contract models the report explicitly as the `Gc` variant.
                matches!(self, CommandResult::Gc { .. })
            }
            // The F3-UI git actions return their result inline as `Git`. They are
            // not graph mutations (no `OpLogEvent` to reconcile): `GitExport`
            // projects, `GitPush` projects-then-pushes, both reply with the
            // branch/commit/pushed triple (DESIGN.md §10.4).
            Command::GitExport { .. } | Command::GitPush { .. } => {
                matches!(self, CommandResult::Git { .. })
            }
            // Every other command is a pure mutation. This includes the P5
            // additions `NodeRunCheck` and `BranchMerge`: both return only an
            // `op_id`, with their durable effect arriving over the event stream
            // (`ResultRecorded` / `MergePerformed`). A conflicting `BranchMerge`
            // still replies with `Mutation`; the conflict set rides the `ids`
            // bag so the contract's reply *shape* is unchanged (DESIGN.md §6.5).
            _ => matches!(self, CommandResult::Mutation { .. }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    fn samples() -> Vec<CommandResult> {
        vec![
            CommandResult::Mutation {
                op_id: Ulid::new(),
                ids: serde_json::json!({"nodeId": Ulid::new().to_string()}),
            },
            CommandResult::Mutation {
                op_id: Ulid::new(),
                ids: serde_json::json!({}),
            },
            CommandResult::Diff {
                changed_paths: vec!["src/a.rs".into(), "src/b.rs".into()],
            },
            CommandResult::Blob {
                bytes: vec![0, 1, 2, 255],
            },
            CommandResult::Gc {
                reclaimable: vec!["b3.1:dead".into()],
                bytes: 4096,
            },
            CommandResult::Git {
                branch: "spork/abc".into(),
                commit_sha: "a".repeat(40),
                pushed: false,
            },
            CommandResult::Git {
                branch: "spork/abc".into(),
                commit_sha: "b".repeat(40),
                pushed: true,
            },
        ]
    }

    #[test]
    fn every_result_round_trips_through_json() {
        for r in samples() {
            let json = serde_json::to_string(&r).expect("serialize");
            let back: CommandResult = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(r, back, "round-trip mismatch for {r:?}");
        }
    }

    #[test]
    fn mutation_result_carries_op_id_and_no_state() {
        let op = Ulid::new();
        let r = CommandResult::Mutation {
            op_id: op,
            ids: serde_json::json!({"nodeId": "x"}),
        };
        assert!(r.is_mutation());
        assert_eq!(r.op_id(), Some(op));
        // The only state-bearing surface a mutation result has is the id bag;
        // it never carries a `changedPaths` / `bytes` graph payload.
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["result"], "MUTATION");
        assert!(v.get("changedPaths").is_none());
    }

    #[test]
    fn read_results_have_no_op_id() {
        let diff = CommandResult::Diff {
            changed_paths: vec![],
        };
        let blob = CommandResult::Blob { bytes: vec![] };
        assert!(!diff.is_mutation());
        assert!(!blob.is_mutation());
        assert_eq!(diff.op_id(), None);
        assert_eq!(blob.op_id(), None);
    }

    #[test]
    fn result_shape_matches_its_command() {
        let node = Ulid::new();
        // Reads → inline variants.
        assert!(CommandResult::Diff {
            changed_paths: vec![]
        }
        .matches_command(&Command::NodeDiff {
            node_id: node,
            against: None
        }));
        assert!(
            CommandResult::Blob { bytes: vec![] }.matches_command(&Command::BlobRead {
                tree_hash: hash_bytes(b"t"),
                path: "p".into(),
            })
        );
        // GC → report variant.
        assert!(CommandResult::Gc {
            reclaimable: vec![],
            bytes: 0
        }
        .matches_command(&Command::GcRun { dry_run: true }));
        // Pure mutations → Mutation variant.
        assert!(CommandResult::Mutation {
            op_id: Ulid::new(),
            ids: serde_json::json!({})
        }
        .matches_command(&Command::NodeRestore { node_id: node }));

        // A read replying with a mutation shape violates the rule.
        assert!(!CommandResult::Mutation {
            op_id: Ulid::new(),
            ids: serde_json::json!({})
        }
        .matches_command(&Command::NodeDiff {
            node_id: node,
            against: None
        }));
    }

    #[test]
    fn p5_commands_reply_with_mutation_shape() {
        // The P5 additions are pure mutations: they reply with `Mutation`
        // (op_id + ids), with state arriving over the event stream.
        let run_check = Command::NodeRunCheck {
            target_node_id: Ulid::new(),
            spec: serde_json::Value::Null,
        };
        assert!(CommandResult::Mutation {
            op_id: Ulid::new(),
            ids: serde_json::json!({"nodeId": Ulid::new().to_string()}),
        }
        .matches_command(&run_check));

        // A conflicting BranchMerge still replies with `Mutation`; the conflict
        // set is carried in the `ids` bag, so the reply *shape* is unchanged.
        let merge = Command::BranchMerge {
            into_ref: "main".into(),
            from_node_id: Ulid::new(),
            resolution: None,
        };
        assert!(CommandResult::Mutation {
            op_id: Ulid::new(),
            ids: serde_json::json!({"conflicts": [{"path": "src/a.rs"}]}),
        }
        .matches_command(&merge));

        // And a read shape is NOT a valid reply for either P5 mutation.
        assert!(!CommandResult::Diff {
            changed_paths: vec![]
        }
        .matches_command(&run_check));
    }

    #[test]
    fn git_actions_reply_with_git_shape_inline() {
        let node = Ulid::new();
        let git = CommandResult::Git {
            branch: "spork/x".into(),
            commit_sha: "0".repeat(40),
            pushed: true,
        };
        // The git result is action-shaped: it carries no op_id.
        assert!(!git.is_mutation());
        assert_eq!(git.op_id(), None);
        // It is the valid reply shape for both git commands.
        assert!(git.matches_command(&Command::GitExport {
            node_id: node,
            branch: None,
        }));
        assert!(git.matches_command(&Command::GitPush {
            node_id: node,
            remote: None,
        }));
        // A mutation shape is NOT a valid reply for a git command.
        assert!(!CommandResult::Mutation {
            op_id: Ulid::new(),
            ids: serde_json::json!({}),
        }
        .matches_command(&Command::GitExport {
            node_id: node,
            branch: None,
        }));

        // Wire form: tagged "GIT", camelCase fields.
        let v = serde_json::to_value(&git).unwrap();
        assert_eq!(v["result"], "GIT");
        assert_eq!(v["commitSha"], "0".repeat(40));
        assert_eq!(v["pushed"], true);
    }
}
