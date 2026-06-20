//! The [`CapabilitySet`] a provider declares for a model.
//!
//! Despite superficial OpenAI-API convergence, support for parallel tool calls,
//! structured output, vision, and prompt caching varies per model and even per
//! local-server version. The router queries a model's `CapabilitySet` *before*
//! choosing a strategy — for example, falling back to a JSON-in-prompt tool
//! protocol for a model that lacks native tool-calling (DESIGN.md §12.4). F4
//! ships the static side of capability negotiation: the table value and where it
//! came from. The runtime probe that *refines* it is deferred to P6, and the
//! [`CapSource`] field is the seam that records which path produced a given set.

use serde::{Deserialize, Serialize};

/// How a model expresses tool calls.
///
/// Native tool-calling lets the provider parse and emit structured tool calls;
/// a model without it must be driven with a JSON-in-prompt convention the
/// adapter parses by hand (DESIGN.md §12.4). The router branches on this when it
/// picks an execution strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCalling {
    /// The provider has first-class tool-calling support.
    Native,
    /// Tool calls must be emulated via a JSON-in-prompt protocol the adapter
    /// parses (the `json_emulated` fallback in DESIGN.md §12.4).
    JsonEmulated,
}

/// Where a [`CapabilitySet`] came from.
///
/// A static-table value is a *hint*, not truth; a probed value was confirmed
/// against a live endpoint (DESIGN.md §12.4). Recording the source lets the
/// router treat a static hint conservatively and trust a probe. F4 only ever
/// produces [`CapSource::StaticTable`]; the probe path is P6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapSource {
    /// Seeded from the static capability table — a hint to be treated
    /// conservatively.
    StaticTable,
    /// Confirmed by a one-time runtime probe keyed by model+endpoint+version
    /// (P6).
    RuntimeProbe,
}

/// The declared capabilities of a single model.
///
/// Keyed by `model_key`, this is the value the
/// [`CapabilityRegistry`](DESIGN.md §12.4) stores. It serializes so it can be
/// persisted in the registry and recorded on a node's resolution; it carries no
/// schema-version field of its own because it is a *cache/hint* refreshed from
/// the table or a probe, never an identity-bearing persisted object (the
/// transcript and selector that *are* identity-bearing carry the versions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// The model this set describes (e.g. `"anthropic/claude-..."`).
    pub model_key: String,
    /// Whether the model can emit multiple tool calls in one turn.
    pub parallel_tool_calls: bool,
    /// Whether the model supports constrained / JSON structured output.
    pub structured_output: bool,
    /// Whether the model accepts image inputs.
    pub vision: bool,
    /// Whether the provider supports prompt caching (the cache-economics lever
    /// in DESIGN.md §12.5).
    pub prompt_caching: bool,
    /// How the model expresses tool calls.
    pub tool_calling: ToolCalling,
    /// Where this set's values came from.
    pub source: CapSource,
}
