//! The isolation seam: [`SandboxTier`], [`Workspace`], and the
//! [`IsolationBackend`] trait.
//!
//! Materializing a node's exact content-addressed snapshot into a live, runnable
//! workspace uses a **tiered isolation ladder** chosen per node by policy rather
//! than forcing one backend everywhere (DESIGN.md §5.3, §11.1). The ladder is
//! frozen here as [`SandboxTier`]; the trait [`IsolationBackend`] is the seam
//! every executor talks to, so executors stay agnostic to which tier they run
//! in. Per the foundation discipline (CLAUDE.md C3) F4 ships exactly one real
//! implementation — [`WorktreeCowBackend`](crate::WorktreeCowBackend) for the
//! [`SandboxTier::WorktreeCow`] tier — and the heavier tiers are additive (P8).
//!
//! The four operations of the seam, straight from DESIGN.md §11.1
//! (`provision` / `exec` / `capturePath` / `teardown`):
//!
//! - [`provision`](IsolationBackend::provision) materializes a snapshot into a
//!   fresh, mutable workspace (and, for the worktree tier, mounts excluded
//!   dependencies read-only from the asset store) under a TTL lease.
//! - [`exec`](IsolationBackend::exec) runs a [`PreparedRun`](crate::PreparedRun)
//!   inside the workspace, honoring a [`CancelToken`](crate::CancelToken).
//! - [`capture_path`](IsolationBackend::capture_path) content-addresses a path
//!   *inside the workspace* back into the object store, returning its
//!   [`Hash`] — the bridge from "live process output" back to "timeline node".
//! - [`teardown`](IsolationBackend::teardown) removes the workspace and releases
//!   its lease; it is reaper-safe (the reaper performs the same removal for a
//!   dead owner).
//!
//! Design references: DESIGN.md §5.3 (execution & isolation), §11.1 (tiered
//! isolation + the `IsolationBackend` interface), §11.3 (lease lifecycle).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use spork_hash::Hash;

use crate::env::EnvManifest;
use crate::error::Result;
use crate::lease::Lease;
use crate::run::{CancelToken, PreparedRun, RawRunOutput};

/// The tiered isolation ladder (DESIGN.md §11.1).
///
/// Isolation strength and cost are inversely related, so the engine offers a
/// ladder selected per node by an isolation policy with the cheapest tier as
/// default. **Only [`WorktreeCow`](SandboxTier::WorktreeCow) has an
/// implementation in F4**; the other tiers are reserved variants that gain
/// implementations additively (P8) behind the same [`IsolationBackend`] trait —
/// the no-domino seam (CLAUDE.md C2/C3). The enum is *not* `#[non_exhaustive]`:
/// the four tiers are the frozen v1 vocabulary, so an exhaustive `match` is
/// forced to consider every tier and a build that meets an unimplemented tier
/// refuses it with [`ExecError::TierUnsupported`](crate::ExecError::TierUnsupported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SandboxTier {
    /// In-process / same-host execution with no extra isolation. Reserved; no
    /// F4 implementation.
    Process,
    /// Git worktree over a copy-on-write filesystem (APFS clonefile / reflink /
    /// ZFS clone), falling back to hardlink/copy. The cheapest tier and the
    /// **only one implemented in F4** ([`WorktreeCowBackend`](crate::WorktreeCowBackend)).
    /// Shares the host kernel, ports, and Docker — right for trusted edits and
    /// unit/validation/sanity checks (the default).
    WorktreeCow,
    /// OCI / devcontainer isolation for reproducible toolchains. Reserved; no F4
    /// implementation (P8).
    Container,
    /// Firecracker / Cloud Hypervisor hardware-virt isolation for untrusted or
    /// stress workloads. Reserved; no F4 implementation (P8).
    MicroVm,
}

impl SandboxTier {
    /// A short, stable label for diagnostics and logging.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            SandboxTier::Process => "process",
            SandboxTier::WorktreeCow => "worktree-cow",
            SandboxTier::Container => "container",
            SandboxTier::MicroVm => "microvm",
        }
    }
}

/// A live, mutable, lease-tracked workspace materialized from a snapshot.
///
/// A `Workspace` is the unit a backend provisions, executes within, and tears
/// down. Its [`root`](Workspace::root) is the on-disk directory the CoW copy
/// lives in; **all filesystem mutation happens against this copy, never the user
/// checkout** (DESIGN.md §5.3, §11.1). It is held under a TTL
/// [`lease`](Workspace::lease) so the crash-safe reaper can reclaim it if the
/// owning process dies (DESIGN.md §11.3). The struct records the
/// [`snapshot`](Workspace::snapshot) and
/// [`env_manifest_hash`](Workspace::env_manifest_hash) it was provisioned from
/// so the live workspace can be tied back to its content-addressed identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The on-disk root of the CoW copy. All execution and capture is confined
    /// here.
    pub root: PathBuf,
    /// The snapshot hash this workspace was materialized from.
    pub snapshot: Hash,
    /// The environment-manifest identity hash this workspace was provisioned for
    /// (two workspaces with the same hash are environment-identical; DESIGN.md
    /// §11.3).
    pub env_manifest_hash: Hash,
    /// The TTL lease that authorizes mutation of this workspace and lets the
    /// reaper reclaim it on owner death (DESIGN.md §11.3).
    pub lease: Lease,
}

impl Workspace {
    /// The lease id authorizing this workspace, as its textual ULID.
    #[must_use]
    pub fn lease_id(&self) -> String {
        self.lease.id.to_string()
    }
}

/// The isolation seam every executor talks to (DESIGN.md §11.1).
///
/// Keeps executors agnostic to which tier they run in: provision a snapshot into
/// a workspace, execute commands inside it, capture results back into the
/// content store, and tear it down. F4 ships exactly one implementation
/// ([`WorktreeCowBackend`](crate::WorktreeCowBackend)); new tiers are new
/// implementations of this trait, never edits to it (CLAUDE.md C3).
pub trait IsolationBackend {
    /// The isolation tier this backend implements.
    fn tier(&self) -> SandboxTier;

    /// Materialize `snapshot` into a fresh, mutable workspace provisioned for
    /// `env`, under a new TTL lease.
    ///
    /// For the worktree tier this reproduces the snapshot tree byte-for-byte via
    /// the content-addressed store (CoW where the filesystem supports it,
    /// hardlink/copy otherwise) and mounts the backend's configured excluded
    /// dependencies **read-only** from the asset store (DESIGN.md §4.10, §10.5).
    /// All mutation thereafter is against the returned workspace's CoW copy,
    /// never the user checkout.
    ///
    /// # Errors
    /// [`ExecError::TierUnsupported`](crate::ExecError::TierUnsupported) on a
    /// backend whose tier has no F4 implementation; plus CAS, asset, and I/O
    /// errors.
    fn provision(&self, snapshot: Hash, env: &EnvManifest) -> Result<Workspace>;

    /// Run `cmd` inside `ws`, honoring `cancel`.
    ///
    /// The command runs with its working directory confined to the workspace
    /// root (or a relative subdirectory of it). A non-zero exit code is a normal
    /// result reported in the returned [`RawRunOutput`], not an error; only the
    /// inability to run, a cancellation, or a lease that is no longer held is an
    /// error.
    ///
    /// # Errors
    /// [`ExecError::LeaseNotHeld`](crate::ExecError::LeaseNotHeld) if the
    /// workspace's lease has lapsed,
    /// [`ExecError::Cancelled`](crate::ExecError::Cancelled) on cancellation,
    /// [`ExecError::Exec`](crate::ExecError::Exec) if the command cannot be run,
    /// and [`ExecError::PathEscape`](crate::ExecError::PathEscape) if `cmd.cwd`
    /// escapes the root.
    fn exec(&self, ws: &Workspace, cmd: PreparedRun, cancel: &CancelToken) -> Result<RawRunOutput>;

    /// Content-address the path `p` (relative to the workspace root, or an
    /// absolute path inside it) back into the object store, returning its hash.
    ///
    /// This is the bridge from live process output back into the timeline: a
    /// result file produced by `exec` is captured here and referenced by content
    /// hash. The path must resolve *inside* the workspace root.
    ///
    /// # Errors
    /// [`ExecError::PathEscape`](crate::ExecError::PathEscape) if `p` resolves
    /// outside the workspace, [`ExecError::LeaseNotHeld`](crate::ExecError::LeaseNotHeld)
    /// if the lease has lapsed, plus CAS and I/O errors.
    fn capture_path(&self, ws: &Workspace, p: &Path) -> Result<Hash>;

    /// Remove the workspace and release its lease.
    ///
    /// Takes the workspace by value so it cannot be used after teardown. This is
    /// reaper-safe: the reaper performs the same removal for a workspace whose
    /// owner died, so a normal teardown and a reaped teardown converge on the
    /// same end state.
    ///
    /// # Errors
    /// I/O errors removing the workspace directory; a workspace already removed
    /// (e.g. by the reaper) is treated as success.
    fn teardown(&self, ws: Workspace) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_labels_are_stable() {
        assert_eq!(SandboxTier::Process.label(), "process");
        assert_eq!(SandboxTier::WorktreeCow.label(), "worktree-cow");
        assert_eq!(SandboxTier::Container.label(), "container");
        assert_eq!(SandboxTier::MicroVm.label(), "microvm");
    }

    #[test]
    fn tier_round_trips_through_serde() {
        for tier in [
            SandboxTier::Process,
            SandboxTier::WorktreeCow,
            SandboxTier::Container,
            SandboxTier::MicroVm,
        ] {
            let json = serde_json::to_string(&tier).unwrap();
            let back: SandboxTier = serde_json::from_str(&json).unwrap();
            assert_eq!(tier, back);
        }
    }
}
