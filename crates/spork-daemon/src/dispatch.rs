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
            Command::GitExport { node_id, branch } => self.cmd_git_export(node_id, branch),
            Command::GitPush { node_id, remote } => self.cmd_git_push(node_id, remote),
            Command::NodeAgentRun {
                target_node_id,
                prompt,
                model_key,
                privacy,
                intent,
            } => self.cmd_node_agent_run(target_node_id, &prompt, &model_key, &privacy, intent),
            Command::BranchMergeGated {
                into_ref,
                from_node_id,
                resolution,
                gate,
                baseline,
                override_reason,
            } => self.cmd_branch_merge_gated(
                &into_ref,
                from_node_id,
                resolution,
                gate,
                baseline,
                override_reason,
            ),
            Command::NodeCheckout { node_id } => self.cmd_node_checkout(node_id),
            Command::NodeContext { node_id } => self.cmd_node_context(node_id),
            Command::NodeHandoff { node_id } => self.cmd_node_handoff(node_id),
            Command::HistoryQuery { request } => self.cmd_history_query(request),
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

    /// `git.export` (action): project a node's snapshot into a real Git commit on
    /// a new branch, returning the branch/commit inline as
    /// [`CommandResult::Git`] (DESIGN.md §10.4).
    ///
    /// This is **not** a graph mutation: the export adds a branch ref + immutable
    /// objects to the user's `.git` but changes no Spork work-DAG state, so it
    /// emits no [`OpLogEvent`] and returns its result inline (the load-bearing
    /// rule's read/action side). It authorizes [`Capability::SnapshotRead`] (it
    /// reads the node's snapshot tree from the CAS), resolves the node, requires
    /// it owns a snapshot, and exports to `branch` (defaulting to
    /// `spork/<nodeId>`). The repo is the daemon's working tree (where the user's
    /// `.git` lives); the bridge leaves HEAD/index/working tree byte-unchanged.
    fn cmd_git_export(
        &self,
        node_id: Ulid,
        branch: Option<String>,
    ) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotRead,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;
        let branch = branch.unwrap_or_else(|| default_export_branch(node_id));
        let commit_sha = self.git_export_branch(node_id, &branch)?;
        Ok(CommandResult::Git {
            branch,
            commit_sha,
            pushed: false,
        })
    }

    /// `git.push` (action): export the node's snapshot to a branch if needed, then
    /// push that branch to a remote through the system `git`, returning the
    /// branch/commit inline with `pushed = true` (DESIGN.md §10.4).
    ///
    /// This is **not** a graph mutation (no [`OpLogEvent`]). It authorizes
    /// [`Capability::NetConnect`] — pushing reaches the network — *and* exports
    /// first (which itself reads the snapshot), so the push always has a branch to
    /// publish. The push shells the user's `git` so it uses their existing
    /// credentials (Spork holds no token, DESIGN.md §15.4). `remote` defaults to
    /// `origin`.
    fn cmd_git_push(
        &self,
        node_id: Ulid,
        remote: Option<String>,
    ) -> Result<CommandResult, DaemonError> {
        let remote = remote.unwrap_or_else(|| "origin".to_string());
        let branch = default_export_branch(node_id);

        // A push reaches the network. `net.connect` is host-scoped, so resolve the
        // concrete host from the remote's URL and authorize against it — the
        // broker enforces the per-host allowlist (DESIGN.md §15.1, §15.2).
        let host = self.git_remote_host(&remote)?;
        self.authorize(Capability::NetConnect, &RequestedScope::host(host))?;

        // Export the branch first so there is always something to push. A repeat
        // export onto an existing branch name fails in the non-invasive bridge;
        // treat that as "already exported" and push the existing branch.
        let commit_sha = match self.git_export_branch(node_id, &branch) {
            Ok(sha) => sha,
            Err(DaemonError::Git(_)) => self.git_branch_commit_sha(&branch)?,
            Err(e) => return Err(e),
        };

        let workdir = self.workdir();
        spork_git::push_branch(&workdir, &remote, &branch)
            .map_err(|e| DaemonError::Git(e.to_string()))?;

        Ok(CommandResult::Git {
            branch,
            commit_sha,
            pushed: true,
        })
    }

    /// Resolve a node to its snapshot tree and export it to `branch`, returning the
    /// new commit SHA. Shared by [`cmd_git_export`](Self::cmd_git_export) and
    /// [`cmd_git_push`](Self::cmd_git_push). Holds the core lock for the CAS read
    /// + git write, exactly like the other snapshot-reading dispatch arms.
    fn git_export_branch(&self, node_id: Ulid, branch: &str) -> Result<String, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let env = core
            .graph
            .get_node(node_id)
            .map_err(|e| DaemonError::Graph(e.to_string()))?
            .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
        let snapshot_hash = env
            .snapshot_hash
            .ok_or_else(|| DaemonError::NotFound(format!("node {node_id} owns no snapshot")))?;
        spork_git::export_snapshot_to_git(&core.workdir, &core.store, snapshot_hash, branch)
            .map_err(|e| DaemonError::Git(e.to_string()))
    }

    /// Read the commit SHA an already-exported `branch` points at (for a push that
    /// re-uses a branch a prior export created). Uses the daemon's working tree as
    /// the repo.
    fn git_branch_commit_sha(&self, branch: &str) -> Result<String, DaemonError> {
        let workdir = self.workdir();
        let repo = git2::Repository::open(&workdir)
            .map_err(|e| DaemonError::Git(format!("open repo {workdir:?}: {e}")))?;
        let reference = repo
            .find_reference(&format!("refs/heads/{branch}"))
            .map_err(|e| DaemonError::Git(format!("find branch {branch}: {e}")))?;
        let commit = reference
            .peel_to_commit()
            .map_err(|e| DaemonError::Git(format!("peel branch {branch}: {e}")))?;
        Ok(commit.id().to_string())
    }

    /// Resolve the network host a `git push` to `remote` will reach, for the
    /// host-scoped `net.connect` authorization.
    ///
    /// Reads the remote's configured URL from the daemon's working-tree repo and
    /// extracts its host. A local/file remote (e.g. a `file://` path or a bare
    /// repo on disk, as the tests use) has no network host, so it resolves to
    /// `"localhost"` — the broker still gates it, just against the local host. An
    /// unknown remote is a [`DaemonError::Git`].
    fn git_remote_host(&self, remote: &str) -> Result<String, DaemonError> {
        let workdir = self.workdir();
        let repo = git2::Repository::open(&workdir)
            .map_err(|e| DaemonError::Git(format!("open repo {workdir:?}: {e}")))?;
        let found = repo
            .find_remote(remote)
            .map_err(|e| DaemonError::Git(format!("find remote {remote:?}: {e}")))?;
        let url = found
            .url()
            .map_err(|e| DaemonError::Git(format!("remote {remote:?} url: {e}")))?;
        Ok(remote_url_host(url))
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

/// The default git branch name a node's snapshot exports to: `spork/<nodeId>`.
///
/// Both `git.export` (when `branch` is `None`) and `git.push` use this, so a push
/// publishes the same branch a prior bare export created (DESIGN.md §10.4).
fn default_export_branch(node_id: Ulid) -> String {
    format!("spork/{node_id}")
}

/// Extract the network host from a git remote URL for `net.connect` scoping.
///
/// Handles the common forms:
/// - `https://github.com/owner/repo.git` → `github.com`
/// - `ssh://git@github.com:22/owner/repo.git` → `github.com`
/// - the scp-style `git@github.com:owner/repo.git` → `github.com`
/// - a local/file remote (`/path/to/repo.git`, `file:///path`, `../repo`) → has
///   no network host, so it resolves to `localhost`.
fn remote_url_host(url: &str) -> String {
    const LOCAL: &str = "localhost";

    // A local file path or explicit file:// URL is not a network host.
    if url.starts_with("file://") || url.starts_with('/') || url.starts_with('.') {
        return LOCAL.to_string();
    }

    // scheme://[user@]host[:port]/path
    if let Some((_scheme, rest)) = url.split_once("://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        let after_user = authority.rsplit('@').next().unwrap_or(authority);
        let host = after_user.split(':').next().unwrap_or(after_user);
        return if host.is_empty() {
            LOCAL.to_string()
        } else {
            host.to_string()
        };
    }

    // scp-style: [user@]host:path (the colon separates host from path).
    if let Some((before_colon, _path)) = url.split_once(':') {
        let host = before_colon.rsplit('@').next().unwrap_or(before_colon);
        if !host.is_empty() {
            return host.to_string();
        }
    }

    LOCAL.to_string()
}

#[cfg(test)]
mod tests {
    use super::{default_export_branch, remote_url_host};
    use ulid::Ulid;

    #[test]
    fn default_export_branch_is_spork_prefixed() {
        let id = Ulid::new();
        assert_eq!(default_export_branch(id), format!("spork/{id}"));
    }

    #[test]
    fn remote_url_host_extracts_the_network_host() {
        assert_eq!(
            remote_url_host("https://github.com/owner/repo.git"),
            "github.com"
        );
        assert_eq!(
            remote_url_host("https://user@gitlab.com:443/owner/repo.git"),
            "gitlab.com"
        );
        assert_eq!(
            remote_url_host("ssh://git@github.com:22/owner/repo.git"),
            "github.com"
        );
        // scp-style.
        assert_eq!(
            remote_url_host("git@github.com:owner/repo.git"),
            "github.com"
        );
    }

    #[test]
    fn remote_url_host_maps_local_remotes_to_localhost() {
        assert_eq!(remote_url_host("/srv/git/repo.git"), "localhost");
        assert_eq!(remote_url_host("file:///srv/git/repo.git"), "localhost");
        assert_eq!(remote_url_host("../sibling/repo.git"), "localhost");
    }
}
