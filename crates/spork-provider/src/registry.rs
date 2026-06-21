//! The [`CapabilityRegistry`] — static capability hints + a runtime-probe seam.
//!
//! Despite OpenAI-API convergence, support for parallel tool calls, structured
//! output, vision, and prompt caching varies per model and even per local-server
//! version, so the router must know a model's [`CapabilitySet`] *before* it picks
//! a strategy — e.g. dropping to a JSON-in-prompt tool protocol for a model with
//! no native tool-calling (DESIGN.md §12.4).
//!
//! F4 froze the static side ([`CapSource::StaticTable`]). P6 adds this registry:
//! a seeded static hint table plus a one-time [`CapabilityProbe`] seam that
//! *refines* a hint into a confirmed [`CapSource::RuntimeProbe`] value. The probe
//! is a trait so it is offline-testable (a fixture probe) and a real networked
//! probe drops in behind it later without touching the registry.

use std::collections::HashMap;

use crate::{CapSource, CapabilitySet, ToolCalling};

/// A one-time capability probe for a model+endpoint.
///
/// Implementors confirm (or correct) a model's static-table hint against the
/// live endpoint. Returning `None` means "could not probe" — the registry keeps
/// the conservative static hint. A real implementation issues a tiny request and
/// inspects the response/headers; tests supply a fixture probe (DESIGN.md §12.4).
pub trait CapabilityProbe {
    /// Probe `model_key`, returning a confirmed capability set if reachable.
    fn probe(&self, model_key: &str) -> Option<CapabilitySet>;
}

/// A registry of model capabilities: seeded static hints, refinable by a probe.
///
/// `get` always returns *some* set — a seeded hint, a probed value, or a
/// conservative default for an unknown model — so the router can always make a
/// strategy decision. Values carry their [`CapSource`] so the router can treat a
/// static hint cautiously and trust a probe.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    table: HashMap<String, CapabilitySet>,
}

impl CapabilityRegistry {
    /// An empty registry (no seeded hints).
    #[must_use]
    pub fn empty() -> Self {
        CapabilityRegistry {
            table: HashMap::new(),
        }
    }

    /// A registry seeded with the static hints for the built-in model families.
    ///
    /// These mirror the adapters' own `describe` hints (DESIGN.md §12.4: the
    /// table is a hint, treated conservatively until a probe confirms it).
    #[must_use]
    pub fn with_builtin_hints() -> Self {
        let mut reg = CapabilityRegistry::empty();
        reg.insert(CapabilitySet {
            model_key: "claude-3-5-sonnet".into(),
            parallel_tool_calls: true,
            structured_output: true,
            vision: true,
            prompt_caching: true,
            tool_calling: ToolCalling::Native,
            source: CapSource::StaticTable,
        });
        reg.insert(CapabilitySet {
            model_key: "gpt-4o".into(),
            parallel_tool_calls: true,
            structured_output: true,
            vision: true,
            prompt_caching: false,
            tool_calling: ToolCalling::Native,
            source: CapSource::StaticTable,
        });
        reg.insert(CapabilitySet {
            model_key: "llama3.1".into(),
            parallel_tool_calls: false,
            structured_output: false,
            vision: false,
            prompt_caching: false,
            tool_calling: ToolCalling::JsonEmulated,
            source: CapSource::StaticTable,
        });
        reg.insert(CapabilitySet {
            model_key: "copilot-cli".into(),
            parallel_tool_calls: false,
            structured_output: false,
            vision: false,
            prompt_caching: false,
            tool_calling: ToolCalling::JsonEmulated,
            source: CapSource::StaticTable,
        });
        reg
    }

    /// Insert or replace a model's capability set (keyed by its `model_key`).
    pub fn insert(&mut self, set: CapabilitySet) {
        self.table.insert(set.model_key.clone(), set);
    }

    /// The capabilities for `model_key`.
    ///
    /// Returns the seeded/probed set if known, else a **conservative default**:
    /// no parallel tools, no structured output, no vision, no caching, and
    /// [`ToolCalling::JsonEmulated`] — the safe assumption for an unknown model,
    /// so the router never over-promises a capability the model lacks
    /// (DESIGN.md §12.4).
    #[must_use]
    pub fn get(&self, model_key: &str) -> CapabilitySet {
        self.table
            .get(model_key)
            .cloned()
            .unwrap_or_else(|| Self::conservative_default(model_key))
    }

    /// The conservative capability set assumed for an unknown model.
    #[must_use]
    pub fn conservative_default(model_key: &str) -> CapabilitySet {
        CapabilitySet {
            model_key: model_key.to_string(),
            parallel_tool_calls: false,
            structured_output: false,
            vision: false,
            prompt_caching: false,
            tool_calling: ToolCalling::JsonEmulated,
            source: CapSource::StaticTable,
        }
    }

    /// Refine `model_key` from a probe, storing the confirmed value tagged
    /// [`CapSource::RuntimeProbe`].
    ///
    /// A no-op (keeping the static hint) if the probe returns `None`. Returns the
    /// set now in effect for the model. The probe is expected to run **once** per
    /// model+endpoint+version and the result cached here (DESIGN.md §12.4).
    pub fn refine(&mut self, model_key: &str, probe: &dyn CapabilityProbe) -> CapabilitySet {
        if let Some(mut probed) = probe.probe(model_key) {
            // The probe path is authoritative: record where it came from.
            probed.source = CapSource::RuntimeProbe;
            probed.model_key = model_key.to_string();
            self.table.insert(model_key.to_string(), probed.clone());
            probed
        } else {
            self.get(model_key)
        }
    }

    /// The tool-calling strategy for a model: native vs. JSON-in-prompt emulation
    /// (DESIGN.md §12.4). The single decision the router branches on.
    #[must_use]
    pub fn tool_strategy(&self, model_key: &str) -> ToolCalling {
        self.get(model_key).tool_calling
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureProbe(CapabilitySet);
    impl CapabilityProbe for FixtureProbe {
        fn probe(&self, _model_key: &str) -> Option<CapabilitySet> {
            Some(self.0.clone())
        }
    }

    struct UnreachableProbe;
    impl CapabilityProbe for UnreachableProbe {
        fn probe(&self, _model_key: &str) -> Option<CapabilitySet> {
            None
        }
    }

    #[test]
    fn builtin_hints_are_static_table() {
        let reg = CapabilityRegistry::with_builtin_hints();
        let claude = reg.get("claude-3-5-sonnet");
        assert_eq!(claude.source, CapSource::StaticTable);
        assert_eq!(claude.tool_calling, ToolCalling::Native);
        assert!(claude.prompt_caching);
    }

    #[test]
    fn unknown_model_is_conservative_json_emulated() {
        let reg = CapabilityRegistry::with_builtin_hints();
        let unknown = reg.get("some/new-model");
        assert_eq!(unknown.tool_calling, ToolCalling::JsonEmulated);
        assert!(!unknown.parallel_tool_calls);
        assert!(!unknown.prompt_caching);
    }

    #[test]
    fn local_model_hint_is_json_emulated() {
        let reg = CapabilityRegistry::with_builtin_hints();
        assert_eq!(reg.tool_strategy("llama3.1"), ToolCalling::JsonEmulated);
    }

    #[test]
    fn probe_upgrades_source_to_runtime_and_overrides_hint() {
        let mut reg = CapabilityRegistry::with_builtin_hints();
        // The static hint for llama3.1 is json_emulated; a probe discovers the
        // local server actually supports native tools.
        let probe = FixtureProbe(CapabilitySet {
            model_key: "ignored".into(),
            parallel_tool_calls: true,
            structured_output: true,
            vision: false,
            prompt_caching: false,
            tool_calling: ToolCalling::Native,
            source: CapSource::StaticTable, // refine() forces RuntimeProbe
        });
        let refined = reg.refine("llama3.1", &probe);
        assert_eq!(refined.source, CapSource::RuntimeProbe);
        assert_eq!(refined.tool_calling, ToolCalling::Native);
        assert_eq!(refined.model_key, "llama3.1");
        // And it is now what the registry returns.
        assert_eq!(reg.get("llama3.1").source, CapSource::RuntimeProbe);
    }

    #[test]
    fn unreachable_probe_keeps_static_hint() {
        let mut reg = CapabilityRegistry::with_builtin_hints();
        let kept = reg.refine("claude-3-5-sonnet", &UnreachableProbe);
        assert_eq!(kept.source, CapSource::StaticTable);
        assert_eq!(reg.get("claude-3-5-sonnet").source, CapSource::StaticTable);
    }
}
