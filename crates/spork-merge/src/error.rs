//! The merge-schema error taxonomy: [`MergeSchemaError`].
//!
//! The merge crate freezes *data shapes*, so its failures are narrow: the only
//! fallible operation is content-addressing a payload, which can fail if the
//! canonical encoder rejects a value. Every such failure funnels through one
//! `#[non_exhaustive]` enum so the P5 Merge node and the P9 graft (the
//! downstream consumers, DESIGN.md §6.5, A.4) match a single taxonomy and new
//! failure modes can be added additively later without a breaking change
//! (CLAUDE.md C2).

use thiserror::Error;

/// Errors produced by the merge-schema crate.
///
/// The enum is `#[non_exhaustive]`: F4 freezes the merge *schemas*, not the
/// exhaustive list of every way a future merge engine might fail, so variants
/// may be added in P5/P9 (an actual three-way file merge, a graft validation)
/// without breaking downstream `match`es (CLAUDE.md C2; DESIGN.md A.4).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MergeSchemaError {
    /// Canonical serialization of an identity-bearing merge payload failed.
    ///
    /// Wraps a [`spork_canon::CanonError`] so the float-prohibition and
    /// serialize errors of the canonical encoder surface through the merge
    /// taxonomy. Content-addressing a [`SyntheticTranscript`](crate::SyntheticTranscript)
    /// or a [`ConflictResolution`](crate::ConflictResolution) is the only place
    /// this can arise, and the typed schemas here never introduce a float, so in
    /// practice it does not fire — but it is surfaced rather than hidden so the
    /// hashing contract is honest (DESIGN.md §6.1).
    #[error("canonical serialization failed: {0}")]
    Canon(#[from] spork_canon::CanonError),
}
