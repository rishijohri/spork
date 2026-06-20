//! The provider-layer error taxonomy: [`ProviderError`].
//!
//! Every fallible operation in this crate — wire mapping in both directions,
//! projection to a target provider, and selector resolution in the router —
//! funnels through one `#[non_exhaustive]` error enum so callers match a single
//! taxonomy and new failure modes can be added additively in later phases
//! without a breaking change (DESIGN.md §12.1, §12.6).

use thiserror::Error;

/// Errors produced by the provider seam.
///
/// The enum is `#[non_exhaustive]`: F4 freezes the *seam*, not the exhaustive
/// list of every way a future networked adapter might fail, so variants may be
/// added in P6+ without breaking downstream `match`es (DESIGN.md §12.6).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// A wire payload handed to [`from_wire`](crate::ProviderAdapter::from_wire)
    /// did not match the shape this adapter understands.
    ///
    /// This is the normalization-drift guard from DESIGN.md §12.6: a missing
    /// `role`, an unknown content-block `type`, or a `tool_result` with no
    /// `tool_use_id` is rejected loudly here rather than silently dropping a
    /// tool-call id and corrupting the agent loop.
    #[error("malformed provider wire payload: {0}")]
    MalformedWire(String),

    /// A canonical transcript could not be rendered into a provider's wire
    /// format.
    ///
    /// Raised by [`to_wire`](crate::ProviderAdapter::to_wire) when the canonical
    /// content cannot be represented in the target provider's schema (for
    /// example a role the provider has no slot for).
    #[error("cannot render canonical transcript to wire: {0}")]
    UnmappableContent(String),

    /// The router was asked to resolve a selector whose
    /// [`PrivacyClass`](crate::PrivacyClass) forbids the only model the router
    /// can provide.
    ///
    /// The v1 [`SingleProviderRouter`](crate::SingleProviderRouter) resolves
    /// every selector to the cloud Anthropic provider, so a
    /// [`PrivacyClass::LocalOnly`](crate::PrivacyClass::LocalOnly) node — which
    /// must never reach a third party — is refused here rather than silently
    /// downgraded (DESIGN.md §12.3, §12.5).
    #[error("privacy class {requested} forbids resolving to provider '{provider}'")]
    PrivacyViolation {
        /// The privacy class the caller requested, rendered for diagnostics.
        requested: String,
        /// The provider the router would otherwise have resolved to.
        provider: String,
    },

    /// No model is registered for the requested selector.
    ///
    /// In v1 this only fires for an explicitly empty selector key; multi-model
    /// routing tables arrive in P6.
    #[error("no model registered for selector '{0}'")]
    NoSuchModel(String),

    /// Canonical serialization of an identity-bearing payload failed.
    ///
    /// Wraps a [`spork_canon::CanonError`] so the float-prohibition and
    /// serialize errors of the canonical encoder surface through the provider
    /// taxonomy (DESIGN.md §6.1).
    #[error("canonical serialization failed: {0}")]
    Canon(#[from] spork_canon::CanonError),
}
