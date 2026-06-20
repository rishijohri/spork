//! The crate's error type, [`NodesError`].
//!
//! Building, registering, and running the six built-in node types funnels its
//! failures into [`NodesError`]. The variants are deliberately discriminable so a
//! caller can react precisely: a registry rejection (a malformed descriptor, a
//! duplicate version) is distinct from a malformed payload, a runner failure, or
//! a merge that could not be reconciled.
//!
//! The enum is `#[non_exhaustive]` so later built-in node types or richer error
//! detail can be added without a breaking change to downstream `match`es — the
//! no-domino discipline (CLAUDE.md C2/C3).
//!
//! Design references: DESIGN.md §6.2 (node envelope + payload), §6.5 / A.4
//! (merge), §7.1 (the built-in taxonomy), §8.1 / §8.2 (the Runner SPI).

use thiserror::Error;

/// Errors produced when building, registering, or running the built-in node
/// types.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodesError {
    /// Registering a built-in descriptor through the public
    /// [`NodeTypeRegistry`](spork_registry::NodeTypeRegistry) was rejected.
    ///
    /// The built-ins go through the *same* registration path a third party uses
    /// (DESIGN.md §7.1), so they are subject to the same contract — notably the
    /// `owns_snapshot` ⇒ `SnapshotRef` out-port rule and the duplicate-version
    /// rule. A rejection here is the registry's typed error, surfaced verbatim.
    #[error("registry rejected a built-in descriptor: {0}")]
    Registry(#[from] spork_registry::RegistryError),

    /// A node payload did not satisfy its versioned schema — a required field was
    /// missing, had the wrong type, or held an out-of-range value.
    ///
    /// Carries a human-readable explanation naming the field at fault so a
    /// malformed payload fails loudly rather than producing a wrong node.
    #[error("invalid node payload: {0}")]
    Payload(String),

    /// A built-in observing [`Runner`](spork_runner::Runner) failed while
    /// preparing, executing, or normalizing a check.
    ///
    /// The wrapped [`RunnerError`](spork_runner::RunnerError) is preserved so the
    /// caller can tell an unsupported kind from a config error from a backend
    /// failure (DESIGN.md §8.1).
    #[error("runner error: {0}")]
    Runner(#[from] spork_runner::RunnerError),

    /// A three-way merge could not be computed — a referenced snapshot tree could
    /// not be read, or an input was malformed.
    ///
    /// This is *not* a conflict: a conflicting merge is reported as data (a
    /// [`MergeOutcome::Conflicts`](crate::merge::MergeOutcome::Conflicts) conflict
    /// set), never as an error (DESIGN.md A.4). This variant is reserved for the
    /// inability to *attempt* the reconciliation at all.
    #[error("merge could not be computed: {0}")]
    Merge(String),

    /// An underlying content-store operation failed (reading a tree/blob to
    /// reconcile, or writing a merged tree).
    #[error("content store error: {0}")]
    Cas(String),

    /// A persisted record could not be (de)serialized through `serde_json`.
    #[error("serialization error: {0}")]
    Serialize(String),
}

impl From<spork_cas::CasError> for NodesError {
    fn from(e: spork_cas::CasError) -> Self {
        NodesError::Cas(e.to_string())
    }
}

impl From<serde_json::Error> for NodesError {
    fn from(e: serde_json::Error) -> Self {
        NodesError::Serialize(e.to_string())
    }
}

/// Convenience result alias for the crate's fallible operations.
pub type Result<T> = std::result::Result<T, NodesError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_error_converts() {
        let cas = spork_cas::CasError::Canon("nope".into());
        let e: NodesError = cas.into();
        assert!(matches!(e, NodesError::Cas(_)));
    }

    #[test]
    fn serde_error_converts() {
        let bad: std::result::Result<serde_json::Value, _> = serde_json::from_str("{");
        let e: NodesError = bad.unwrap_err().into();
        assert!(matches!(e, NodesError::Serialize(_)));
    }

    #[test]
    fn registry_error_converts() {
        let reg = spork_registry::RegistryError::OwnsSnapshotMismatch { id: "x".into() };
        let e: NodesError = reg.into();
        assert!(matches!(e, NodesError::Registry(_)));
    }

    #[test]
    fn payload_and_merge_messages_are_informative() {
        assert!(NodesError::Payload("missing field origin".into())
            .to_string()
            .contains("origin"));
        assert!(NodesError::Merge("no common ancestor".into())
            .to_string()
            .contains("common ancestor"));
    }
}
