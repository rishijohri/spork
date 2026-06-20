//! The crate error type: [`ProjError`].
//!
//! Every fallible operation in this crate funnels into one `#[non_exhaustive]`
//! enum so callers can match the failure modes that matter (a checkpoint whose
//! recorded hash disagrees with its bytes) while staying forward-compatible as
//! new variants are added behind the seam (CLAUDE.md C3). Lower-layer errors are
//! flattened to owned `String`s so this enum is `Send + Sync + 'static` and easy
//! to surface across threads, matching `spork-log`'s [`LogError`] convention.
//!
//! [`LogError`]: spork_log::LogError

use thiserror::Error;

/// An error from the projection layer.
///
/// The variants split into *plumbing* failures that wrap a lower layer
/// ([`ProjError::Log`] for the event log, [`ProjError::Canon`] for canonical
/// serialization) and an *integrity* failure ([`ProjError::CheckpointMismatch`])
/// that signals a checkpoint whose bytes no longer match its recorded hash —
/// i.e. corruption or tamper of persisted projection state.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjError {
    /// Reading the event log failed (open, query, decode, hash-chain, …).
    ///
    /// Carries the flattened message from [`spork_log::LogError`]. A projection
    /// is a pure function of the log, so any failure to read the log surfaces
    /// here rather than being papered over with a partial result.
    #[error("event log error: {0}")]
    Log(String),

    /// Canonical serialization of a snapshot failed (see `spork-canon`).
    ///
    /// A snapshot must be canonically serializable for its hash to be stable
    /// across machines and runs; if it carries a value that canonicalization
    /// rejects (for example a float — forbidden in identity-bearing data), the
    /// checkpoint cannot be formed and this error is returned.
    #[error("canonical serialization error: {0}")]
    Canon(String),

    /// A checkpoint's recorded `snapshot_hash` does not match its
    /// `snapshot_bytes`.
    ///
    /// Returned by [`crate::ProjectionStore::load_or_rebuild`] (and
    /// [`crate::Checkpoint::verify`]) when a checkpoint is loaded whose bytes
    /// were altered after the hash was computed. Because a projection is always
    /// rebuildable from the log, the correct recovery is to discard the
    /// checkpoint and rebuild — but the mismatch is surfaced rather than
    /// silently trusted.
    #[error("checkpoint hash does not match its bytes")]
    CheckpointMismatch,
}

impl From<spork_log::LogError> for ProjError {
    fn from(e: spork_log::LogError) -> Self {
        ProjError::Log(e.to_string())
    }
}

impl From<spork_canon::CanonError> for ProjError {
    fn from(e: spork_canon::CanonError) -> Self {
        ProjError::Canon(e.to_string())
    }
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, ProjError>;
