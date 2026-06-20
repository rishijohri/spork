//! The [`Daemon`] core: the single owner of all F3 state and the integration
//! point behind the [`spork_ipc::CommandHandler`] seam.
//!
//! # Why one lock over one core
//!
//! Every privileged subsystem this phase introduces — the F2 [`GraphService`],
//! the content [`ObjectStore`], the [`CapabilityBroker`], the [`FileVault`], the
//! drift pipeline, and the working directory the restore guard materializes into
//! — is inherently single-threaded or interior-mutable, and a command may touch
//! several of them in one transaction (authorize, then mutate the graph, then
//! publish events). The daemon therefore owns them all inside one
//! [`DaemonCore`] behind a single [`Mutex`], so a dispatch is a serialized
//! critical section: the broker's audit trail, the writer actor's append order,
//! and the ordered event stream all advance together with no interleaving. This
//! is the in-process realization of DESIGN.md §5.5 — "all privileged operations
//! live exclusively in the daemon behind a capability-scoped IPC API".
//!
//! The ordered [`EventStream`] and the [`EphemeralBus`] are deliberately kept
//! *outside* the lock (they are internally synchronized and cloneable), so a
//! subscriber can drain events — or a flood of ephemeral frames — without ever
//! contending with a dispatch. That separation is what lets the daemon honor the
//! durable-vs-ephemeral non-interference guarantee (DESIGN.md §14.4).
//!
//! Design references: DESIGN.md §5.5, §14.1, §14.3.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use spork_broker::{Capability, CapabilityBroker, Grant, Scope};
use spork_cas::{LooseStore, ObjectStore};
use spork_drift::{DriftCapture, SecretScanner};
use spork_graph::{GraphProjection, GraphService};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use spork_log::{EventLog, WriterHandle};
use spork_stream::{EphemeralBus, EventStream};
use spork_vault::FileVault;

use crate::error::DaemonError;

/// The on-disk path glob the daemon's default snapshot grants cover.
///
/// `snapshot.read` / `snapshot.write` are path-scoped (DESIGN.md §15.2); the
/// daemon grants itself the whole working tree (`**`) by default so its own
/// snapshot capture/restore are authorized, while a *runner* would receive a
/// narrower grant. Every dispatch still names the concrete path it acts on, so
/// the audit trail records the real scope used, not the grant envelope.
pub const WORKTREE_GLOB: &str = "**";

/// The schema version of the daemon's own configuration record (CLAUDE.md C5).
///
/// The daemon persists no struct of its own beyond the records its subsystems
/// own (each already versioned), so this versions the *wiring contract* — the
/// set of subsystems the daemon composes — which a future generation can grow
/// additively behind the same `CommandHandler` seam.
pub const DAEMON_SCHEMA_VERSION: u16 = 1;

/// All the privileged, single-threaded state a command may touch, owned behind
/// one lock so a dispatch is an atomic critical section.
///
/// `pub(crate)` because the dispatch logic (in [`crate::dispatch`]) and the
/// view-model surface (in [`crate::view`]) read and mutate it through the
/// daemon's lock; nothing outside the crate sees the raw core.
pub(crate) struct DaemonCore {
    /// The F2 validating command layer over the typed graph. The live read
    /// model the view-model surface denormalizes and the mutation entry point
    /// for `node.create` / ref ops / drift capture.
    pub(crate) graph: GraphService,
    /// The content-addressed object store (loose objects + packs) holding every
    /// snapshot tree and conversation blob.
    pub(crate) store: ObjectStore<LooseStore>,
    /// The deny-by-default capability broker every privileged op clears first,
    /// appending an [`spork_broker::AuditEntry`] per call (DESIGN.md §15.1).
    pub(crate) broker: CapabilityBroker,
    /// The credential vault. Secrets are resolved only here, never hashed into
    /// the CAS, never returned to a renderer (DESIGN.md §15.4).
    pub(crate) vault: FileVault,
    /// The drift-capture pipeline (fuse → attribute → secret-scan → store →
    /// auto-drift node) over the working directory (DESIGN.md §10.1, §10.2).
    pub(crate) drift: DriftCapture,
    /// The ignore profile (and matcher) used for snapshot capture; the same
    /// exclusion machinery that defines snapshot identity (DESIGN.md §10.5).
    pub(crate) ignore_profile: IgnoreProfile,
    /// The matcher derived from [`DaemonCore::ignore_profile`].
    pub(crate) matcher: IgnoreMatcher,
    /// The working directory restore materializes into.
    pub(crate) workdir: PathBuf,
    /// The undo/redo op cursor over the mutations this daemon performed.
    ///
    /// The op-log's *operations* are undoable (DESIGN.md §10.2, A.1
    /// `op.undo`/`op.redo`): each mutation the daemon performs pushes its `op_id`
    /// here; `op.undo` moves the cursor back and records an `OP_UNDONE` event,
    /// `op.redo` moves it forward and records an `OP_REDONE` event. Undo/redo do
    /// not delete graph state — restore is the state-moving primitive and forward
    /// history always survives (DESIGN.md §6.4, §10.3); the cursor is the durable
    /// record of *which* op the user is currently at.
    pub(crate) ops: OpCursor,
}

/// An undo/redo cursor over the op_ids of the mutations performed this session.
///
/// `done` holds the ops in performed order; `undone` holds ops that have been
/// undone (a stack, most-recent on top), available to redo. This is the minimal,
/// complete realization of the A.1 `op.undo`/`op.redo` contract: the events are
/// the durable record, and the cursor decides which op a bare
/// `op.undo`/`op.redo` (no explicit id) targets.
#[derive(Debug, Default)]
pub(crate) struct OpCursor {
    /// Ops still in effect, oldest first.
    done: Vec<ulid::Ulid>,
    /// Ops that were undone, most-recently-undone last (the redo stack).
    undone: Vec<ulid::Ulid>,
}

impl OpCursor {
    /// Record a freshly performed op. A new operation invalidates the redo stack
    /// (you cannot redo past a divergent action), matching every editor's
    /// undo/redo model.
    pub(crate) fn push(&mut self, op_id: ulid::Ulid) {
        self.done.push(op_id);
        self.undone.clear();
    }

    /// Undo an op: the given one if `target` is `Some` (it must be currently
    /// done), else the most recent done op. Returns the undone op_id, or `None`
    /// if there is nothing (matching) to undo.
    pub(crate) fn undo(&mut self, target: Option<ulid::Ulid>) -> Option<ulid::Ulid> {
        let idx = match target {
            Some(id) => self.done.iter().rposition(|&d| d == id)?,
            None => self.done.len().checked_sub(1)?,
        };
        let op = self.done.remove(idx);
        self.undone.push(op);
        Some(op)
    }

    /// Redo an op: the given one if `target` is `Some` (it must be currently
    /// undone), else the most recently undone op. Returns the redone op_id, or
    /// `None` if there is nothing (matching) to redo.
    pub(crate) fn redo(&mut self, target: Option<ulid::Ulid>) -> Option<ulid::Ulid> {
        let idx = match target {
            Some(id) => self.undone.iter().rposition(|&d| d == id)?,
            None => self.undone.len().checked_sub(1)?,
        };
        let op = self.undone.remove(idx);
        self.done.push(op);
        Some(op)
    }
}

/// The Spork headless daemon — the single integration point and read surface.
///
/// Construct one with [`Daemon::open`] (or [`Daemon::builder`] for a custom grant
/// set / vault location). It implements [`spork_ipc::CommandHandler`] (see
/// [`crate::dispatch`]) and exposes the frozen read/view-model boundary
/// ([`Daemon::graph_view`], [`Daemon::subscribe_events`]).
///
/// `dispatch` takes `&self`: all mutation lives behind the interior
/// [`Mutex`]/actor, so the daemon is shared across an IPC transport (and across
/// threads) without an exclusive borrow, exactly as the frozen
/// [`CommandHandler`](spork_ipc::CommandHandler) seam requires.
pub struct Daemon {
    /// The serialized critical-section state (see [`DaemonCore`]).
    pub(crate) core: Mutex<DaemonCore>,
    /// The single F1 write path. Cloned to feed a transient restore guard and to
    /// rebuild readers; never a second writer (the actor is the only one).
    pub(crate) writer: WriterHandle,
    /// A handle to the durable event log, for building readers (tailing the log
    /// onto the ordered stream, rebuilding the projection after a guarded op).
    pub(crate) log: Arc<EventLog>,
    /// The directory holding the CAS (`objects/`), so a transient restore guard
    /// can reopen the same store.
    pub(crate) cas_dir: PathBuf,
    /// The next `seq` the ordered [`EventStream`] expects. The rail's `seq` is a
    /// dense, contiguous counter the daemon owns (it does not have to equal the
    /// F1 log's `seq`, since not every log entry is a renderer-facing
    /// transition); guarded by its own lock so event publication is serialized
    /// independently of the core critical section.
    pub(crate) next_seq: Mutex<u64>,
    /// The ordered durable event stream the renderer reduces over. Kept outside
    /// the core lock so draining never contends with a dispatch (DESIGN.md
    /// §14.4).
    pub(crate) events: EventStream,
    /// The node-id-keyed ephemeral side-channels (chat tokens, run stdout). Also
    /// outside the core lock; a flood here never stalls the ordered stream.
    pub(crate) ephemeral: EphemeralBus,
    /// The content-addressed result cache for observing checks (P5).
    ///
    /// Keyed by the F4 [`DerivationKey`](spork_runner::DerivationKey) (input
    /// digest + formula generation), so an identical
    /// `(spec, input_tree, runner_version, change_scope)` re-run is a **cache
    /// hit** that returns the stored envelope without re-executing — including the
    /// auto-run Sanity check that cache-hits on an unchanged subtree (DESIGN §8.1,
    /// §8.2, §9.2). Held outside the core lock (it is internally `Sync`) so a
    /// check's cache I/O never contends with a graph dispatch.
    pub(crate) result_cache: spork_runner::InMemoryResultCache,
}

/// A builder for a [`Daemon`], for callers that need a custom grant set or vault
/// directory.
///
/// The common case is [`Daemon::open`], which grants the daemon the whole
/// working tree for snapshot read/write (so its own capture/restore is
/// authorized) and nothing else — a runner's narrower grants are layered on by a
/// caller that knows the runner's manifest (DESIGN.md §15.2). Use the builder to
/// start from an explicit grant list (e.g. a test that asserts a denial, or a
/// deployment that grants `secrets.get` for a named handle).
pub struct DaemonBuilder {
    root: PathBuf,
    grants: Vec<Grant>,
    ignore_profile: IgnoreProfile,
    scanner: SecretScanner,
}

impl DaemonBuilder {
    /// Start a builder rooted at `root` (which holds the working tree, the CAS,
    /// the event log, and the vault), with the default grant set and the default
    /// secret scanner.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DaemonBuilder {
            root: root.into(),
            grants: default_grants(),
            ignore_profile: IgnoreProfile::default_profile(),
            scanner: SecretScanner::default(),
        }
    }

    /// Replace the broker's grant set wholesale (deny-by-default otherwise).
    #[must_use]
    pub fn with_grants(mut self, grants: Vec<Grant>) -> Self {
        self.grants = grants;
        self
    }

    /// Append one grant to the broker's grant set.
    #[must_use]
    pub fn grant(mut self, grant: Grant) -> Self {
        self.grants.push(grant);
        self
    }

    /// Use a non-default ignore profile for snapshot capture (changing it
    /// changes snapshot identity — DESIGN.md §10.5).
    #[must_use]
    pub fn with_ignore_profile(mut self, profile: IgnoreProfile) -> Self {
        self.ignore_profile = profile;
        self
    }

    /// Use a non-default secret scanner for capture-time redaction.
    #[must_use]
    pub fn with_secret_scanner(mut self, scanner: SecretScanner) -> Self {
        self.scanner = scanner;
        self
    }

    /// Build the daemon, creating the working tree, CAS, event log, and vault
    /// directories under `root` as needed.
    ///
    /// # Errors
    /// Returns [`DaemonError`] if any backing store cannot be opened/created.
    pub fn build(self) -> Result<Daemon, DaemonError> {
        let root = self.root;
        let workdir = root.join("work");
        let cas_dir = root.join("cas");
        let vault_dir = root.join("vault");
        let log_path = root.join("log.db");
        std::fs::create_dir_all(&workdir)
            .map_err(|e| DaemonError::Io(format!("create workdir {workdir:?}: {e}")))?;
        std::fs::create_dir_all(&cas_dir)
            .map_err(|e| DaemonError::Io(format!("create cas dir {cas_dir:?}: {e}")))?;

        let log = Arc::new(EventLog::open(&log_path).map_err(|e| DaemonError::Log(e.to_string()))?);
        let writer = log.writer();

        // Rebuild the live projection from the log (empty for a fresh root, the
        // full history for an existing one) — the graph is a pure projection.
        let graph = build_graph_service(&log, &writer)?;

        let store = ObjectStore::new(
            LooseStore::open(&cas_dir).map_err(|e| DaemonError::Cas(e.to_string()))?,
        );

        let broker = CapabilityBroker::new(self.grants);
        let vault = FileVault::open(&vault_dir).map_err(|e| DaemonError::Vault(e.to_string()))?;

        let matcher = IgnoreMatcher::new(&self.ignore_profile);
        let drift = DriftCapture::with_profile(&workdir, self.ignore_profile.clone())
            .with_scanner(self.scanner);

        let core = DaemonCore {
            graph,
            store,
            broker,
            vault,
            drift,
            ignore_profile: self.ignore_profile,
            matcher,
            workdir,
            ops: OpCursor::default(),
        };

        // The ordered rail is a fresh, dense seq counter the daemon owns; it
        // starts at 1 to match `EventStream::new`, which expects the first
        // published event to carry `seq == 1`. (The rail carries the
        // renderer-facing transitions the daemon publishes this session; the
        // durable F1 log remains the separate source of truth.)
        Ok(Daemon {
            core: Mutex::new(core),
            writer,
            log,
            cas_dir,
            next_seq: Mutex::new(1),
            events: EventStream::new(),
            ephemeral: EphemeralBus::new(),
            result_cache: spork_runner::InMemoryResultCache::new(),
        })
    }
}

impl Daemon {
    /// Open a daemon rooted at `root` with the default grant set.
    ///
    /// `root` holds `work/` (the working tree), `cas/` (the content store),
    /// `log.db` (the event log), and `vault/` (the credential vault); they are
    /// created as needed. The default grants authorize the daemon's own snapshot
    /// read/write over the whole working tree and nothing else (deny-by-default
    /// for everything else — DESIGN.md §15.1).
    ///
    /// # Errors
    /// Returns [`DaemonError`] if a backing store cannot be opened/created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, DaemonError> {
        DaemonBuilder::new(root).build()
    }

    /// Start a [`DaemonBuilder`] rooted at `root`.
    #[must_use]
    pub fn builder(root: impl Into<PathBuf>) -> DaemonBuilder {
        DaemonBuilder::new(root)
    }

    /// The working directory the daemon captures from and restores into.
    #[must_use]
    pub fn workdir(&self) -> PathBuf {
        self.core
            .lock()
            .expect("daemon core mutex poisoned")
            .workdir
            .clone()
    }

    /// The directory holding the CAS objects.
    #[must_use]
    pub fn cas_dir(&self) -> &Path {
        &self.cas_dir
    }
}

/// Build a fresh [`GraphService`] whose in-memory projection is rebuilt from the
/// durable log, with the six P5 built-in node types registered.
///
/// Used at startup and after every guarded (restore/fork) operation, since the
/// guard appends to the shared log out-of-band of the daemon's live service; a
/// rebuild re-derives the projection bit-for-bit from the single source of truth
/// (the log), which is exactly the F2 "graph is a pure projection" property.
///
/// The built-ins (Edit, Validation, Stress, Sanity, Merge, Snapshot) are
/// registered through [`spork_nodes::register_builtins`] — the *public* registry
/// path a P8 plugin uses, with no built-in-only side door (DESIGN §7.1, §9). This
/// supersedes the F2 `register_builtin_snapshot` dogfood call (the `snapshot`
/// kind is now one of the six P5 built-ins), and is purely additive: the F2
/// `snapshot@1.0.0` contract is unchanged, just registered from one place that
/// also brings the other five kinds.
pub(crate) fn build_graph_service(
    log: &EventLog,
    writer: &WriterHandle,
) -> Result<GraphService, DaemonError> {
    let reader = log.reader().map_err(|e| DaemonError::Log(e.to_string()))?;
    let projection = GraphProjection::rebuild_from_log(&reader)
        .map_err(|e| DaemonError::Graph(e.to_string()))?;
    let mut graph = GraphService::new(writer.clone(), projection, default_migrations());
    register_builtins_into(&mut graph)?;
    Ok(graph)
}

/// Register the six P5 built-in node types into a graph service through the
/// public registry path (DESIGN §7.1, §9).
///
/// Factored out so both the live service and any transient (restore-guard)
/// service register the identical built-in set. It iterates
/// [`spork_nodes::builtin_descriptors`] and calls the service's public
/// [`register_descriptor`](GraphService::register_descriptor) for each — exactly
/// the call a third-party plugin makes (no special-casing).
pub(crate) fn register_builtins_into(graph: &mut GraphService) -> Result<(), DaemonError> {
    for descriptor in spork_nodes::builtin_descriptors() {
        graph
            .register_descriptor(descriptor)
            .map_err(|e| DaemonError::Graph(e.to_string()))?;
    }
    Ok(())
}

/// The shared (empty in v1) migration registry every rebuilt service uses.
///
/// No node payload has evolved yet, so the registry is empty; it is the frozen
/// seam through which a payload version is added additively on read (DESIGN.md
/// §7.2), passed by `Arc` because the single-threaded service shares it within
/// its own thread.
#[allow(clippy::arc_with_non_send_sync)]
fn default_migrations() -> Arc<spork_migrate::MigrationRegistry> {
    Arc::new(spork_migrate::MigrationRegistry::new())
}

/// The daemon's default grant set: snapshot read + write over the whole working
/// tree, plus `process.spawn` so the built-in observing checks can run, and
/// nothing else.
///
/// This authorizes the daemon's own capture/restore and the P5 observing checks
/// (the trusted core runs the six built-ins out of the box, DESIGN.md §7.1,
/// §8.2), while every other capability — `net.connect`, `model.invoke`,
/// `secrets.get` — stays denied by default until a caller grants it for a
/// specific runner/scope (DESIGN.md §15.1, §15.2). A non-hermetic check
/// (Validation/Stress) authorizes `process.spawn`; a hermetic one (Sanity)
/// authorizes only `snapshot.read` (see [`crate::nodes`]).
fn default_grants() -> Vec<Grant> {
    let worktree = Scope::new().with_path_globs([WORKTREE_GLOB]);
    vec![
        Grant::new(Capability::SnapshotRead, worktree.clone()),
        Grant::new(Capability::SnapshotWrite, worktree.clone()),
        Grant::new(Capability::ProcessSpawn, worktree),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    #[test]
    fn op_cursor_undo_redo_targets_most_recent() {
        let mut c = OpCursor::default();
        let a = Ulid::new();
        let b = Ulid::new();
        c.push(a);
        c.push(b);
        // Bare undo pops the most recent (b), then a.
        assert_eq!(c.undo(None), Some(b));
        assert_eq!(c.undo(None), Some(a));
        assert_eq!(c.undo(None), None, "nothing left to undo");
        // Redo replays in reverse-undo order: a, then b.
        assert_eq!(c.redo(None), Some(a));
        assert_eq!(c.redo(None), Some(b));
        assert_eq!(c.redo(None), None, "nothing left to redo");
    }

    #[test]
    fn op_cursor_undo_redo_can_target_a_specific_op() {
        let mut c = OpCursor::default();
        let a = Ulid::new();
        let b = Ulid::new();
        c.push(a);
        c.push(b);
        // Undo the older op explicitly.
        assert_eq!(c.undo(Some(a)), Some(a));
        // b is still done; undoing an already-undone op fails.
        assert_eq!(c.undo(Some(a)), None);
        // Redo a explicitly.
        assert_eq!(c.redo(Some(a)), Some(a));
        assert_eq!(c.redo(Some(a)), None, "a is already done again");
    }

    #[test]
    fn a_new_op_clears_the_redo_stack() {
        let mut c = OpCursor::default();
        let a = Ulid::new();
        let b = Ulid::new();
        c.push(a);
        c.undo(None);
        // A divergent new op invalidates the redo of a.
        c.push(b);
        assert_eq!(c.redo(None), None, "redo stack cleared by a new op");
    }

    #[test]
    fn open_creates_the_backing_dirs_and_default_grants_deny_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(dir.path()).unwrap();
        assert!(daemon.workdir().exists());
        assert!(daemon.cas_dir().exists());
        // Default grants cover snapshot read+write and process.spawn (so the
        // built-in observing checks run out of the box, DESIGN §8.2) and nothing
        // else — net/model/secrets stay denied by default.
        let core = daemon.core.lock().unwrap();
        let caps: Vec<_> = core.broker.grants().iter().map(|g| g.capability).collect();
        assert!(caps.contains(&Capability::SnapshotRead));
        assert!(caps.contains(&Capability::SnapshotWrite));
        assert!(caps.contains(&Capability::ProcessSpawn));
        assert!(!caps.contains(&Capability::NetConnect));
        assert!(!caps.contains(&Capability::SecretsGet));
    }
}
