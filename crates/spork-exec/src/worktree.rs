//! The v1 isolation backend: [`WorktreeCowBackend`] over [`spork_cas`] and
//! [`spork_asset`].
//!
//! This is the single F4 implementation of [`IsolationBackend`](crate::IsolationBackend)
//! — the default, cheapest tier ([`SandboxTier::WorktreeCow`](crate::SandboxTier::WorktreeCow)).
//! It realizes the worktree-on-CoW model of DESIGN.md §5.3, §11.1: materialize a
//! node's exact content-addressed snapshot into a fresh, mutable worktree, mount
//! the excluded dependencies **read-only** from the asset store (DESIGN.md
//! §4.10, §10.5), run commands against that copy, and tear it down — **all
//! mutation against the CoW copy, never the user checkout.**
//!
//! ## What `provision` does
//!
//! 1. Materializes the snapshot tree byte-for-byte into a fresh worktree
//!    directory via [`ObjectStore::materialize_snapshot`](spork_cas::ObjectStore::materialize_snapshot).
//!    The CoW story (reflink/clonefile with a hardlink/copy fallback) lives
//!    inside `spork-cas`/`spork-asset`; this backend composes it rather than
//!    re-implementing it.
//! 2. Mounts every configured excluded dependency **read-only** at its declared
//!    sub-path via [`AssetStore::materialize`](spork_asset::AssetStore::materialize).
//!    The asset store forces the materialized result read-only so the agent
//!    never wastes a turn editing a vendored dependency (DESIGN.md §10.5).
//! 3. Acquires a durable TTL [`Lease`](crate::Lease) in the
//!    [`LeaseLedger`](crate::LeaseLedger) so the crash-safe reaper can reclaim
//!    the worktree if this process dies (DESIGN.md §11.3).
//!
//! ## What `exec` / `capture_path` / `teardown` do
//!
//! `exec` runs a [`PreparedRun`](crate::PreparedRun) with its working directory
//! confined to the worktree (a `..`-escaping or absolute `cwd` is refused),
//! honoring the [`CancelToken`](crate::CancelToken). `capture_path`
//! content-addresses a path *inside* the worktree back into the object store.
//! `teardown` removes the worktree and releases the lease, converging on the
//! same end state the reaper would reach for a dead owner.
//!
//! Design references: DESIGN.md §5.3 (worktree-on-CoW default), §11.1 (the
//! `IsolationBackend` interface + CoW/fallback), §11.3 (lease lifecycle), §4.10
//! / §10.5 (deps reconstructed read-only from the asset store).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use spork_asset::{AssetKey, AssetStore};
use spork_cas::{ObjectStore, StorageBackend};
use spork_hash::Hash;

use crate::backend::{IsolationBackend, SandboxTier, Workspace};
use crate::env::EnvManifest;
use crate::error::{ExecError, Result};
use crate::lease::{remove_workspace, Clock, LeaseLedger, SystemClock};
use crate::run::{CancelToken, PreparedRun, RawRunOutput};

/// One excluded dependency to mount read-only into a provisioned worktree.
///
/// Pairs the [`AssetKey`](spork_asset::AssetKey) naming the asset with the
/// sub-path (relative to the worktree root) it should be materialized at — for
/// example, an opaque model-weights blob mounted at `models/weights.bin`, or a
/// reconstructable `node_modules` tree at `node_modules`. The asset store
/// materializes it read-only (DESIGN.md §10.5).
#[derive(Debug, Clone)]
pub struct AssetMount {
    /// The key naming the asset in the asset store.
    pub key: AssetKey,
    /// Where to mount it, relative to the worktree root.
    pub mount_at: PathBuf,
}

impl AssetMount {
    /// Construct a mount of `key` at `mount_at` (relative to the worktree root).
    #[must_use]
    pub fn new(key: AssetKey, mount_at: impl Into<PathBuf>) -> Self {
        AssetMount {
            key,
            mount_at: mount_at.into(),
        }
    }
}

/// The worktree-on-CoW isolation backend — the one F4 implementation.
///
/// Composes a content-addressed [`ObjectStore`](spork_cas::ObjectStore) (for
/// snapshot materialization and path capture), an
/// [`AssetStore`](spork_asset::AssetStore) (for read-only dependency mounts),
/// and a durable [`LeaseLedger`](crate::LeaseLedger) (for crash-safe teardown).
/// Worktrees are created under a configured `worktree_dir`. The asset mounts a
/// provision should apply are configured per backend via
/// [`with_asset_mount`](WorktreeCowBackend::with_asset_mount).
///
/// The ledger and worktree counter are behind a [`Mutex`] so the backend is
/// `Sync` and usable behind `&self` (the [`IsolationBackend`] methods take
/// `&self`).
#[derive(Debug)]
pub struct WorktreeCowBackend<S: StorageBackend + Sync, A: AssetStore, C: Clock = SystemClock> {
    objects: ObjectStore<S>,
    assets: A,
    worktree_dir: PathBuf,
    ledger: Mutex<LeaseLedger>,
    clock: C,
    owner_pid: u32,
    ttl_ms: u64,
    heartbeat_ms: u64,
    mounts: Vec<AssetMount>,
    counter: Mutex<u64>,
}

impl<S: StorageBackend + Sync, A: AssetStore> WorktreeCowBackend<S, A, SystemClock> {
    /// Construct a backend with the system clock and this process's pid as the
    /// lease owner.
    ///
    /// `worktree_dir` is the directory under which fresh worktrees are
    /// materialized; `ledger` is the durable lease ledger this backend records
    /// into. Default lease parameters are a 60s TTL with a 5s heartbeat; tune
    /// them with [`with_lease_params`](WorktreeCowBackend::with_lease_params).
    #[must_use]
    pub fn new(
        objects: ObjectStore<S>,
        assets: A,
        worktree_dir: impl Into<PathBuf>,
        ledger: LeaseLedger,
    ) -> Self {
        WorktreeCowBackend {
            objects,
            assets,
            worktree_dir: worktree_dir.into(),
            ledger: Mutex::new(ledger),
            clock: SystemClock,
            owner_pid: std::process::id(),
            ttl_ms: 60_000,
            heartbeat_ms: 5_000,
            mounts: Vec::new(),
            counter: Mutex::new(0),
        }
    }
}

impl<S: StorageBackend + Sync, A: AssetStore, C: Clock> WorktreeCowBackend<S, A, C> {
    /// Construct a backend with an explicit clock (used in tests to drive lease
    /// TTLs deterministically).
    #[must_use]
    pub fn with_clock(
        objects: ObjectStore<S>,
        assets: A,
        worktree_dir: impl Into<PathBuf>,
        ledger: LeaseLedger,
        clock: C,
    ) -> Self {
        WorktreeCowBackend {
            objects,
            assets,
            worktree_dir: worktree_dir.into(),
            ledger: Mutex::new(ledger),
            clock,
            owner_pid: std::process::id(),
            ttl_ms: 60_000,
            heartbeat_ms: 5_000,
            mounts: Vec::new(),
            counter: Mutex::new(0),
        }
    }

    /// Set the owner pid recorded on acquired leases (builder style).
    ///
    /// Defaults to this process's pid. Overriding it is how a test simulates a
    /// lease whose owner is a different (or dead) process.
    #[must_use]
    pub fn with_owner_pid(mut self, pid: u32) -> Self {
        self.owner_pid = pid;
        self
    }

    /// Set the lease TTL and heartbeat intervals in milliseconds (builder
    /// style).
    #[must_use]
    pub fn with_lease_params(mut self, ttl_ms: u64, heartbeat_ms: u64) -> Self {
        self.ttl_ms = ttl_ms;
        self.heartbeat_ms = heartbeat_ms;
        self
    }

    /// Register an excluded dependency to mount read-only on every provision
    /// (builder style).
    #[must_use]
    pub fn with_asset_mount(mut self, mount: AssetMount) -> Self {
        self.mounts.push(mount);
        self
    }

    /// Borrow the underlying object store (e.g. to read back a captured blob).
    #[must_use]
    pub fn objects(&self) -> &ObjectStore<S> {
        &self.objects
    }

    /// Borrow the underlying asset store.
    #[must_use]
    pub fn assets(&self) -> &A {
        &self.assets
    }

    /// Run a closure with the locked durable ledger (e.g. to heartbeat a lease).
    ///
    /// This is the supported way for an owner to renew its lease mid-run so the
    /// reaper does not reclaim a long-running workspace (DESIGN.md §11.3).
    pub fn with_ledger<R>(&self, f: impl FnOnce(&mut LeaseLedger) -> R) -> R {
        let mut guard = self.ledger.lock().expect("lease ledger mutex poisoned");
        f(&mut guard)
    }

    /// Allocate the next unique worktree directory name under `worktree_dir`.
    fn next_worktree_root(&self) -> PathBuf {
        let mut counter = self
            .counter
            .lock()
            .expect("worktree counter mutex poisoned");
        let n = *counter;
        *counter += 1;
        self.worktree_dir.join(format!("ws-{n:08x}"))
    }

    /// Resolve a caller-supplied path against the workspace root, refusing any
    /// path that escapes the root.
    fn resolve_in_workspace(root: &Path, p: &Path) -> Result<PathBuf> {
        let candidate = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        };
        // Reject `..` components outright: a lexical check is sufficient and does
        // not require the path to exist (so it works before capture, too).
        if candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ExecError::PathEscape {
                path: p.to_path_buf(),
                root: root.to_path_buf(),
            });
        }
        if !candidate.starts_with(root) {
            return Err(ExecError::PathEscape {
                path: p.to_path_buf(),
                root: root.to_path_buf(),
            });
        }
        Ok(candidate)
    }

    /// Assert the workspace's lease is currently held; error otherwise.
    fn assert_lease_held(&self, ws: &Workspace) -> Result<()> {
        let guard = self.ledger.lock().expect("lease ledger mutex poisoned");
        if guard.is_held(&ws.lease.id, &self.clock) {
            Ok(())
        } else {
            Err(ExecError::LeaseNotHeld {
                lease: ws.lease.id.to_string(),
                root: ws.root.clone(),
            })
        }
    }
}

impl<S: StorageBackend + Sync, A: AssetStore, C: Clock> IsolationBackend
    for WorktreeCowBackend<S, A, C>
{
    fn tier(&self) -> SandboxTier {
        SandboxTier::WorktreeCow
    }

    fn provision(&self, snapshot: Hash, env: &EnvManifest) -> Result<Workspace> {
        let env_manifest_hash = env.env_manifest_hash()?;
        let root = self.next_worktree_root();

        // 1. Materialize the snapshot tree byte-for-byte into the fresh worktree
        //    (CoW where the filesystem supports it; this is the user-checkout-
        //    never-touched copy).
        self.objects.materialize_snapshot(&snapshot, &root)?;

        // 2. Mount every configured excluded dependency read-only from the asset
        //    store, at its declared sub-path (refusing any escape).
        for mount in &self.mounts {
            let dest = Self::resolve_in_workspace(&root, &mount.mount_at)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ExecError::io(parent, e))?;
            }
            self.assets.materialize(&mount.key, &dest)?;
        }

        // 3. Acquire a durable lease so the reaper can reclaim this worktree on a
        //    crash.
        let lease = {
            let mut guard = self.ledger.lock().expect("lease ledger mutex poisoned");
            guard.acquire(
                &root,
                self.owner_pid,
                self.ttl_ms,
                self.heartbeat_ms,
                &self.clock,
            )?
        };

        Ok(Workspace {
            root,
            snapshot,
            env_manifest_hash,
            lease,
        })
    }

    fn exec(&self, ws: &Workspace, cmd: PreparedRun, cancel: &CancelToken) -> Result<RawRunOutput> {
        self.assert_lease_held(ws)?;
        if cancel.is_cancelled() {
            return Err(ExecError::Cancelled);
        }

        let cwd = match &cmd.cwd {
            Some(rel) => Self::resolve_in_workspace(&ws.root, rel)?,
            None => ws.root.clone(),
        };

        let mut command = std::process::Command::new(&cmd.program);
        command.args(&cmd.args).current_dir(&cwd);
        for (k, v) in &cmd.env {
            command.env(k, v);
        }

        // Re-check cancellation just before spawning so a cancel that arrives
        // during setup is honored.
        if cancel.is_cancelled() {
            return Err(ExecError::Cancelled);
        }

        let output = command.output().map_err(|e| ExecError::Exec {
            command: cmd.program.clone(),
            reason: e.to_string(),
        })?;

        if cancel.is_cancelled() {
            return Err(ExecError::Cancelled);
        }

        Ok(RawRunOutput::new(
            output.status.code(),
            output.stdout,
            output.stderr,
        ))
    }

    fn capture_path(&self, ws: &Workspace, p: &Path) -> Result<Hash> {
        self.assert_lease_held(ws)?;
        let abs = Self::resolve_in_workspace(&ws.root, p)?;
        let meta = std::fs::symlink_metadata(&abs).map_err(|e| ExecError::io(&abs, e))?;
        if meta.is_dir() {
            // Capture a subtree as a content-addressed tree object under the
            // F0-frozen deps-excluded policy, so a captured result subtree never
            // drags `node_modules`/`target` into the content store (DESIGN.md
            // §10.5).
            let matcher =
                spork_ignore::IgnoreMatcher::new(&spork_ignore::IgnoreProfile::default_profile());
            let (hash, _stats) = self.objects.put_tree(&abs, &matcher)?;
            Ok(hash)
        } else {
            // Capture a regular file as a blob.
            let bytes = std::fs::read(&abs).map_err(|e| ExecError::io(&abs, e))?;
            let (hash, _stats) = self.objects.put_blob_bytes(&bytes)?;
            Ok(hash)
        }
    }

    fn teardown(&self, ws: Workspace) -> Result<()> {
        remove_workspace(&ws.root)?;
        let mut guard = self.ledger.lock().expect("lease ledger mutex poisoned");
        guard.release(&ws.lease.id)?;
        Ok(())
    }
}
