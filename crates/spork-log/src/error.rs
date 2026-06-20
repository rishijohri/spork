//! The crate error type: [`LogError`].
//!
//! Every fallible operation in this crate funnels into one `#[non_exhaustive]`
//! enum so callers can match the failure modes that matter (a broken hash chain,
//! a non-contiguous sequence) while remaining forward-compatible as new variants
//! are added behind the seam (CLAUDE.md C3).

use thiserror::Error;

/// An error from the event log.
///
/// The variants split into *integrity* failures that signal tamper or
/// corruption ([`LogError::HashChainBroken`], [`LogError::NotContiguous`]) and
/// *plumbing* failures wrapping a lower layer ([`LogError::Sqlite`],
/// [`LogError::Canon`], [`LogError::Migration`]). Lower-layer errors are flattened
/// to owned `String`s rather than wrapping foreign error types so this enum stays
/// `Send + Sync + 'static` and crosses the writer-actor channel cleanly.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LogError {
    /// A SQLite operation (open, prepare, step, commit, …) failed.
    ///
    /// Carries the flattened message from `rusqlite`. This also covers the
    /// writer actor being unreachable (its thread has gone away), which is
    /// reported here rather than as a panic so a caller can surface it.
    #[error("sqlite error: {0}")]
    Sqlite(String),

    /// The hash chain failed verification at sequence `seq`.
    ///
    /// Returned by [`crate::Reader::verify_chain`] when a stored event's
    /// recomputed `this_event_hash` does not match the stored value, or when an
    /// event's `prev_event_hash` does not equal the previous event's
    /// `this_event_hash`. The `seq` is the first event at which the break is
    /// detected — the tamper point.
    #[error("hash chain broken at seq {seq}")]
    HashChainBroken {
        /// The sequence number at which the chain first fails to verify.
        seq: u64,
    },

    /// Canonical serialization of a payload failed (e.g. it contained a float,
    /// which is forbidden in identity-bearing data — see `spork-canon`).
    #[error("canonical serialization error: {0}")]
    Canon(String),

    /// A migration applied on read failed (see `spork-migrate`).
    #[error("migration error: {0}")]
    Migration(String),

    /// Stored sequence numbers are not contiguous from 1.
    ///
    /// The log invariant is that `seq` runs `1, 2, 3, …` with no gaps. A gap is
    /// either corruption or a bug in the writer; it is surfaced rather than
    /// silently skipped because every downstream projection assumes contiguity.
    #[error("non-contiguous sequence: expected {expected}, got {got}")]
    NotContiguous {
        /// The sequence number that was expected next.
        expected: u64,
        /// The sequence number actually found.
        got: u64,
    },
}

impl From<rusqlite::Error> for LogError {
    fn from(e: rusqlite::Error) -> Self {
        LogError::Sqlite(e.to_string())
    }
}

impl From<spork_canon::CanonError> for LogError {
    fn from(e: spork_canon::CanonError) -> Self {
        LogError::Canon(e.to_string())
    }
}

impl From<spork_migrate::MigrationError> for LogError {
    fn from(e: spork_migrate::MigrationError) -> Self {
        LogError::Migration(e.to_string())
    }
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, LogError>;
