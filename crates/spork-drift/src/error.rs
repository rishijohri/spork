//! Error type for drift capture (DESIGN.md §10.1, §10.2, §15.5).

use thiserror::Error;

/// The error type for the drift-capture pipeline.
///
/// Every fallible operation in this crate — fusing change sources, walking the
/// working tree, scanning for secrets, putting non-secret bytes into the CAS,
/// and recording the auto-drift node through the graph — funnels its failure
/// modes here so a caller has one error surface to match on.
///
/// It is `#[non_exhaustive]` because the drift pipeline gains capture sources
/// and capture stages over time (DESIGN.md §10.2); adding a variant must not be
/// a breaking change for downstream matchers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DriftError {
    /// A filesystem operation (read, walk, metadata) failed.
    ///
    /// The string includes the offending path and the OS error so the cause is
    /// self-describing without leaking a non-`Send` `io::Error` into the type.
    #[error("filesystem error: {0}")]
    Io(String),

    /// Storing the non-secret working tree into the content-addressed store
    /// failed (DESIGN.md §10.1 content layer).
    #[error("content store error: {0}")]
    Cas(String),

    /// Recording the auto-drift snapshot node through the graph service failed
    /// (DESIGN.md §10.1 timeline layer).
    #[error("graph error: {0}")]
    Graph(String),

    /// A configured secret-pattern regex failed to compile.
    ///
    /// The built-in [`crate::SecretScanner::with_default_patterns`] never returns
    /// this (its patterns are fixed and known-good); it surfaces only when a
    /// caller supplies a custom, invalid pattern.
    #[error("invalid secret pattern: {0}")]
    Pattern(String),
}

impl DriftError {
    /// Construct an [`DriftError::Io`] from a path and the underlying error.
    pub(crate) fn io(path: impl AsRef<std::path::Path>, err: &std::io::Error) -> Self {
        DriftError::Io(format!("{}: {err}", path.as_ref().display()))
    }
}

/// A convenient `Result` alias for the drift pipeline.
pub type Result<T> = std::result::Result<T, DriftError>;
