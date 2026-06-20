//! Error type for the identity layer.
//!
//! Parsing a self-describing address can fail in exactly three ways, and each
//! is a distinct, actionable condition the caller must be able to discriminate.
//! In particular [`HashError::UnknownGeneration`] is the load-bearing "no-domino"
//! signal: an object minted under a hash generation this binary does not
//! understand is *rejected*, never silently re-interpreted (DESIGN.md §6.1,
//! Appendix A.7 constraint C-3).

use thiserror::Error;

/// Errors produced while parsing or validating Spork identity values.
///
/// These arise from [`crate::ObjectId`]'s [`std::str::FromStr`] implementation
/// and from [`crate::Hash`] hex parsing. They are intentionally exhaustive over
/// the failure modes of a self-describing address so callers can react
/// precisely (for example, surfacing an [`UnknownGeneration`](HashError::UnknownGeneration)
/// as "this object was written by a newer Spork" rather than as generic
/// corruption).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HashError {
    /// The algorithm label in an address is not one this build recognizes.
    ///
    /// v1 understands only `b3` (BLAKE3). Any other label is refused rather
    /// than guessed at, so a future algorithm can be added behind the same
    /// seam without older binaries misreading its addresses.
    #[error("unknown hash algorithm: {0:?}")]
    UnknownAlgo(String),

    /// The generation number is syntactically valid but not supported here.
    ///
    /// The generation is the "no-domino" version seam on the hash itself
    /// (DESIGN.md §6.1, A.7 C-3). When the canonical encoding or hash family
    /// is revised, the new objects carry a higher generation; an older binary
    /// must *reject* them — never rehash or reinterpret in place — which is
    /// exactly this error.
    #[error("unknown hash generation: {0}")]
    UnknownGeneration(u8),

    /// The input was not a well-formed address/hash at the syntactic level.
    ///
    /// Examples: missing the `<tag>:<hex>` separator, an empty tag, a
    /// non-numeric generation, or a hex digest that is not exactly 32 bytes
    /// of lowercase hexadecimal.
    #[error("malformed identity value: {0}")]
    Malformed(String),
}
