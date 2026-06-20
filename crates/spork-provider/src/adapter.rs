//! The owned [`ProviderAdapter`] port.
//!
//! This is the hexagonal port that normalizes every provider's request,
//! response, and tool-call shape behind one canonical form, so the underlying
//! engine can be swapped without touching the agent loop (DESIGN.md §12.1). F4
//! freezes the *port* and ships exactly one real implementation behind it — the
//! native [`AnthropicAdapter`](crate::AnthropicAdapter) mapping (CLAUDE.md C3);
//! the OpenAI-compatible, local-server, and CLI-subprocess adapters are P6.

use serde_json::Value;

use crate::{CanonicalTranscript, CapabilitySet, ProviderError};

/// A single provider's canonical request/response/tool-call mapping.
///
/// The two halves of the seam are [`to_wire`](ProviderAdapter::to_wire) (render
/// the stored canonical transcript into the provider's request JSON) and
/// [`from_wire`](ProviderAdapter::from_wire) (parse a provider response back
/// into canonical turns). F4 implements the *mapping* only — there is no HTTP
/// here; the network transport that would call the provider with the
/// `to_wire` payload and feed the response to `from_wire` is P6.
///
/// The mapping is the genuinely hard, testable part: tool-call id schemes, role
/// conventions, system-prompt placement, and content-block typing all differ
/// per provider, and a lost id or dropped block silently corrupts the agent
/// loop (DESIGN.md §12.1, §12.6). Implementations are therefore expected to be
/// covered by contract tests against recorded/handwritten fixtures.
pub trait ProviderAdapter {
    /// Describe the model this adapter serves.
    ///
    /// Returns the static [`CapabilitySet`] the router consults before choosing
    /// a strategy (DESIGN.md §12.4). It is stable for a given adapter instance.
    fn describe(&self) -> CapabilitySet;

    /// Render a canonical transcript into this provider's request wire JSON.
    ///
    /// # Errors
    /// Returns [`ProviderError::UnmappableContent`] if some canonical content
    /// has no representation in the provider's schema.
    fn to_wire(&self, transcript: &CanonicalTranscript) -> Result<Value, ProviderError>;

    /// Parse a provider response payload back into canonical turns.
    ///
    /// Takes `&self` deliberately: an adapter is bound to a model/endpoint, and
    /// parsing may depend on that binding, so `from_wire` is the symmetric
    /// counterpart of [`to_wire`](ProviderAdapter::to_wire) on the same
    /// instance (this is the frozen port signature from the F4 contract, hence
    /// the `wrong_self_convention` allow).
    ///
    /// # Errors
    /// Returns [`ProviderError::MalformedWire`] if the payload does not match
    /// the shape this adapter understands (a missing role, an unknown
    /// content-block type, a tool result with no correlating id) — drift is
    /// rejected loudly rather than swallowed (DESIGN.md §12.6).
    #[allow(clippy::wrong_self_convention)]
    fn from_wire(&self, wire: Value) -> Result<CanonicalTranscript, ProviderError>;

    /// Whether this adapter exposes the per-adapter raw-passthrough escape
    /// hatch.
    ///
    /// The hatch lets a caller send a provider-native payload unmediated when a
    /// new provider feature is not yet exposed through the canonical form
    /// (DESIGN.md §12.2). The native [`AnthropicAdapter`](crate::AnthropicAdapter)
    /// supports it.
    fn raw_passthrough(&self) -> bool;
}
