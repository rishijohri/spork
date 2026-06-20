//! The atomic dual-restore guard: [`RestoreGuard`].
//!
//! The guard is the one real implementation (CLAUDE.md C3) of the §6.4 / §10.3
//! restore guarantee: restoring a node moves its **code snapshot and bound
//! conversation together, or neither**. Both happen under a single lock, with
//! all verification done *before* any byte is written, so a divergence (a
//! missing snapshot, a missing/mismatched conversation, an absent node) leaves
//! the working directory and refs byte-for-byte unchanged — the guard **fails
//! closed** (DESIGN §6.4 invariant 1).
//!
//! Restore is recorded as an *event* (`restore.performed`) through the F1
//! writer, not as an overwrite: the node that was forward of the restore target
//! is never deleted, so **forward history survives** as a sibling and a bad
//! restore is itself just another op (DESIGN §6.4, §10.1, §10.3).
//!
//! [`branch_fork`](RestoreGuard::branch_fork) is **metadata-only**: it creates a
//! new branch [`RefId`](crate::RefId) pointing at an existing node and copies
//! **zero bytes** (DESIGN §6.3 — "a `forkBranch` creates a head `Ref` and copies
//! no code until checkout").
//!
//! Design references: DESIGN.md §6.3, §6.4, §10.1, §10.3, §11.4.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::json;
use spork_cas::{ObjectStore, StorageBackend};
use spork_graph::{GraphService, NodeEnvelope, RefKind};
use spork_hash::Hash;
use spork_log::{NewEvent, WriterHandle};
use ulid::Ulid;

use crate::error::RestoreError;
use crate::outcome::{RefId, RestoreOutcome};

/// The event type recorded when a restore succeeds. Appended through the F1
/// writer so the restore is a durable, ordered op (DESIGN §10.1).
pub const EVENT_RESTORE_PERFORMED: &str = "restore.performed";

/// The schema version of the `restore.performed` event payload (CLAUDE.md C5).
pub const RESTORE_EVENT_SCHEMA_VERSION: u16 = 1;

/// The actor string the guard stamps on the events it appends.
const RESTORE_ACTOR: &str = "spork-restore";

/// The default node-payload field carrying the bound conversation ref.
///
/// A mutating node binds a conversation/context ref alongside its code snapshot
/// (DESIGN §7.2). The canonical transcript schema is an F4 concern; here only
/// the *ref slot* is frozen, read from this payload field (a 64-char lowercase
/// hex BLAKE3 digest, the §10.3 `ConversationPointer`).
pub const DEFAULT_CONVERSATION_REF_FIELD: &str = "conversationRef";

/// The mutable state the guard serializes behind one lock.
///
/// Holding the [`GraphService`] (the validated command + projection layer), the
/// content [`ObjectStore`], and the working directory together lets the guard run
/// the whole dual restore — verify snapshot, verify conversation, materialize,
/// move `HEAD`, record the event — as one critical section, which is what makes
/// the §10.3 atomicity guarantee real rather than aspirational.
struct GuardState<S: StorageBackend> {
    graph: GraphService,
    store: ObjectStore<S>,
    /// The working directory the restore materializes the snapshot into.
    workdir: PathBuf,
    /// The F1 single write path, used to append the `restore.performed` event.
    writer: WriterHandle,
    /// The node-payload field name that carries the bound conversation ref.
    conversation_ref_field: String,
}

/// The atomic dual-restore guard.
///
/// Owns the graph service, the content store, and the target working directory,
/// and serializes [`restore`](RestoreGuard::restore) and
/// [`branch_fork`](RestoreGuard::branch_fork) behind a single lock so a restore
/// is transactional across code and conversation.
///
/// The guard is generic over the content [`StorageBackend`]; v1 ships over
/// `spork_cas::LooseStore`.
pub struct RestoreGuard<S: StorageBackend> {
    state: Mutex<GuardState<S>>,
}

impl<S: StorageBackend + Sync> RestoreGuard<S> {
    /// Construct a guard over a graph service, content store, working directory,
    /// and the F1 writer handle.
    ///
    /// The conversation ref is read from the node payload's
    /// [`DEFAULT_CONVERSATION_REF_FIELD`]; use
    /// [`with_conversation_ref_field`](RestoreGuard::with_conversation_ref_field)
    /// to bind a different field name.
    #[must_use]
    pub fn new(
        graph: GraphService,
        store: ObjectStore<S>,
        workdir: impl Into<PathBuf>,
        writer: WriterHandle,
    ) -> Self {
        RestoreGuard {
            state: Mutex::new(GuardState {
                graph,
                store,
                workdir: workdir.into(),
                writer,
                conversation_ref_field: DEFAULT_CONVERSATION_REF_FIELD.to_string(),
            }),
        }
    }

    /// Override the node-payload field name that carries the bound conversation
    /// ref (the default is [`DEFAULT_CONVERSATION_REF_FIELD`]).
    #[must_use]
    pub fn with_conversation_ref_field(self, field: impl Into<String>) -> Self {
        {
            let mut guard = self
                .state
                .lock()
                .expect("restore guard mutex poisoned during configuration");
            guard.conversation_ref_field = field.into();
        }
        self
    }

    /// Borrow the graph service for read queries (under the guard's lock).
    ///
    /// A small accessor so a caller can inspect the post-restore graph (e.g.
    /// assert forward history survived) without a second service handle.
    ///
    /// # Panics
    /// Panics if the guard lock is poisoned by a prior panic while held.
    pub fn with_graph<R>(&self, f: impl FnOnce(&GraphService) -> R) -> R {
        let guard = self.state.lock().expect("restore guard mutex poisoned");
        f(&guard.graph)
    }

    /// Atomically restore a node's code snapshot **and** bound conversation.
    ///
    /// The whole operation runs under one lock (DESIGN §10.3). The order is
    /// **verify-everything-then-mutate**, which is what makes the guard
    /// fail-closed:
    ///
    /// 1. Resolve the node envelope; a mutating node must carry a
    ///    `snapshot_hash`. A missing node or a node with no snapshot is a
    ///    [`RestoreError::Divergence`].
    /// 2. Confirm the code snapshot object is present in the content store, else
    ///    [`RestoreError::MissingSnapshot`].
    /// 3. Read the node payload's bound conversation ref (if any) and confirm it
    ///    resolves in the content store, else [`RestoreError::MissingConversation`].
    /// 4. Only now materialize the snapshot — into a fresh staging dir, then swap
    ///    it into the working directory in one move, so a materialize fault still
    ///    leaves the working dir untouched.
    /// 5. Move `HEAD` onto the restored node (a recorded `graph.ref_moved`) and
    ///    append a `restore.performed` event through the F1 writer, so the restore
    ///    is a durable op and **forward history survives** (the node is never
    ///    deleted).
    ///
    /// Returns a [`RestoreOutcome`] carrying the restored refs and the
    /// shaped-but-empty effects-log slot (DESIGN §11.4).
    ///
    /// # Errors
    /// Any divergence (steps 1–3) returns the corresponding [`RestoreError`] with
    /// the working directory and refs **unchanged**. A failure during or after
    /// materialization (steps 4–5) is reported as the precise underlying error;
    /// because object content is immutable and `HEAD`/the event move last, a
    /// post-swap fault still leaves a consistent, restorable graph.
    pub fn restore(&self, node_id: Ulid) -> Result<RestoreOutcome, RestoreError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| RestoreError::Log("restore guard mutex poisoned".to_string()))?;

        // ---- Phase 1: verify everything before touching any byte. -----------
        let node = resolve_node(&guard.graph, node_id)?;
        let snapshot_hash = require_snapshot(&node)?;
        verify_present(&guard.store, &snapshot_hash, Presence::Snapshot)?;

        let conversation =
            read_conversation_ref(&node, &guard.conversation_ref_field, &guard.graph)?;
        if let Some(conv) = conversation {
            verify_present(&guard.store, &conv, Presence::Conversation)?;
        }

        // ---- Phase 2: materialize into a staging dir, then swap atomically. --
        // Verification has passed; this is the first mutation, and it lands in a
        // sibling staging dir so a fault here still leaves `workdir` intact.
        let workdir = guard.workdir.clone();
        materialize_into_workdir(&guard.store, &snapshot_hash, &workdir)?;

        // ---- Phase 3: move HEAD + record the restore as an event. -----------
        // `move_ref` records a `graph.ref_moved`; we then append the
        // `restore.performed` op. The restore target is unchanged in the graph
        // and any node forward of it still exists — forward history survives.
        move_head(&mut guard.graph, node_id)?;
        append_restore_event(
            &guard.writer,
            node_id,
            &snapshot_hash,
            conversation.as_ref(),
        )?;

        Ok(RestoreOutcome::new(node_id, snapshot_hash, conversation))
    }

    /// Create a new branch ref at an existing node — **metadata only**.
    ///
    /// A fork is a pointer, not a checkout: it creates a [`RefKind::Branch`] ref
    /// named `name` pointing at `from_node_id` and copies **zero bytes** (DESIGN
    /// §6.3). The new ref is recorded as a `graph.ref_created` event (refs are
    /// events, so pointer history is recoverable — §6.3).
    ///
    /// # Errors
    /// - [`RestoreError::Divergence`] if `from_node_id` does not exist.
    /// - [`RestoreError::Log`] if the underlying ref-create append fails.
    pub fn branch_fork(&self, from_node_id: Ulid, name: &str) -> Result<RefId, RestoreError> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| RestoreError::Log("restore guard mutex poisoned".to_string()))?;

        // Verify the source node exists; a fork off nothing is a divergence.
        resolve_node(&guard.graph, from_node_id)?;

        guard
            .graph
            .create_ref(name, RefKind::Branch, from_node_id)
            .map_err(|e| RestoreError::Log(format!("branch_fork create_ref: {e}")))?;

        Ok(RefId(name.to_string()))
    }
}

/// Which kind of object a presence check concerns, so a missing object maps to
/// the right [`RestoreError`] variant.
#[derive(Clone, Copy)]
enum Presence {
    Snapshot,
    Conversation,
}

/// Resolve a node envelope, mapping absence to a divergence (fail-closed).
fn resolve_node(graph: &GraphService, node_id: Ulid) -> Result<NodeEnvelope, RestoreError> {
    match graph.get_node(node_id) {
        Ok(Some(node)) => Ok(node),
        Ok(None) => Err(RestoreError::Divergence {
            detail: format!("node {node_id} does not exist"),
        }),
        Err(e) => Err(RestoreError::Log(format!("get_node {node_id}: {e}"))),
    }
}

/// Require that a node owns a code snapshot and return its hash.
///
/// A node that does not own a snapshot (an observing/context node), or a mutating
/// node missing its `snapshot_hash`, is not restorable as code state — a
/// divergence, not a silent partial restore (DESIGN §6.4).
fn require_snapshot(node: &NodeEnvelope) -> Result<Hash, RestoreError> {
    if !node.owns_snapshot {
        return Err(RestoreError::Divergence {
            detail: format!(
                "node {} (kind {}) does not own a snapshot; only mutating nodes are restorable",
                node.id, node.kind
            ),
        });
    }
    node.snapshot_hash.ok_or_else(|| RestoreError::Divergence {
        detail: format!(
            "node {} claims to own a snapshot but carries no snapshot_hash",
            node.id
        ),
    })
}

/// Confirm an object is present in the content store, else fail closed with the
/// presence-specific error.
fn verify_present<S: StorageBackend>(
    store: &ObjectStore<S>,
    hash: &Hash,
    which: Presence,
) -> Result<(), RestoreError> {
    let present = store
        .backend()
        .has(hash)
        .map_err(|e| RestoreError::Log(format!("content-store presence check {hash}: {e}")))?;
    if present {
        return Ok(());
    }
    Err(match which {
        Presence::Snapshot => RestoreError::MissingSnapshot(*hash),
        Presence::Conversation => RestoreError::MissingConversation(*hash),
    })
}

/// Read the node's bound conversation ref from its payload, if any.
///
/// The ref lives in the payload field configured on the guard (default
/// [`DEFAULT_CONVERSATION_REF_FIELD`]). An absent field means the node binds no
/// conversation (a snapshot/import node) — that is `Ok(None)`, not an error. A
/// *present-but-malformed* ref is a divergence: the binding is corrupt and the
/// guard must not proceed (fail-closed).
fn read_conversation_ref(
    node: &NodeEnvelope,
    field: &str,
    graph: &GraphService,
) -> Result<Option<Hash>, RestoreError> {
    let payload = match graph.get_payload(node.id) {
        Ok(Some((payload, _version))) => payload,
        Ok(None) => return Ok(None),
        Err(e) => return Err(RestoreError::Log(format!("get_payload {}: {e}", node.id))),
    };

    let Some(raw) = payload.get(field) else {
        return Ok(None);
    };
    // A JSON null in the slot is an explicit "no conversation bound".
    if raw.is_null() {
        return Ok(None);
    }
    let Some(hex) = raw.as_str() else {
        return Err(RestoreError::Divergence {
            detail: format!(
                "node {} field `{field}` is present but not a hex digest string",
                node.id
            ),
        });
    };
    Hash::from_hex(hex)
        .map(Some)
        .map_err(|e| RestoreError::Divergence {
            detail: format!(
                "node {} bound conversation ref `{hex}` is malformed: {e}",
                node.id
            ),
        })
}

/// Materialize a snapshot's root tree into `workdir`, atomically.
///
/// To keep the restore fail-closed up to the final move, the snapshot is
/// materialized into a fresh sibling staging directory first; only once that
/// fully succeeds is the existing working directory replaced by it with a rename.
/// A fault while staging therefore never corrupts the live working directory.
fn materialize_into_workdir<S: StorageBackend + Sync>(
    store: &ObjectStore<S>,
    snapshot: &Hash,
    workdir: &Path,
) -> Result<(), RestoreError> {
    let parent = workdir
        .parent()
        .ok_or_else(|| RestoreError::Log(format!("workdir {workdir:?} has no parent directory")))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| RestoreError::Log(format!("create workdir parent {parent:?}: {e}")))?;

    // A staging dir alongside the workdir so the final swap is a same-filesystem
    // rename. The file name embeds the snapshot hash to avoid collisions.
    let file_name = workdir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workdir");
    let staging = parent.join(format!(".{file_name}.restore-staging.{snapshot}"));
    // Clear any stale staging dir from a previous interrupted restore.
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| RestoreError::Log(format!("clear stale staging {staging:?}: {e}")))?;
    }

    store
        .materialize_snapshot(snapshot, &staging)
        .map_err(|e| {
            RestoreError::Log(format!("materialize snapshot {snapshot} -> staging: {e}"))
        })?;

    // Swap: move the old workdir aside, move staging into place, drop the old
    // one. Each step is a rename (cheap, atomic on the same filesystem); the
    // window where neither exists at `workdir` is a single rename wide.
    let backup = parent.join(format!(".{file_name}.restore-backup.{snapshot}"));
    if backup.exists() {
        std::fs::remove_dir_all(&backup)
            .map_err(|e| RestoreError::Log(format!("clear stale backup {backup:?}: {e}")))?;
    }
    let had_workdir = workdir.exists();
    if had_workdir {
        std::fs::rename(workdir, &backup)
            .map_err(|e| RestoreError::Log(format!("move old workdir aside: {e}")))?;
    }
    if let Err(e) = std::fs::rename(&staging, workdir) {
        // Restore the previous working dir so we still leave a consistent state.
        if had_workdir {
            let _ = std::fs::rename(&backup, workdir);
        }
        return Err(RestoreError::Log(format!(
            "swap staged restore into workdir: {e}"
        )));
    }
    if had_workdir {
        std::fs::remove_dir_all(&backup)
            .map_err(|e| RestoreError::Log(format!("remove old workdir backup: {e}")))?;
    }
    Ok(())
}

/// Move the working head (`HEAD`) onto the restored node, creating it if absent.
///
/// `HEAD` may not yet exist (a fresh graph with no head pointer); in that case we
/// create it rather than fail, since restore is the act that establishes the
/// working head.
fn move_head(graph: &mut GraphService, node_id: Ulid) -> Result<(), RestoreError> {
    let exists = graph
        .projection()
        .ref_target("HEAD")
        .map_err(|e| RestoreError::Log(format!("read HEAD ref: {e}")))?
        .is_some();
    let result = if exists {
        graph.move_ref("HEAD", node_id)
    } else {
        graph.create_ref("HEAD", RefKind::Head, node_id)
    };
    result.map_err(|e| RestoreError::Log(format!("move HEAD to {node_id}: {e}")))
}

/// Append the `restore.performed` event through the F1 writer.
///
/// Recording the restore as an op (rather than mutating the graph in place) is
/// what keeps forward history alive: the restore target and everything forward of
/// it stay in the graph, and the restore is itself an undoable op (DESIGN §10.1).
fn append_restore_event(
    writer: &WriterHandle,
    node_id: Ulid,
    snapshot: &Hash,
    conversation: Option<&Hash>,
) -> Result<(), RestoreError> {
    let payload = json!({
        "node_id": node_id.to_string(),
        "restored_snapshot": snapshot.to_hex(),
        "restored_conversation": conversation.map(Hash::to_hex),
    });
    let event = NewEvent::new(
        EVENT_RESTORE_PERFORMED,
        RESTORE_EVENT_SCHEMA_VERSION,
        payload,
        RESTORE_ACTOR,
    );
    writer
        .append(event)
        .map_err(|e| RestoreError::Log(format!("append restore.performed: {e}")))?;
    Ok(())
}
