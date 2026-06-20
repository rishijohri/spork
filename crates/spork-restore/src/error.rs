//! The restore error taxonomy: [`RestoreError`].
//!
//! Every failure mode of the atomic dual-restore guard is a variant here. The
//! enum is `#[non_exhaustive]` so future restore-relevant failures (e.g. a
//! materialization fault once the worktree materializer grows richer) can be
//! added additively without a flag day (CLAUDE.md C2).
//!
//! Design references: DESIGN.md §6.4 (fail-closed restore), §10.3 (dual restore).

use spork_hash::Hash;
use thiserror::Error;

/// A failure of [`restore`](crate::RestoreGuard::restore) or
/// [`branch_fork`](crate::RestoreGuard::branch_fork).
///
/// Restore is **fail-closed**: any of these variants means *nothing changed* —
/// the working directory and refs are left exactly as they were before the call.
/// The variants distinguish *why* the guard refused so a caller (and the future
/// renderer) can surface a precise, truthful reason rather than a generic error.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum RestoreError {
    /// The code snapshot and the bound conversation refs disagree, or the node
    /// is not in a restorable shape (e.g. a mutating node missing its snapshot
    /// hash, or a node that does not exist). The restore rolled back and changed
    /// nothing — this is the precise §6.4 "partial restore" failure the guard
    /// exists to prevent.
    #[error("restore diverged, nothing changed: {detail}")]
    Divergence {
        /// A human-readable explanation of the divergence.
        detail: String,
    },

    /// The node's code snapshot object is absent from the content store, so the
    /// working tree cannot be materialized. The restore was refused before any
    /// byte was written.
    #[error("missing code snapshot {0} in the content store")]
    MissingSnapshot(Hash),

    /// The node's bound conversation ref does not resolve in the content store,
    /// so code-and-conversation cannot move together. The restore was refused
    /// before any byte was written (DESIGN §10.3).
    #[error("missing bound conversation {0} in the content store")]
    MissingConversation(Hash),

    /// An underlying event-log / graph / content-store operation failed (e.g.
    /// the writer actor is gone, or materialization hit an I/O fault). The guard
    /// treats this like any other failure: it does not record a restore event
    /// and reports the cause.
    #[error("restore log/store error: {0}")]
    Log(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    #[test]
    fn divergence_message_carries_detail() {
        let e = RestoreError::Divergence {
            detail: "conversation ref malformed".to_string(),
        };
        let msg = e.to_string();
        assert!(msg.contains("nothing changed"));
        assert!(msg.contains("conversation ref malformed"));
    }

    #[test]
    fn missing_object_messages_name_the_class() {
        let snap = hash_bytes(b"snap");
        let conv = hash_bytes(b"conv");
        assert!(RestoreError::MissingSnapshot(snap)
            .to_string()
            .contains("code snapshot"));
        assert!(RestoreError::MissingConversation(conv)
            .to_string()
            .contains("conversation"));
    }

    #[test]
    fn log_message_wraps_cause() {
        let e = RestoreError::Log("writer actor gone".to_string());
        assert!(e.to_string().contains("writer actor gone"));
    }
}
