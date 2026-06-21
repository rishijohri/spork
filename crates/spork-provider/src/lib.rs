//! Spork F4 model seam — the canonical transcript and provider mapping.
//!
//! This crate freezes the model-access seam: a provider-neutral
//! `CanonicalTranscript` and the `ProviderAdapter` port that maps it to and from
//! a concrete provider's wire format. F4 ships the mapping only — the hard,
//! testable part — and adds no network client; the HTTP transport and
//! multi-provider routing are deferred to P6. Per the foundation discipline
//! (CLAUDE.md C3) the trait is the seam and F4 ships exactly one real adapter
//! and one real router behind it.
//!
//! The frozen surface (filled in as the F4 implementation lands):
//!
//! - `CanonicalTranscript` / `CanonicalTurn` / `ContentBlock` / `Role` — the
//!   provider-neutral, schema-versioned conversation (CLAUDE.md C5), with an
//!   `OpaqueProviderBlock` escape hatch for provider-specific payloads.
//! - `ProviderAdapter` — the port: `describe`, `to_wire`, `from_wire`,
//!   `raw_passthrough`. F4 implements the native `AnthropicAdapter`, which maps
//!   a canonical transcript to and from Anthropic Messages JSON (system,
//!   messages, content blocks, `tool_use`, `tool_result`) with no HTTP, tested
//!   against handwritten / recorded Anthropic-shaped fixtures.
//! - `ProviderProjection` — re-renders the canonical transcript to a target
//!   provider; a cross-provider projection drops `OpaqueProviderBlock`s tagged
//!   for other providers and records a `lossyProjection` warning.
//! - `ModelRouter` — selector resolution. F4 ships `SingleProviderRouter`,
//!   which resolves every selector to Anthropic, enforces `PrivacyClass` (a
//!   `LocalOnly` node must not resolve to a cloud provider), and exposes
//!   `next_fallback` (`None` in v1). Multi-provider routing is P6.
//! - `CapabilitySet` / `PrivacyClass` — the declared model capabilities and the
//!   privacy constraint the router enforces.
//!
//! This realizes the provider-abstraction model in DESIGN.md §5.4 ("Model
//! access & providers") and the canonical transcript, projection, and routing
//! seams in §12.1-§12.3.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod adapter;
mod anthropic;
mod capability;
mod error;
mod openai;
mod privacy;
mod projection;
mod router;
mod transcript;

pub use adapter::ProviderAdapter;
pub use anthropic::AnthropicAdapter;
pub use capability::{CapSource, CapabilitySet, ToolCalling};
pub use error::ProviderError;
pub use openai::OpenAiAdapter;
pub use privacy::{Locality, PrivacyClass};
pub use projection::{LossyProjection, Projected, ProviderProjection};
pub use router::{
    ModelRouter, ModelSelector, ResolvedModel, SingleProviderRouter, MODEL_SELECTOR_SCHEMA_VERSION,
};
pub use transcript::{
    CanonicalTranscript, CanonicalTurn, ContentBlock, OpaqueProviderBlock, Role,
    CANONICAL_TRANSCRIPT_SCHEMA_VERSION,
};

/// The provider key for the native Anthropic adapter.
///
/// This is the single, stable string under which Anthropic-owned
/// [`OpaqueProviderBlock`]s are tagged and to which the v1
/// [`SingleProviderRouter`] resolves. Keeping it a crate constant means the wire
/// mapping, the projection drop test, and the router all agree on one spelling
/// (DESIGN.md §12.3).
pub const ANTHROPIC_PROVIDER_KEY: &str = "anthropic";

/// The provider key for the OpenAI-compatible adapter ([`OpenAiAdapter`]).
///
/// The stable string under which OpenAI-owned [`OpaqueProviderBlock`]s (e.g.
/// reasoning) are tagged; the multi-provider router (P6) keys this adapter under
/// it. One OpenAI-compatible wire shape serves OpenAI, OpenRouter, vLLM, and
/// most local servers — they differ by endpoint/locality, not wire format.
pub const OPENAI_PROVIDER_KEY: &str = "openai";
