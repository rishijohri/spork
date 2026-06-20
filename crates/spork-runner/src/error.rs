//! The crate's error type, [`RunnerError`].
//!
//! Every fallible operation in the result-normalization seam — preparing a check
//! against a materialized worktree, running it, normalizing raw output into a
//! [`ResultEnvelope`](crate::ResultEnvelope), and reading or writing the
//! content-addressed derivation cache — funnels its failures into
//! [`RunnerError`]. The variants are deliberately discriminable so callers can
//! react precisely: an unknown metric id is a distinct, typed condition the
//! metric-id registry raises so a silent gate break is impossible
//! ([`RunnerError::UnknownMetric`]); a check kind a runner does not implement is
//! a typed refusal ([`RunnerError::UnsupportedKind`]); a malformed `config`
//! payload is reported with the field at fault ([`RunnerError::Config`]).
//!
//! The enum is `#[non_exhaustive]` so later runner adapters (TestRunner,
//! StressRunner, and user-defined kinds in the P phases) can add variants
//! without a breaking change to downstream `match`es — the no-domino discipline
//! (CLAUDE.md C2/C3).
//!
//! Design references: DESIGN.md §8.1 (`CheckSpec` / Runner SPI / one
//! `ResultEnvelope`), §8.2 (execution against a materialized tree + the result
//! cache), §4.4 (`inputDigest` / `derivationKey`).

use thiserror::Error;

/// Errors produced by the result-normalization seam.
///
/// Spans the surfaces of the seam: check dispatch
/// ([`UnsupportedKind`](RunnerError::UnsupportedKind),
/// [`Config`](RunnerError::Config)), the metric-id registry
/// ([`UnknownMetric`](RunnerError::UnknownMetric)), execution delegated to the
/// isolation backend ([`Exec`](RunnerError::Exec)), the content-addressed cache
/// ([`Cache`](RunnerError::Cache)), and the canonical-encoding / I/O plumbing
/// beneath them.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RunnerError {
    /// A [`CheckSpec`](crate::CheckSpec) named a `kind` this runner does not
    /// implement.
    ///
    /// F4 ships exactly one runner ([`SanityRunner`](crate::SanityRunner) for
    /// the `"sanity"` kind); test/stress/user kinds are added additively behind
    /// the same [`Runner`](crate::Runner) trait (P phases). This is a complete,
    /// deterministic refusal naming the kind — not a stub.
    #[error("check kind {kind:?} is not implemented by runner {runner:?}")]
    UnsupportedKind {
        /// The kind requested by the spec.
        kind: String,
        /// The runner that was asked to handle it.
        runner: String,
    },

    /// A [`CheckSpec::config`](crate::CheckSpec::config) payload was malformed —
    /// a required field was missing, had the wrong JSON type, or held an invalid
    /// value (e.g. a bad glob pattern or a zero line limit).
    ///
    /// Carries a human-readable explanation naming the field at fault so a
    /// misconfigured check fails loudly at prepare time rather than silently
    /// producing a wrong result.
    #[error("invalid check config: {0}")]
    Config(String),

    /// A metric id was referenced that the metric-id registry does not know.
    ///
    /// All metrics flow through a registry so a metric *rename* is contained in
    /// one place and can never silently break a gate that keys off the old id
    /// (DESIGN.md §8.1, the load-bearing `ResultEnvelope`). Emitting or
    /// resolving an unregistered id is this typed error, never a silent pass.
    #[error("unknown metric id {0:?} (not present in the metric-id registry)")]
    UnknownMetric(String),

    /// The underlying isolation backend ([`spork_exec`]) failed while
    /// provisioning, executing, or capturing.
    ///
    /// The runner delegates the actual sandbox work to an
    /// [`IsolationBackend`](spork_exec::IsolationBackend); a failure there
    /// (lease lapsed, command could not spawn, path escape, …) is surfaced here
    /// with the backend's own message preserved.
    #[error("execution backend error: {0}")]
    Exec(String),

    /// A content-addressed derivation-cache operation failed — a stored envelope
    /// could not be decoded, or its blob could not be read or written.
    ///
    /// Carries an explanation; a cache failure is never silently treated as a
    /// miss-that-poisons, it is surfaced so the caller can decide.
    #[error("result cache error: {0}")]
    Cache(String),

    /// The canonical encoding of a hashed record (a [`CheckSpec`](crate::CheckSpec)
    /// config, the cache key inputs, or a [`ResultEnvelope`](crate::ResultEnvelope))
    /// failed.
    ///
    /// Carries the [`spork_canon::CanonError`] message; a non-byte-stable input
    /// (a float that slipped into a config) is reported rather than silently
    /// producing an unstable digest.
    #[error("canonicalization failed: {0}")]
    Canon(String),

    /// A `ResultEnvelope` (or another persisted record) could not be
    /// (de)serialized through `serde_json`.
    #[error("serialization error: {0}")]
    Serialize(String),

    /// An underlying I/O operation failed while reading the materialized
    /// worktree, tagged with the path it targeted.
    #[error("i/o error at {path}: {source}")]
    Io {
        /// The filesystem path the operation was targeting.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

impl RunnerError {
    /// Build a [`RunnerError::Io`] tagged with the path it occurred at.
    pub(crate) fn io(path: impl std::fmt::Display, source: std::io::Error) -> Self {
        RunnerError::Io {
            path: path.to_string(),
            source,
        }
    }

    /// Build a [`RunnerError::Config`] from a message.
    pub(crate) fn config(msg: impl Into<String>) -> Self {
        RunnerError::Config(msg.into())
    }
}

impl From<spork_exec::ExecError> for RunnerError {
    fn from(e: spork_exec::ExecError) -> Self {
        RunnerError::Exec(e.to_string())
    }
}

impl From<spork_canon::CanonError> for RunnerError {
    fn from(e: spork_canon::CanonError) -> Self {
        RunnerError::Canon(e.to_string())
    }
}

impl From<serde_json::Error> for RunnerError {
    fn from(e: serde_json::Error) -> Self {
        RunnerError::Serialize(e.to_string())
    }
}

/// Convenience result alias for the crate's fallible operations.
pub type Result<T> = std::result::Result<T, RunnerError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_kind_names_both_parties() {
        let e = RunnerError::UnsupportedKind {
            kind: "test".into(),
            runner: "sanity".into(),
        };
        let s = e.to_string();
        assert!(s.contains("test"));
        assert!(s.contains("sanity"));
    }

    #[test]
    fn exec_error_converts() {
        let exec = spork_exec::ExecError::Cancelled;
        let e: RunnerError = exec.into();
        assert!(matches!(e, RunnerError::Exec(_)));
    }

    #[test]
    fn canon_error_converts() {
        let canon = spork_canon::CanonError::FloatNotAllowed;
        let e: RunnerError = canon.into();
        assert!(matches!(e, RunnerError::Canon(_)));
    }

    #[test]
    fn io_helper_carries_path() {
        let e = RunnerError::io(
            "/ws/src/main.rs",
            std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        );
        assert!(e.to_string().contains("/ws/src/main.rs"));
    }
}
