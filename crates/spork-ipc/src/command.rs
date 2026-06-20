//! The [`Command`] request set — the frozen, typed mutation/read surface the
//! renderer sends to the daemon.
//!
//! A `Command` is one entry on the request/response channel of the IPC contract
//! (DESIGN.md A.1, "Command channel"). Each variant maps to a row of the A.1
//! command table:
//!
//! | `Command` variant | A.1 command | Kind |
//! |---|---|---|
//! | [`Command::NodeCreate`]  | `node.create`  | mutation |
//! | [`Command::NodeRestore`] | `node.restore` | mutation |
//! | [`Command::BranchFork`]  | `branch.fork`  | mutation |
//! | [`Command::RefCreate`]   | (ref creation) | mutation |
//! | [`Command::RefMove`]     | (ref move)     | mutation |
//! | [`Command::OpUndo`]      | `op.undo`      | mutation |
//! | [`Command::OpRedo`]      | `op.redo`      | mutation |
//! | [`Command::GcRun`]       | `gc.run`       | mutation |
//! | [`Command::NodeDiff`]    | `node.diff`    | read |
//! | [`Command::BlobRead`]    | `blob.read`    | read |
//! | [`Command::NodeRunCheck`] | `node.runCheck` | mutation |
//! | [`Command::BranchMerge`]  | `branch.merge`  | mutation |
//! | [`Command::GitExport`]    | `git.export`    | action (inline) |
//! | [`Command::GitPush`]      | `git.push`      | action (inline) |
//!
//! `NodeRunCheck`/`BranchMerge` are P5 additions (DESIGN.md §6.5, §8.1, §8.2):
//! running an observing check against a node, and merging a branch via 3-way
//! reconciliation. `GitExport`/`GitPush` are the F3-UI git additions (DESIGN.md
//! §10.4): they project / push a node's snapshot to a real Git branch. The git
//! pair are **action-shaped**, not graph mutations — they change no work-DAG
//! state, so they emit no [`OpLogEvent`](crate::OpLogEvent) and return their
//! result inline as [`CommandResult::Git`](crate::CommandResult::Git). All four
//! are appended, not inserted, so the frozen wire form of the original variants
//! is unchanged (CLAUDE.md C2/C3).
//!
//! # The load-bearing rule (frozen here)
//!
//! Mutation commands ([`Command::NodeCreate`] through [`Command::GcRun`]) return
//! only a correlation handle ([`CommandResult::Mutation`]'s `op_id`); the
//! resulting graph state arrives **exclusively** over the ordered
//! [`OpLogEvent`](crate::OpLogEvent) stream. Read commands ([`Command::NodeDiff`],
//! [`Command::BlobRead`]) return their data inline. See the crate-level docs and
//! [`CommandResult`](crate::CommandResult) for the full statement of this rule.
//!
//! # Serde shape
//!
//! `Command` is internally tagged on a `"command"` field with
//! `SCREAMING_SNAKE_CASE` variant names, so a wire payload reads
//! `{"command":"NODE_CREATE", ...fields}`. Field names are `camelCase` to match
//! the renderer's TypeScript binding (A.1 names commands and fields in
//! camelCase). The tag is a stable string, not the enum's source order, so
//! reordering or appending variants never changes the wire form (CLAUDE.md C2).
//!
//! Design references: DESIGN.md §5.5, §14.1, §14.4, A.1.

use serde::{Deserialize, Serialize};
use spork_graph::RefKind;
use spork_hash::Hash;
use ulid::Ulid;

/// The schema version of the [`Command`] envelope.
///
/// Bumped only by an additive, backward-compatible change (a new variant or an
/// optional field). A breaking change would instead introduce a *new* command
/// generation rather than mutate this one in place (CLAUDE.md C2).
pub const COMMAND_SCHEMA_VERSION: u16 = 1;

/// A typed request from the renderer to the daemon.
///
/// Variants split cleanly into **mutations** — which return an
/// [`op_id`](crate::CommandResult::Mutation) and emit
/// [`OpLogEvent`](crate::OpLogEvent)s — and **reads**
/// ([`Command::NodeDiff`], [`Command::BlobRead`]) which return data inline. The
/// daemon authorizes every variant through the capability broker before acting
/// (DESIGN.md §15.1).
///
/// This enum is **frozen**: existing variants and their fields do not change.
/// The contract evolves only by appending new variants, which older daemons
/// reject as unknown rather than misinterpret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all_fields = "camelCase")]
pub enum Command {
    /// Create a new typed work node (A.1 `node.create`). A *mutation*: returns
    /// an `op_id`; the created node and its parent edges arrive as
    /// [`OpLogEvent::NodeCreated`](crate::OpLogEvent::NodeCreated) and
    /// [`OpLogEvent::EdgeAdded`](crate::OpLogEvent::EdgeAdded).
    NodeCreate {
        /// The node kind discriminator, resolved through the node-type registry
        /// (DESIGN.md §6.2). A free string here so user-defined kinds need no
        /// contract change (CLAUDE.md C3).
        kind: String,
        /// The registry type version the payload was authored against; lets the
        /// daemon select the right payload migration (DESIGN.md §7.2).
        type_version: String,
        /// Lineage parents. Empty for a root node; one entry for a linear
        /// child; several for a merge node (DESIGN.md §6.3).
        parent_ids: Vec<Ulid>,
        /// The branch ref this node is created on.
        branch_id: String,
        /// The node-type-specific payload, validated against the registry's
        /// schema by the daemon (DESIGN.md §6.2, A.1).
        payload: serde_json::Value,
        /// Whether this node owns a restorable snapshot. Must agree with the
        /// registry's declaration for `kind` or the daemon rejects it
        /// (DESIGN.md §6.2 `ownsSnapshot`).
        owns_snapshot: bool,
        /// The content hash of the owned snapshot tree, present iff
        /// `owns_snapshot` is true.
        snapshot_hash: Option<Hash>,
    },

    /// Restore the code (and bound conversation) of an existing node
    /// (A.1 `node.restore`). A *mutation*: returns an `op_id`; the restore is
    /// recorded as [`OpLogEvent::RestorePerformed`](crate::OpLogEvent::RestorePerformed)
    /// (and a [`OpLogEvent::RefMoved`](crate::OpLogEvent::RefMoved)). Restore is
    /// an event, never an overwrite, so forward history survives (DESIGN.md
    /// §6.4, §10.3).
    NodeRestore {
        /// The node whose snapshot/conversation to materialize.
        node_id: Ulid,
    },

    /// Fork a new branch from a node (A.1 `branch.fork`). A *mutation*:
    /// metadata-only (zero bytes copied); returns an `op_id`; emits
    /// [`OpLogEvent::BranchForked`](crate::OpLogEvent::BranchForked) and
    /// [`OpLogEvent::RefCreated`](crate::OpLogEvent::RefCreated) (DESIGN.md
    /// §6.3, §10.3).
    BranchFork {
        /// The node the new branch's head points at.
        from_node_id: Ulid,
        /// Human-facing name of the new branch ref.
        name: String,
    },

    /// Create a new ref (branch/tag/head). A *mutation*: returns an `op_id`;
    /// emits [`OpLogEvent::RefCreated`](crate::OpLogEvent::RefCreated).
    RefCreate {
        /// The ref name.
        name: String,
        /// The kind of ref (DESIGN.md §6.3 `RefKind`).
        kind: RefKind,
        /// The node the new ref points at.
        to: Ulid,
    },

    /// Move an existing ref to a different node. A *mutation*: returns an
    /// `op_id`; emits [`OpLogEvent::RefMoved`](crate::OpLogEvent::RefMoved).
    RefMove {
        /// The ref name to move.
        name: String,
        /// The node the ref should now point at.
        to: Ulid,
    },

    /// Undo an operation (A.1 `op.undo`). A *mutation*: returns an `op_id`;
    /// emits [`OpLogEvent::OpUndone`](crate::OpLogEvent::OpUndone). `op_id`
    /// `None` undoes the most recent undoable op.
    OpUndo {
        /// The specific op to undo, or `None` for the latest.
        op_id: Option<Ulid>,
    },

    /// Redo an operation (A.1 `op.redo`). A *mutation*: returns an `op_id`;
    /// emits [`OpLogEvent::OpRedone`](crate::OpLogEvent::OpRedone). `op_id`
    /// `None` redoes the most recently undone op.
    OpRedo {
        /// The specific op to redo, or `None` for the latest.
        op_id: Option<Ulid>,
    },

    /// Run garbage collection (A.1 `gc.run`). A *mutation*: returns an `op_id`
    /// alongside the reclaimable report ([`CommandResult::Gc`](crate::CommandResult::Gc)),
    /// and (when not a dry run) emits
    /// [`OpLogEvent::GcPerformed`](crate::OpLogEvent::GcPerformed).
    GcRun {
        /// When true, compute the reclaimable set without deleting anything.
        dry_run: bool,
    },

    /// Compute the changed-path set of a node against a baseline (A.1
    /// `node.diff`). A **read**: returns [`CommandResult::Diff`](crate::CommandResult::Diff)
    /// inline; emits no events (DESIGN.md §14.5).
    NodeDiff {
        /// The node to diff.
        node_id: Ulid,
        /// The op/node id to diff against, or `None` to diff against the node's
        /// parent tree.
        against: Option<Ulid>,
    },

    /// Read a blob from a tree by path (A.1 `blob.read`). A **read**: returns
    /// [`CommandResult::Blob`](crate::CommandResult::Blob) inline; emits no
    /// events. Fetched lazily by the renderer as files open (DESIGN.md §14.5).
    BlobRead {
        /// The tree hash to read from.
        tree_hash: Hash,
        /// The path within that tree.
        path: String,
    },

    /// Run an observing check (Validation/Stress/Sanity) against a node (P5,
    /// DESIGN.md §6.5, §8.1, §8.2). A *mutation*: returns an `op_id`; the daemon
    /// runs the check via the F4 Runner SPI, stores an append-only
    /// `ResultEnvelope`, and attaches an **observing** result node to the target
    /// without mutating it — emitting
    /// [`OpLogEvent::ResultRecorded`](crate::OpLogEvent::ResultRecorded).
    NodeRunCheck {
        /// The node the check observes. Its snapshot is never mutated; the result
        /// attaches as an observing child (DESIGN.md §8.1).
        target_node_id: Ulid,
        /// The check specification (which built-in check, its command/config),
        /// resolved by the daemon against the registered node types. A free
        /// JSON value so new check shapes need no contract change (CLAUDE.md C3).
        spec: serde_json::Value,
    },

    /// Merge a branch into a target ref via 3-way reconciliation (P5, DESIGN.md
    /// §6.5). A *mutation*: returns an `op_id`; on a clean reconciliation the
    /// daemon creates a materializable Merge node (owns a snapshot) and emits
    /// [`OpLogEvent::MergePerformed`](crate::OpLogEvent::MergePerformed). On
    /// conflicts it returns the conflict set in the
    /// [`CommandResult`](crate::CommandResult) and builds **no** half-node.
    BranchMerge {
        /// The ref the merge result lands on.
        into_ref: String,
        /// The node carrying the changes being merged in.
        from_node_id: Ulid,
        /// An optional pre-supplied conflict resolution (e.g. from the F3-UI
        /// three-way resolver). `None` requests an automatic reconciliation;
        /// when conflicts remain the command returns a conflict set instead of
        /// merging. A free JSON value so the resolution schema can evolve
        /// without a contract change (CLAUDE.md C3).
        resolution: Option<serde_json::Value>,
    },

    /// Export a node's snapshot to a real Git commit on a new branch (F3-UI
    /// "Commit to GitHub", DESIGN.md §10.4). **Action-shaped, not a graph
    /// mutation**: it does not change the work-DAG, so it emits no
    /// [`OpLogEvent`](crate::OpLogEvent); it returns its result *inline* as
    /// [`CommandResult::Git`](crate::CommandResult::Git). The non-invasive bridge
    /// adds a new branch ref + objects to `.git` and never moves the user's
    /// HEAD/index/working tree.
    GitExport {
        /// The snapshot-owning node whose tree to project into a Git commit.
        node_id: Ulid,
        /// The branch name to create, or `None` to default to `spork/<nodeId>`.
        branch: Option<String>,
    },

    /// Push a node's exported branch to a Git remote (F3-UI "Push to GitHub",
    /// DESIGN.md §10.4). **Action-shaped, not a graph mutation**: it emits no
    /// [`OpLogEvent`](crate::OpLogEvent) and returns its result *inline* as
    /// [`CommandResult::Git`](crate::CommandResult::Git). Shells the system `git`
    /// so it uses the user's existing credentials; exports first if the branch is
    /// not yet present.
    GitPush {
        /// The snapshot-owning node whose exported branch to push.
        node_id: Ulid,
        /// The remote to push to, or `None` to default to `origin`.
        remote: Option<String>,
    },
}

impl Command {
    /// Whether this command is a *mutation* (returns an `op_id` and emits
    /// events) as opposed to a *read* / *action* (returns data inline, emits
    /// nothing). The reads ([`Command::NodeDiff`], [`Command::BlobRead`]) and the
    /// action-shaped git commands ([`Command::GitExport`], [`Command::GitPush`])
    /// are all non-mutations: they reply inline and never touch the event stream.
    ///
    /// This is the programmatic statement of the load-bearing rule, so a daemon
    /// or test can assert the right result/return shape per command without
    /// re-deriving the split by hand.
    #[must_use]
    pub fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Command::NodeDiff { .. }
                | Command::BlobRead { .. }
                | Command::GitExport { .. }
                | Command::GitPush { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    fn sample_mutations() -> Vec<Command> {
        let a = Ulid::new();
        let b = Ulid::new();
        vec![
            Command::NodeCreate {
                kind: "codebase-edit".into(),
                type_version: "1.0.0".into(),
                parent_ids: vec![a],
                branch_id: "main".into(),
                payload: serde_json::json!({"prompt": "do the thing"}),
                owns_snapshot: true,
                snapshot_hash: Some(hash_bytes(b"tree")),
            },
            Command::NodeRestore { node_id: a },
            Command::BranchFork {
                from_node_id: a,
                name: "experiment".into(),
            },
            Command::RefCreate {
                name: "v1".into(),
                kind: RefKind::Tag,
                to: b,
            },
            Command::RefMove {
                name: "main".into(),
                to: b,
            },
            Command::OpUndo { op_id: Some(a) },
            Command::OpRedo { op_id: None },
            Command::GcRun { dry_run: true },
            Command::NodeRunCheck {
                target_node_id: a,
                spec: serde_json::json!({"check": "validation", "command": "cargo test"}),
            },
            Command::BranchMerge {
                into_ref: "main".into(),
                from_node_id: b,
                resolution: None,
            },
            Command::BranchMerge {
                into_ref: "main".into(),
                from_node_id: b,
                resolution: Some(serde_json::json!({"hunks": []})),
            },
        ]
    }

    fn sample_reads() -> Vec<Command> {
        let a = Ulid::new();
        vec![
            Command::NodeDiff {
                node_id: a,
                against: None,
            },
            Command::BlobRead {
                tree_hash: hash_bytes(b"t"),
                path: "src/main.rs".into(),
            },
        ]
    }

    /// The F3-UI git actions: not graph mutations (they reply inline with a `Git`
    /// result and emit no events), so they round-trip and classify with the reads.
    fn sample_git_actions() -> Vec<Command> {
        let a = Ulid::new();
        vec![
            Command::GitExport {
                node_id: a,
                branch: None,
            },
            Command::GitExport {
                node_id: a,
                branch: Some("feature/x".into()),
            },
            Command::GitPush {
                node_id: a,
                remote: None,
            },
            Command::GitPush {
                node_id: a,
                remote: Some("upstream".into()),
            },
        ]
    }

    #[test]
    fn every_command_round_trips_through_json() {
        for cmd in sample_mutations()
            .into_iter()
            .chain(sample_reads())
            .chain(sample_git_actions())
        {
            let json = serde_json::to_string(&cmd).expect("serialize");
            let back: Command = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(cmd, back, "round-trip mismatch for {cmd:?}");
        }
    }

    #[test]
    fn wire_form_is_tagged_and_camel_cased() {
        let cmd = Command::NodeCreate {
            kind: "k".into(),
            type_version: "1.0.0".into(),
            parent_ids: vec![],
            branch_id: "main".into(),
            payload: serde_json::Value::Null,
            owns_snapshot: false,
            snapshot_hash: None,
        };
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        // Internally tagged on "command" with a SCREAMING_SNAKE_CASE tag.
        assert_eq!(v["command"], "NODE_CREATE");
        // Fields are camelCase to match the TS binding.
        assert!(v.get("typeVersion").is_some());
        assert!(v.get("parentIds").is_some());
        assert!(v.get("ownsSnapshot").is_some());
    }

    #[test]
    fn mutation_vs_read_classification_matches_contract() {
        for cmd in sample_mutations() {
            assert!(cmd.is_mutation(), "{cmd:?} should be a mutation");
        }
        for cmd in sample_reads() {
            assert!(!cmd.is_mutation(), "{cmd:?} should be a read");
        }
        // The git actions are not graph mutations: they emit no events.
        for cmd in sample_git_actions() {
            assert!(!cmd.is_mutation(), "{cmd:?} should not be a mutation");
        }
    }

    #[test]
    fn git_commands_use_tagged_camel_case_wire_form() {
        let node = Ulid::new();
        let export = Command::GitExport {
            node_id: node,
            branch: None,
        };
        let v = serde_json::to_value(&export).unwrap();
        assert_eq!(v["command"], "GIT_EXPORT");
        assert_eq!(v["nodeId"], node.to_string());
        assert!(v["branch"].is_null());

        let push = Command::GitPush {
            node_id: node,
            remote: Some("origin".into()),
        };
        let pv = serde_json::to_value(&push).unwrap();
        assert_eq!(pv["command"], "GIT_PUSH");
        assert_eq!(pv["nodeId"], node.to_string());
        assert_eq!(pv["remote"], "origin");
    }

    #[test]
    fn optional_op_id_omitted_field_deserializes() {
        // A renderer that sends `null` and one that sends the field both parse.
        let with_null = r#"{"command":"OP_UNDO","opId":null}"#;
        let parsed: Command = serde_json::from_str(with_null).unwrap();
        assert_eq!(parsed, Command::OpUndo { op_id: None });
    }

    #[test]
    fn p5_commands_use_tagged_camel_case_wire_form() {
        // NODE_RUN_CHECK carries the observed target and an opaque spec.
        let target = Ulid::new();
        let check = Command::NodeRunCheck {
            target_node_id: target,
            spec: serde_json::json!({"check": "sanity"}),
        };
        let v = serde_json::to_value(&check).unwrap();
        assert_eq!(v["command"], "NODE_RUN_CHECK");
        assert_eq!(v["targetNodeId"], target.to_string());
        assert_eq!(v["spec"]["check"], "sanity");

        // BRANCH_MERGE carries the into-ref, source node, and optional resolution.
        let from = Ulid::new();
        let merge = Command::BranchMerge {
            into_ref: "main".into(),
            from_node_id: from,
            resolution: None,
        };
        let mv = serde_json::to_value(&merge).unwrap();
        assert_eq!(mv["command"], "BRANCH_MERGE");
        assert_eq!(mv["intoRef"], "main");
        assert_eq!(mv["fromNodeId"], from.to_string());
        assert!(mv["resolution"].is_null());
    }

    #[test]
    fn p5_commands_are_mutations() {
        // Both new commands are mutations: they return an op_id and emit events.
        assert!(Command::NodeRunCheck {
            target_node_id: Ulid::new(),
            spec: serde_json::Value::Null,
        }
        .is_mutation());
        assert!(Command::BranchMerge {
            into_ref: "main".into(),
            from_node_id: Ulid::new(),
            resolution: None,
        }
        .is_mutation());
    }
}
