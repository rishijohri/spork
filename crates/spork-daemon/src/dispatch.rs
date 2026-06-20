//! The [`CommandHandler`] implementation — the single dispatch entry point that
//! ties every F3 subsystem together (DESIGN.md §5.5, §14.1, A.1).
//!
//! Each `dispatch` call follows the same shape, in order:
//!
//! 1. **Authorize.** Clear the command's required capability through the
//!    deny-by-default broker (DESIGN.md §15.1–§15.2). A denial appends an
//!    [`AuditEntry`](spork_broker::AuditEntry) and returns
//!    [`IpcError::Capability`] — the broker is the *only* path to a side effect,
//!    so a missing grant stops the command before it touches any state.
//! 2. **Perform.** For a mutation, append durable F1 events through the
//!    single-writer actor (via the graph service or the restore guard) and
//!    publish the renderer-facing [`OpLogEvent`]s onto the ordered rail. For a
//!    read, fetch the data lazily from the CAS.
//! 3. **Return.** A mutation returns only its `op_id` (+ any freshly-minted ids);
//!    its resulting state already left over the event stream. A read returns its
//!    data inline. This is the load-bearing rule the IPC contract freezes, and
//!    [`CommandResult::matches_command`] asserts the daemon honors it.
//!
//! Design references: DESIGN.md §5.5, §14.1, §14.4, §14.5, §15.1, §15.2, A.1.

use spork_broker::{Capability, RequestedScope};
use spork_graph::EdgeType;
use spork_ipc::{Command, CommandHandler, CommandResult, IpcError, OpLogEvent};
use ulid::Ulid;

use crate::core::{Daemon, WORKTREE_GLOB};
use crate::error::DaemonError;
use crate::read::{blob_read, node_diff};

impl CommandHandler for Daemon {
    /// Dispatch one command. See the module docs for the authorize → perform →
    /// return contract every arm follows.
    fn dispatch(&self, cmd: Command) -> Result<CommandResult, IpcError> {
        let result = self.dispatch_inner(cmd.clone())?;
        // The daemon never violates the load-bearing rule: a mutation replies
        // with only an op_id, a read with inline data. This is a cheap
        // belt-and-braces check of the contract the IPC crate freezes.
        debug_assert!(
            result.matches_command(&cmd),
            "dispatch produced a result shape that violates the IPC rule for {cmd:?}"
        );
        Ok(result)
    }
}

impl Daemon {
    /// The fallible body of [`CommandHandler::dispatch`], returning the daemon's
    /// internal error which the trait impl lowers to [`IpcError`].
    fn dispatch_inner(&self, cmd: Command) -> Result<CommandResult, DaemonError> {
        match cmd {
            Command::NodeCreate {
                kind,
                type_version,
                parent_ids,
                branch_id,
                payload,
                owns_snapshot,
                snapshot_hash,
            } => self.cmd_node_create(
                &kind,
                &type_version,
                parent_ids,
                &branch_id,
                payload,
                owns_snapshot,
                snapshot_hash,
            ),
            Command::NodeRestore { node_id } => self.cmd_node_restore(node_id),
            Command::BranchFork { from_node_id, name } => self.cmd_branch_fork(from_node_id, &name),
            Command::RefCreate { name, kind, to } => self.cmd_ref_create(&name, kind, to),
            Command::RefMove { name, to } => self.cmd_ref_move(&name, to),
            Command::OpUndo { op_id } => self.cmd_op_undo(op_id),
            Command::OpRedo { op_id } => self.cmd_op_redo(op_id),
            Command::GcRun { dry_run } => self.cmd_gc_run(dry_run),
            Command::NodeDiff { node_id, against } => self.cmd_node_diff(node_id, against),
            Command::BlobRead { tree_hash, path } => self.cmd_blob_read(tree_hash, &path),
            Command::NodeRunCheck {
                target_node_id,
                spec,
            } => self.cmd_node_run_check(target_node_id, spec),
            Command::BranchMerge {
                into_ref,
                from_node_id,
                resolution,
            } => self.cmd_branch_merge(&into_ref, from_node_id, resolution),
        }
    }

    /// Authorize a capability through the broker, recording an audit entry, and
    /// map a denial to [`DaemonError::Capability`].
    ///
    /// `scope` is the concrete action being requested (the path for a
    /// snapshot op, etc.); the broker checks it against the granted scope and
    /// appends an [`AuditEntry`](spork_broker::AuditEntry) for *every* call —
    /// allow or deny (DESIGN.md §15.2). Holds the core lock for the check so the
    /// audit trail advances in dispatch order.
    pub(crate) fn authorize(
        &self,
        cap: Capability,
        scope: &RequestedScope,
    ) -> Result<(), DaemonError> {
        let mut core = self.core.lock().expect("daemon core mutex poisoned");
        core.broker
            .authorize(cap, scope)
            .map(|_token| ())
            .map_err(|e| DaemonError::Capability(e.to_string()))
    }

    /// `node.create`: validate + append through the graph service, then publish
    /// `NODE_CREATED` and one `EDGE_ADDED` per parent (DESIGN.md A.1).
    #[allow(clippy::too_many_arguments)]
    fn cmd_node_create(
        &self,
        kind: &str,
        type_version: &str,
        parent_ids: Vec<Ulid>,
        branch_id: &str,
        payload: serde_json::Value,
        owns_snapshot: bool,
        snapshot_hash: Option<spork_hash::Hash>,
    ) -> Result<CommandResult, DaemonError> {
        // Creating a node writes a snapshot binding — a snapshot.write op.
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        let version = parse_version(type_version)?;
        // Keep a copy of the payload for the Edit auto-run hook below; the
        // original is moved into the graph service.
        let payload_for_hook = payload.clone();
        let envelope = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.graph
                .create_node(
                    kind,
                    Some(&version),
                    parent_ids.clone(),
                    branch_id,
                    payload,
                    owns_snapshot,
                    snapshot_hash,
                )
                .map_err(|e| DaemonError::Graph(e.to_string()))?
        };

        let node_id = envelope.id;
        // The resulting state flows out on the ordered stream, never on the
        // mutation's return value.
        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id,
            schema_version: envelope.payload_schema_version,
        })?;
        for parent in &parent_ids {
            let parent = *parent;
            self.publish_event(|seq| OpLogEvent::EdgeAdded {
                seq,
                from: parent,
                to: node_id,
                edge: EdgeType::ParentChild,
            })?;
        }

        // P5 Edit auto-run hook (DESIGN §8.2 `onEditNodeCommitted`): creating an
        // Edit (a mutating node) auto-schedules a change-scoped Sanity check
        // against it, which cache-hits on an unchanged subtree. Best-effort and
        // observing — a check failure never fails the Edit creation. Additive
        // behind the frozen F3 create path (CLAUDE.md C3).
        self.maybe_auto_run_sanity(node_id, kind, &payload_for_hook)?;

        Ok(self.record_mutation(serde_json::json!({ "nodeId": node_id.to_string() })))
    }

    /// `node.restore`: atomic dual-restore through the guard, fail-closed; on
    /// success publish `RESTORE_PERFORMED` and `REF_MOVED` (HEAD). Forward
    /// history survives — restore is an event, not an overwrite (DESIGN.md §6.4,
    /// §10.3).
    fn cmd_node_restore(&self, node_id: Ulid) -> Result<CommandResult, DaemonError> {
        // Restore materializes the snapshot into the working dir — snapshot.write.
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        let outcome = self.guarded_restore(node_id)?;
        debug_assert_eq!(outcome.node_id, node_id);

        self.publish_event(|seq| OpLogEvent::RestorePerformed { seq, node_id })?;
        self.publish_event(|seq| OpLogEvent::RefMoved {
            seq,
            ref_name: "HEAD".to_string(),
            to: node_id,
        })?;

        Ok(self.record_mutation(serde_json::json!({ "nodeId": node_id.to_string() })))
    }

    /// `branch.fork`: metadata-only new branch ref at a node (zero bytes); on
    /// success publish `BRANCH_FORKED` and `REF_CREATED` (DESIGN.md §6.3, §10.3).
    fn cmd_branch_fork(
        &self,
        from_node_id: Ulid,
        name: &str,
    ) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        let ref_id = self.guarded_branch_fork(from_node_id, name)?;
        let ref_name = ref_id.as_str().to_string();

        let forked = ref_name.clone();
        self.publish_event(|seq| OpLogEvent::BranchForked {
            seq,
            ref_name: forked,
        })?;
        let created = ref_name.clone();
        self.publish_event(|seq| OpLogEvent::RefCreated {
            seq,
            ref_name: created,
        })?;

        Ok(self.record_mutation(serde_json::json!({ "refId": ref_name })))
    }

    /// Create a ref through the graph service; publish `REF_CREATED`.
    fn cmd_ref_create(
        &self,
        name: &str,
        kind: spork_graph::RefKind,
        to: Ulid,
    ) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.graph
                .create_ref(name, kind, to)
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
        }
        let ref_name = name.to_string();
        self.publish_event(|seq| OpLogEvent::RefCreated { seq, ref_name })?;
        Ok(self.record_mutation(serde_json::json!({ "refId": name })))
    }

    /// Move a ref through the graph service; publish `REF_MOVED`.
    fn cmd_ref_move(&self, name: &str, to: Ulid) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.graph
                .move_ref(name, to)
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
        }
        let ref_name = name.to_string();
        self.publish_event(|seq| OpLogEvent::RefMoved { seq, ref_name, to })?;
        Ok(self.record_mutation(serde_json::json!({ "refId": name })))
    }

    /// `op.undo`: move the op cursor back and publish `OP_UNDONE`.
    ///
    /// Undo does not delete graph state (restore is the state-moving primitive
    /// and forward history always survives — DESIGN.md §6.4, §10.3); the
    /// `OP_UNDONE` event is the durable record of the cursor move. A bare undo
    /// (no id) targets the most recent op; an explicit id targets that op.
    fn cmd_op_undo(&self, op_id: Option<Ulid>) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        let undone = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.ops.undo(op_id)
        };
        let undone = undone
            .ok_or_else(|| DaemonError::NotFound("no matching operation to undo".to_string()))?;
        self.publish_event(|seq| OpLogEvent::OpUndone { seq })?;
        Ok(CommandResult::Mutation {
            op_id: undone,
            ids: serde_json::json!({ "undone": undone.to_string() }),
        })
    }

    /// `op.redo`: move the op cursor forward and publish `OP_REDONE`.
    fn cmd_op_redo(&self, op_id: Option<Ulid>) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        let redone = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.ops.redo(op_id)
        };
        let redone = redone
            .ok_or_else(|| DaemonError::NotFound("no matching operation to redo".to_string()))?;
        self.publish_event(|seq| OpLogEvent::OpRedone { seq })?;
        Ok(CommandResult::Mutation {
            op_id: redone,
            ids: serde_json::json!({ "redone": redone.to_string() }),
        })
    }

    /// `gc.run`: compute the conservatively-reclaimable set and (when not a dry
    /// run) publish `GC_PERFORMED`.
    ///
    /// GC is reachability-aware mark-sweep (DESIGN.md §6.4, A.2) and is
    /// **conservative by default** — a reachability bug is silent data loss, so
    /// the v1 headless GC reports nothing as reclaimable (every object is treated
    /// as reachable from a live node/ref) rather than risk reclaiming a
    /// restorable state. It still records the op as a durable `GC_PERFORMED`
    /// event when run for real, so the timeline reflects that GC ran.
    fn cmd_gc_run(&self, dry_run: bool) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        // Conservative: no object is reported reclaimable in this headless slice.
        let reclaimable: Vec<String> = Vec::new();
        let bytes = 0u64;
        if !dry_run {
            self.publish_event(|seq| OpLogEvent::GcPerformed { seq })?;
        }
        Ok(CommandResult::Gc { reclaimable, bytes })
    }

    /// `node.diff` (read): the changed-path set, inline, no events.
    fn cmd_node_diff(
        &self,
        node_id: Ulid,
        against: Option<Ulid>,
    ) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotRead,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let changed_paths = node_diff(&core, node_id, against)?;
        Ok(CommandResult::Diff { changed_paths })
    }

    /// `blob.read` (read): one blob's bytes, inline, no events.
    ///
    /// Authorized against the CONCRETE path being read (not the whole-tree glob),
    /// so the broker can enforce per-path read narrowing and the `AuditEntry`
    /// records the real path (DESIGN §15.2). Content reads are the per-path
    /// case; `node.diff` below is a node-level changed-path metadata read.
    fn cmd_blob_read(
        &self,
        tree_hash: spork_hash::Hash,
        path: &str,
    ) -> Result<CommandResult, DaemonError> {
        self.authorize(Capability::SnapshotRead, &RequestedScope::path(path))?;
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let bytes = blob_read(&core.store, tree_hash, path)?;
        Ok(CommandResult::Blob { bytes })
    }

    /// Mint a fresh `op_id`, record it on the undo/redo cursor, and wrap it (with
    /// the given minted-id bag) as a [`CommandResult::Mutation`].
    ///
    /// Centralizing this guarantees every mutation returns the *same* shape (an
    /// op_id + ids, never resulting state) and that every performed op is on the
    /// undo cursor.
    pub(crate) fn record_mutation(&self, ids: serde_json::Value) -> CommandResult {
        let op_id = Ulid::new();
        {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            core.ops.push(op_id);
        }
        CommandResult::Mutation { op_id, ids }
    }
}

/// Parse a semver string from the IPC `type_version` field.
fn parse_version(s: &str) -> Result<semver::Version, DaemonError> {
    semver::Version::parse(s)
        .map_err(|e| DaemonError::Graph(format!("invalid type_version {s:?}: {e}")))
}
