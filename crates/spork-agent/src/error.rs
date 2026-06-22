//! The agent-orchestration error taxonomy: [`AgentError`].

use thiserror::Error;

/// A failure running one agent turn.
///
/// `#[non_exhaustive]` so later phases (tool-execution loops in P8, streaming)
/// can add variants additively without breaking a downstream `match`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The selector could not be resolved, or a wire mapping failed — a
    /// deterministic provider-seam error (privacy violation, unknown model,
    /// unmappable content, malformed response). These do **not** trigger
    /// fallback: re-trying the same deterministic mapping would fail identically.
    #[error(transparent)]
    Provider(#[from] spork_provider::ProviderError),

    /// No built-in adapter is registered for a resolved provider key. A P8
    /// plugin provider would register its adapter; until then an unknown
    /// provider is surfaced rather than silently skipped.
    #[error("no adapter for provider '{0}'")]
    NoAdapter(String),

    /// The [`TransportResolver`](crate::TransportResolver) has no transport
    /// configured for the resolved provider. The agent loop treats this as a
    /// *fallbackable* reachability failure (so a daemon that configures only
    /// local/CLI transports gracefully declines a cloud resolution and falls
    /// back) — it is not, on its own, a fatal error unless the whole chain is
    /// unconfigured.
    #[error("no transport configured for provider '{0}'")]
    NoTransport(String),

    /// Every provider in the fallback chain failed at the transport level. Names
    /// the number of attempts and the last transport error for diagnosis — the
    /// loop tried, and exhausted, the router's fallback chain (DESIGN §12.3).
    #[error("all {attempts} provider(s) failed; last transport error: {last}")]
    AllProvidersFailed {
        /// How many providers were attempted (the first plus every fallback).
        attempts: u32,
        /// The last transport error encountered, rendered for diagnostics.
        last: String,
    },
}
