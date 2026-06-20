//! Spork F4 execution seam — isolation, leases, and the hashed environment.
//!
//! This crate freezes the execution-substrate seam for Spork: how a node's
//! content-addressed snapshot becomes a live, mutable workspace that runs
//! commands, and how the resources that execution needs are admitted and
//! reclaimed. Per the foundation discipline (CLAUDE.md C3) the trait is the
//! seam and F4 ships exactly one real implementation behind it; later tiers are
//! additive.
//!
//! The frozen surface (filled in as the F4 implementation lands):
//!
//! - `IsolationBackend` — the execution port, with the `SandboxTier` ladder
//!   (`Process` / `WorktreeCow` / `Container` / `MicroVm`). F4 implements only
//!   the `WorktreeCow` tier (`WorktreeCowBackend`): it materializes a snapshot
//!   tree into a fresh worktree via the copy-on-write `spork-cas` backend
//!   (reflink/clonefile with a hardlink/copy fallback) and mounts excluded
//!   dependencies read-only from the `spork-asset` `AssetStore`, then runs and
//!   tears down against the CoW copy — never the user checkout. Container and
//!   microVM tiers are deferred to P8.
//! - `EnvManifest` — a canonical-serialized, hashed environment descriptor
//!   (`env_manifest_hash`); two nodes with the same hash are environment-
//!   identical. It carries its own `schema_version` (CLAUDE.md C5).
//! - `ResourceProfile` / `Lease` — the admission inputs and the durable lease
//!   record (id, owner pid, ttl, heartbeat, durability).
//! - `Scheduler` — the admission seam. F4 ships a serial `SerialScheduler`;
//!   the full constraint solver is P7.
//! - A durable lease ledger plus a reaper that reclaims a workspace whose lease
//!   expired or whose owner process is dead, surviving a restart.
//!
//! This realizes the execution model in DESIGN.md §5.3 ("Execution &
//! isolation"), the sandbox / resource / scheduling seams in §11.1-§11.3, and
//! the deps-excluded, asset-store-backed dependency materialization in §4.10.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod backend;
mod env;
mod error;
mod lease;
mod resource;
mod run;
mod scheduler;
mod worktree;

pub use backend::{IsolationBackend, SandboxTier, Workspace};
pub use env::{EnvManifest, ENV_MANIFEST_VERSION};
pub use error::{ExecError, Result};
pub use lease::{
    Clock, Lease, LeaseLedger, ProcessChecker, Reaper, ReclaimReason, Reclaimed, SystemClock,
    SystemProcessChecker, LEASE_VERSION, LEDGER_VERSION,
};
pub use resource::{Budget, PooledNeed, ResourceProfile, RESOURCE_PROFILE_VERSION};
pub use run::{
    CancelToken, PreparedRun, RawRunOutput, PREPARED_RUN_VERSION, RAW_RUN_OUTPUT_VERSION,
};
pub use scheduler::{
    Admission, AdmissionPlan, Disposition, ScheduledItem, Scheduler, SerialScheduler,
};
pub use worktree::{AssetMount, WorktreeCowBackend};
