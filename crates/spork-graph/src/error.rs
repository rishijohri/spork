//! The command-layer error type: [`GraphError`].
//!
//! Every public [`GraphService`](crate::GraphService) command returns
//! `Result<_, GraphError>`. The variants enumerate the *validation* failures the
//! command layer enforces before it ever appends to the log (an unknown kind, an
//! `owns_snapshot`/`snapshot_hash` mismatch, a disallowed-or-cyclic edge, a
//! missing node), plus the *infrastructure* failures of the substrate it sits on
//! (the F1 log and the canonical encoder). Keeping validation and infrastructure
//! errors in one enum lets a caller match on the precise reason a mutation was
//! rejected.
//!
//! `#[non_exhaustive]` so new rejection reasons can be added additively without
//! breaking downstream matchers (CLAUDE.md C2 — no domino).
//!
//! Design references: DESIGN.md §6.2, §6.3, §6.5, §7.2 (the validate-then-append
//! contract these errors enforce).

use thiserror::Error;
use ulid::Ulid;

/// A failure of a [`GraphService`](crate::GraphService) command.
///
/// The validation variants ([`UnknownKind`](GraphError::UnknownKind),
/// [`OwnsSnapshotMismatch`](GraphError::OwnsSnapshotMismatch),
/// [`Edge`](GraphError::Edge), [`MissingNode`](GraphError::MissingNode)) are
/// raised *before* any event is appended, so a rejected mutation leaves the log
/// and projection untouched. The infrastructure variants
/// ([`Log`](GraphError::Log), [`Canon`](GraphError::Canon)) wrap failures of the
/// underlying F1 log and canonical encoder.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GraphError {
    /// The requested `kind` (optionally at a specific `type_version`) does not
    /// resolve in the [`NodeTypeRegistry`](spork_registry::NodeTypeRegistry).
    ///
    /// A node can only be created for a registered type — built-ins register
    /// through the same public path third parties use (DESIGN §6.5, §7.1).
    #[error("unknown node kind: {id}")]
    UnknownKind {
        /// The unresolved kind (with version, when one was requested).
        id: String,
    },

    /// The create request's snapshot ownership disagrees with the type
    /// descriptor or with the presence of a `snapshot_hash`.
    ///
    /// A `snapshot_hash` must be present **iff** the resolved descriptor's
    /// `owns_snapshot` is `true` (DESIGN §6.2, §7.2). The registry separately
    /// guarantees that an `owns_snapshot` descriptor exposes a `SnapshotRef`
    /// out-port at registration time.
    #[error("owns_snapshot/snapshot_hash mismatch for the requested node type")]
    OwnsSnapshotMismatch,

    /// An edge could not be added: it is cyclic or its type is not allowed by
    /// the `from`-node's descriptor (DESIGN §6.3).
    #[error(transparent)]
    Edge(#[from] spork_edges::EdgeError),

    /// A referenced node does not exist in the projection.
    ///
    /// Raised by [`add_edge`](crate::GraphService::add_edge) when `from` or `to`
    /// is unknown, so an edge is never appended against a dangling endpoint.
    #[error("node not found: {id}")]
    MissingNode {
        /// The id of the missing node.
        id: Ulid,
    },

    /// The underlying F1 event log (append or read) failed.
    ///
    /// Wrapped as a string so this crate's error type does not leak the
    /// `spork-log` error variants into its own stable surface.
    #[error("event log error: {0}")]
    Log(String),

    /// Canonical serialization failed (for example, a payload carried a float,
    /// which is forbidden in identity-bearing data — see `spork-canon`).
    #[error("canonicalization error: {0}")]
    Canon(String),
}

impl From<spork_log::LogError> for GraphError {
    fn from(e: spork_log::LogError) -> Self {
        GraphError::Log(e.to_string())
    }
}

impl From<spork_canon::CanonError> for GraphError {
    fn from(e: spork_canon::CanonError) -> Self {
        GraphError::Canon(e.to_string())
    }
}

/// A convenient `Result` alias for command-layer operations.
pub type Result<T> = std::result::Result<T, GraphError>;
