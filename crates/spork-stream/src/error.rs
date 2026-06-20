//! The crate error vocabulary, [`StreamError`].
//!
//! The two rails fail for different reasons, and the type makes that explicit:
//! the ordered [`EventStream`](crate::EventStream) can detect a **gap** in the
//! durable `seq` sequence (the one invariant it must never silently swallow,
//! DESIGN.md §5.5, §14.4), while tailing the F1 log can surface a
//! [`spork_log::LogError`]. The ephemeral rail, by contract, never errors on a
//! flood — frames may be coalesced or dropped under backpressure — so it has no
//! variant here.
//!
//! Design references: DESIGN.md §5.5, §14.4.

use thiserror::Error;

/// Errors raised by the dual-delivery transport.
///
/// Marked `#[non_exhaustive]` so future transports (e.g. a network-spanning
/// ordered channel) can add variants without breaking matchers (CLAUDE.md C2).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StreamError {
    /// The ordered durable stream observed a hole in the `seq` sequence: the
    /// next event's `seq` was not exactly one greater than the last delivered
    /// one. This is the gapless-delivery invariant of the durable rail
    /// (DESIGN.md §14.4); the transport surfaces it rather than papering over it.
    #[error("ordered event-stream gap: expected seq {expected}, got {got}")]
    SeqGap {
        /// The `seq` the stream required next (last delivered + 1).
        expected: u64,
        /// The `seq` actually presented by the source.
        got: u64,
    },

    /// Tailing the underlying F1 event log failed.
    #[error("event log read failed while tailing: {0}")]
    Log(String),
}

impl From<spork_log::LogError> for StreamError {
    fn from(e: spork_log::LogError) -> Self {
        StreamError::Log(e.to_string())
    }
}
