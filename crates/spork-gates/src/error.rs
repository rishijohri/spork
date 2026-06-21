//! The gate error taxonomy.

use thiserror::Error;

/// Errors raised while evaluating a gate.
///
/// `#[non_exhaustive]` so future variants are additive (CLAUDE.md C2). Note that
/// an *unsatisfiable* predicate is **not** an error — it produces a
/// [`Decision::Blocked`](crate::Decision) verdict with reasons. Errors here are
/// reserved for structural failures (a verdict that cannot be hashed).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum GateError {
    /// Canonical encoding of a verdict failed (only possible if a float entered a
    /// struct, which the integer-only schemas prevent).
    #[error("canonical encoding failed: {0}")]
    Canon(String),
}

/// A convenience result alias for the gates crate.
pub type Result<T> = std::result::Result<T, GateError>;
