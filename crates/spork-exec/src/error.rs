//! The crate's error type, [`ExecError`].
//!
//! Every fallible operation in the execution seam — provisioning a workspace,
//! running a command, capturing a path back into the content store, tearing a
//! workspace down, and the durable lease/reaper plumbing — funnels its failures
//! into [`ExecError`]. The variants are deliberately discriminable so callers
//! (and the reaper) can react precisely: a tier that has no F4 implementation is
//! a distinct, typed refusal ([`ExecError::TierUnsupported`]); a lease whose
//! owning process has died is a distinct, typed condition the reaper keys off
//! ([`ExecError::LeaseExpired`]); a workspace whose lease no longer authorizes a
//! mutation is refused ([`ExecError::LeaseNotHeld`]).
//!
//! The enum is `#[non_exhaustive]` so later isolation tiers (Container, MicroVm
//! in P8) and a richer scheduler (P7) can add variants without a breaking change
//! to downstream `match`es — the no-domino discipline (CLAUDE.md C2).
//!
//! Design references: DESIGN.md §5.3 (execution & isolation), §11.1-§11.3
//! (tiered isolation, resource-aware scheduling, lease-based teardown), §4.10
//! (the asset store seam used by `provision`).

use std::path::PathBuf;

use thiserror::Error;

use crate::SandboxTier;

/// Errors produced by the execution seam.
///
/// Spans the four surfaces of the seam: tier capability
/// ([`TierUnsupported`](ExecError::TierUnsupported)), workspace/lease lifecycle
/// ([`LeaseNotHeld`](ExecError::LeaseNotHeld),
/// [`LeaseExpired`](ExecError::LeaseExpired),
/// [`UnknownWorkspace`](ExecError::UnknownWorkspace)), command execution
/// ([`Exec`](ExecError::Exec), [`Cancelled`](ExecError::Cancelled),
/// [`PathEscape`](ExecError::PathEscape)), and the I/O / CAS / asset / ledger
/// plumbing beneath them.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExecError {
    /// The requested [`SandboxTier`] has no implementation in this build.
    ///
    /// F4 ships exactly one tier ([`SandboxTier::WorktreeCow`]); the
    /// `Process`, `Container`, and `MicroVm` tiers are added additively in later
    /// phases (P8). This is a *complete*, deterministic refusal naming the tier —
    /// not a stub.
    #[error(
        "isolation tier {0:?} is not implemented in this build (only WorktreeCow ships in F4)"
    )]
    TierUnsupported(SandboxTier),

    /// A workspace mutation (exec / capture / teardown) was attempted while the
    /// workspace's lease was not held — it had expired, been reclaimed by the
    /// reaper, or never existed in the ledger.
    ///
    /// Every workspace is held under a TTL [`Lease`](crate::Lease); a mutation
    /// must present a live lease so the reaper can never race a live caller
    /// (DESIGN.md §11.3).
    #[error("lease {lease} for workspace {root:?} is not held (expired or reclaimed)")]
    LeaseNotHeld {
        /// The lease id that was expected to authorize the operation.
        lease: String,
        /// The workspace root the operation targeted.
        root: PathBuf,
    },

    /// A lease was found in the ledger but is no longer valid — its TTL elapsed
    /// without a heartbeat, or its owning process is dead.
    ///
    /// The reaper raises (and acts on) this condition; it is surfaced as an
    /// error when a caller tries to renew or assert a lease that has already
    /// lapsed (DESIGN.md §11.3, the crash-safe reaper).
    #[error("lease {0} has expired (ttl elapsed or owner process is dead)")]
    LeaseExpired(String),

    /// A lease id referenced an entry not present in the durable ledger.
    #[error("no lease with id {0} is recorded in the ledger")]
    UnknownLease(String),

    /// A workspace root was referenced that the backend never provisioned (or
    /// that has already been torn down).
    #[error("workspace {0:?} is not known to this backend")]
    UnknownWorkspace(PathBuf),

    /// A captured/relative path escaped the workspace root (e.g. via `..` or an
    /// absolute path), which would let a capture read outside the CoW copy.
    ///
    /// All filesystem access is confined to the CoW workspace, never the user
    /// checkout (DESIGN.md §5.3, §11.1); a path that resolves outside the root is
    /// refused rather than followed.
    #[error("path {path:?} escapes workspace root {root:?}")]
    PathEscape {
        /// The offending path.
        path: PathBuf,
        /// The workspace root it escaped.
        root: PathBuf,
    },

    /// A prepared command failed to spawn or execute.
    ///
    /// The command itself exiting non-zero is *not* an error — that is reported
    /// in [`RawRunOutput::exit_code`](crate::RawRunOutput::exit_code). This
    /// variant covers the inability to *run* the command at all (missing binary,
    /// permission denied spawning, etc.).
    #[error("failed to execute command {command:?}: {reason}")]
    Exec {
        /// The program that could not be executed.
        command: String,
        /// Why execution could not proceed.
        reason: String,
    },

    /// Execution was cancelled via the [`CancelToken`](crate::CancelToken)
    /// before the command completed.
    #[error("execution was cancelled before completion")]
    Cancelled,

    /// The canonical encoding of an [`EnvManifest`](crate::EnvManifest) (or
    /// another hashed record) failed.
    ///
    /// Carries the [`spork_canon::CanonError`] message so a non-byte-stable
    /// input (e.g. a float that slipped into a record) is not lost in
    /// translation.
    #[error("canonicalization failed: {0}")]
    Canon(String),

    /// A durable lease-ledger record could not be decoded.
    ///
    /// The ledger keeps a self-describing record per lease; this covers a record
    /// whose bytes do not parse (a schema version this build does not
    /// understand, or on-disk corruption).
    #[error("failed to decode lease ledger record: {0}")]
    Decode(String),

    /// An underlying I/O operation failed, tagged with the path it targeted.
    #[error("i/o error at {path:?}: {source}")]
    Io {
        /// The filesystem path the operation was targeting, if applicable.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// An error surfaced from the content-addressed store
    /// ([`spork_cas`]) while materializing a snapshot or capturing a path.
    #[error("content-addressed store error: {0}")]
    Cas(String),

    /// An error surfaced from the asset store ([`spork_asset`]) while
    /// materializing an excluded, read-only dependency.
    #[error("asset store error: {0}")]
    Asset(String),
}

impl ExecError {
    /// Build an [`ExecError::Io`] tagged with the path it occurred at.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ExecError::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<spork_cas::CasError> for ExecError {
    fn from(e: spork_cas::CasError) -> Self {
        ExecError::Cas(e.to_string())
    }
}

impl From<spork_asset::AssetError> for ExecError {
    fn from(e: spork_asset::AssetError) -> Self {
        ExecError::Asset(e.to_string())
    }
}

impl From<spork_canon::CanonError> for ExecError {
    fn from(e: spork_canon::CanonError) -> Self {
        ExecError::Canon(e.to_string())
    }
}

/// Convenience result alias for the crate's fallible operations.
pub type Result<T> = std::result::Result<T, ExecError>;
