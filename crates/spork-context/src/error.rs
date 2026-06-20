//! The context-layer error taxonomy: [`ContextError`].
//!
//! Every fallible operation in this crate — compiling a node's context,
//! generating a handoff, or hashing the stable prefix — funnels through one
//! `#[non_exhaustive]` error enum so callers match a single taxonomy and new
//! failure modes (lineage walking, ancestor selection) can be added additively
//! in P7 without a breaking change (CLAUDE.md C2 — no domino; DESIGN.md §13.2).

use thiserror::Error;

use crate::layer::ContextLayerKind;

/// A failure of the context seam.
///
/// The enum is `#[non_exhaustive]`: F4 freezes the *seam* (the layered,
/// `prefix_hash`-keyed compiler and the regenerable handoff), not the exhaustive
/// list of every way the P7 lineage-walking compiler might fail, so variants may
/// be added later without breaking downstream `match`es (DESIGN.md §13.2).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ContextError {
    /// The node the compiler or handoff generator was asked to operate on does
    /// not exist in the supplied source.
    ///
    /// Raised before any layer is assembled, so a missing node never yields a
    /// half-built [`CompiledContext`](crate::CompiledContext).
    #[error("node not found in context source: {0}")]
    NodeNotFound(String),

    /// A layer required by the policy could not be produced by the source.
    ///
    /// For example, an [`AncestorStrategy::HandoffOnly`](crate::AncestorStrategy)
    /// policy that finds no handoff for an ancestor, or a
    /// [`Degrade::Fail`](crate::Degrade) policy whose assembled context exceeds
    /// the budget. The offending [`ContextLayerKind`] is named so the
    /// [`SelectionDecision`](crate::SelectionDecision) trace and the error agree
    /// on the cause (DESIGN.md §13.3, §13.6).
    #[error("required context layer {kind:?} could not be produced: {reason}")]
    MissingLayer {
        /// The kind of layer that could not be produced.
        kind: ContextLayerKind,
        /// A human-readable reason, surfaced in diagnostics.
        reason: String,
    },

    /// The assembled context exceeded the policy budget and the policy's
    /// [`Degrade`](crate::Degrade) mode is [`Degrade::Fail`](crate::Degrade::Fail).
    ///
    /// The compiler reports the budget and the assembled size so the caller can
    /// surface a precise "context too large" diagnostic rather than silently
    /// truncating (the tuning hazard called out in DESIGN.md §13.3).
    #[error("context budget exceeded: assembled {assembled} tokens > budget {budget}")]
    BudgetExceeded {
        /// The policy's `exploration_budget_tokens`.
        budget: u64,
        /// The estimated token size of the assembled context.
        assembled: u64,
    },

    /// Canonical serialization of an identity-bearing payload failed.
    ///
    /// Wraps a [`spork_canon::CanonError`] so the float-prohibition and
    /// serialize errors of the canonical encoder — the encoder the `prefix_hash`
    /// and `lineage_hash` are computed through — surface through this taxonomy
    /// (DESIGN.md §6.1, §13.2).
    #[error("canonical serialization failed: {0}")]
    Canon(#[from] spork_canon::CanonError),

    /// A provider-layer operation (transcript hashing, projection) failed.
    ///
    /// Wraps a [`spork_provider::ProviderError`] so a handoff built from a
    /// content-addressed transcript reports the provider failure through the
    /// context taxonomy (DESIGN.md §12.3, §13.5).
    #[error("provider error: {0}")]
    Provider(#[from] spork_provider::ProviderError),
}
