//! The baseline error taxonomy.

use thiserror::Error;

/// Errors raised while comparing results against a baseline or scoring
/// flakiness.
///
/// `#[non_exhaustive]` so future variants are additive (CLAUDE.md C2).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BaselineError {
    /// A metric the baseline pins was absent from the result envelope, so the
    /// gate cannot evaluate its delta. Surfaced rather than silently treated as
    /// "no regression" — a missing metric is a real evaluation gap (DESIGN.md
    /// §8.3).
    #[error("baseline metric {0:?} is absent from the result")]
    MissingMetric(String),

    /// Canonical encoding failed (only possible if a float entered a struct,
    /// which the integer-only schemas prevent).
    #[error("canonical encoding failed: {0}")]
    Canon(String),
}

/// A convenience result alias for the baseline crate.
pub type Result<T> = std::result::Result<T, BaselineError>;
