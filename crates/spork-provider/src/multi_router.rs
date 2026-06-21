//! The [`MultiProviderRouter`] — P6 routing behind the frozen [`ModelRouter`].
//!
//! F4 shipped [`SingleProviderRouter`](crate::SingleProviderRouter) (everything
//! resolves to Anthropic). P6 adds this router *behind the same frozen
//! [`ModelRouter`] trait* (CLAUDE.md C3): a registry of providers each bound to a
//! [`Locality`], selector resolution for the widened
//! [`SelectorMode`](crate::SelectorMode) (`pinned | policy | inheritFromParent`),
//! [`PrivacyClass`] enforcement *before* it commits to a provider, and a fallback
//! chain with a circuit-breaker (DESIGN.md §12.1, §12.3, §12.5).
//!
//! Model keys use a `provider/model` convention (e.g. `openai/gpt-4o`,
//! `local/llama3.1`); a bare key (no `/`) routes to the default provider. The
//! bare model name keys the [`CapabilityRegistry`] lookup, so the resolved
//! capabilities (native vs. json-emulated tools, caching, …) come from the same
//! table the rest of the router consults (DESIGN.md §12.4).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{
    CapabilityRegistry, Locality, ModelRouter, ModelSelector, PrivacyClass, ProviderError,
    ResolvedModel, SelectorMode, ANTHROPIC_PROVIDER_KEY, CLI_PROVIDER_KEY, LOCAL_PROVIDER_KEY,
    OPENAI_PROVIDER_KEY,
};

/// How long a tripped provider's breaker stays **open** before it is retried
/// (half-open). A circuit *breaker* must close again — a breaker that never
/// recovers is a latch that would permanently degrade a long-lived daemon after a
/// single transient failure (DESIGN.md §12.3, §13 "policy + fallback + breaker").
/// After this cooldown a tripped provider is offered again to fallback/policy; a
/// fresh failure re-trips it. [`MultiProviderRouter::reset`] still clears it
/// immediately.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(30);

/// The provider key for an OpenAI-compatible **aggregator** (e.g. OpenRouter).
///
/// Distinct from [`OPENAI_PROVIDER_KEY`] only in [`Locality`]: an aggregator
/// relays to third parties, so a `no_third_party_aggregator` node is refused it
/// (DESIGN.md §12.5).
pub const AGGREGATOR_PROVIDER_KEY: &str = "openrouter";

/// One registered provider: its key, where it runs, and a representative model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBinding {
    /// The provider key (e.g. `"openai"`), the prefix of a `provider/model` key.
    pub provider: String,
    /// Where the provider runs — the axis [`PrivacyClass`] is checked against.
    pub locality: Locality,
    /// The model used when this provider is reached without an explicit model
    /// (the router default, or a fallback target).
    pub default_model: String,
}

impl ProviderBinding {
    /// Construct a binding.
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        locality: Locality,
        default_model: impl Into<String>,
    ) -> Self {
        ProviderBinding {
            provider: provider.into(),
            locality,
            default_model: default_model.into(),
        }
    }
}

/// A DAG-aware router over several providers (DESIGN.md §12.3).
#[derive(Debug)]
pub struct MultiProviderRouter {
    /// Registered providers in fallback order.
    bindings: Vec<ProviderBinding>,
    /// The provider a bare/empty model key routes to.
    default_provider: String,
    /// The capability table consulted on every resolution (DESIGN.md §12.4).
    capabilities: CapabilityRegistry,
    /// Circuit-breaker: each tripped provider mapped to *when* it tripped, so the
    /// breaker can half-open after [`BREAKER_COOLDOWN`] rather than latch open
    /// forever. Interior-mutable so the frozen `&self` [`ModelRouter`] methods can
    /// record a trip.
    tripped: Mutex<HashMap<String, Instant>>,
}

impl MultiProviderRouter {
    /// A router with the built-in providers registered in a sensible fallback
    /// order: Anthropic and OpenAI (first-party cloud), OpenRouter (aggregator),
    /// a local server, and a CLI agent. Default provider is Anthropic.
    #[must_use]
    pub fn with_builtin_providers() -> Self {
        MultiProviderRouter {
            bindings: vec![
                ProviderBinding::new(
                    ANTHROPIC_PROVIDER_KEY,
                    Locality::FirstPartyCloud,
                    "claude-3-5-sonnet",
                ),
                ProviderBinding::new(OPENAI_PROVIDER_KEY, Locality::FirstPartyCloud, "gpt-4o"),
                ProviderBinding::new(
                    AGGREGATOR_PROVIDER_KEY,
                    Locality::ThirdPartyAggregator,
                    "openrouter/auto",
                ),
                ProviderBinding::new(LOCAL_PROVIDER_KEY, Locality::Local, "llama3.1"),
                ProviderBinding::new(CLI_PROVIDER_KEY, Locality::Local, "copilot-cli"),
            ],
            default_provider: ANTHROPIC_PROVIDER_KEY.to_string(),
            capabilities: CapabilityRegistry::with_builtin_hints(),
            tripped: Mutex::new(HashMap::new()),
        }
    }

    /// An empty router; register providers with [`register`](Self::register).
    #[must_use]
    pub fn new(default_provider: impl Into<String>) -> Self {
        MultiProviderRouter {
            bindings: Vec::new(),
            default_provider: default_provider.into(),
            capabilities: CapabilityRegistry::empty(),
            tripped: Mutex::new(HashMap::new()),
        }
    }

    /// Register a provider binding (chainable). Order is fallback order.
    #[must_use]
    pub fn register(mut self, binding: ProviderBinding) -> Self {
        self.bindings.push(binding);
        self
    }

    /// Replace the capability registry the router consults.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: CapabilityRegistry) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Trip a provider's breaker open. It is skipped by fallback/policy until the
    /// [`BREAKER_COOLDOWN`] elapses (then half-opens and is retried) or
    /// [`reset`](Self::reset) clears it.
    pub fn trip(&self, provider: &str) {
        self.tripped
            .lock()
            .expect("breaker mutex poisoned")
            .insert(provider.to_string(), Instant::now());
    }

    /// Reset (close) a provider's breaker immediately.
    pub fn reset(&self, provider: &str) {
        self.tripped
            .lock()
            .expect("breaker mutex poisoned")
            .remove(provider);
    }

    /// Whether a provider's breaker is currently open (tripped within the last
    /// [`BREAKER_COOLDOWN`]). An expired entry has half-opened and is pruned, so
    /// the provider is retried.
    #[must_use]
    pub fn is_tripped(&self, provider: &str) -> bool {
        self.is_tripped_at(provider, Instant::now())
    }

    /// [`is_tripped`](Self::is_tripped) evaluated at an explicit instant — the
    /// testable seam for breaker recovery without sleeping. Prunes an expired
    /// entry (half-open) as a side effect.
    fn is_tripped_at(&self, provider: &str, now: Instant) -> bool {
        let mut tripped = self.tripped.lock().expect("breaker mutex poisoned");
        match tripped.get(provider) {
            Some(&at) if now.saturating_duration_since(at) < BREAKER_COOLDOWN => true,
            Some(_) => {
                // Cooldown elapsed: half-open — drop the trip so it is retried.
                tripped.remove(provider);
                false
            }
            None => false,
        }
    }

    /// Split a `provider/model` key into `(provider, bare_model)`. A key with no
    /// `/` whose prefix is not a registered provider routes to the default
    /// provider, keeping the whole string as the model name.
    fn split_key(&self, model_key: &str) -> (String, String) {
        if let Some((prefix, rest)) = model_key.split_once('/') {
            if self.binding_for(prefix).is_some() {
                return (prefix.to_string(), rest.to_string());
            }
        }
        (self.default_provider.clone(), model_key.to_string())
    }

    fn binding_for(&self, provider: &str) -> Option<&ProviderBinding> {
        self.bindings.iter().find(|b| b.provider == provider)
    }

    /// Resolve one concrete `model_key` (already mode-expanded) under `privacy`.
    fn resolve_key(
        &self,
        model_key: &str,
        privacy: PrivacyClass,
    ) -> Result<ResolvedModel, ProviderError> {
        // An empty key means "the default provider's default model".
        let (provider, bare) = if model_key.is_empty() {
            let binding = self.binding_for(&self.default_provider).ok_or_else(|| {
                ProviderError::NoSuchModel(format!("default provider '{}'", self.default_provider))
            })?;
            (self.default_provider.clone(), binding.default_model.clone())
        } else {
            self.split_key(model_key)
        };

        let binding = self
            .binding_for(&provider)
            .ok_or_else(|| ProviderError::NoSuchModel(model_key.to_string()))?;

        // Enforce privacy BEFORE committing to a provider (DESIGN.md §12.3/§12.5).
        if !privacy.permits(binding.locality) {
            return Err(ProviderError::PrivacyViolation {
                requested: privacy.as_str().to_string(),
                provider: provider.clone(),
            });
        }

        Ok(ResolvedModel {
            provider,
            locality: binding.locality,
            capabilities: self.capabilities.get(&bare),
            privacy,
        })
    }
}

impl ModelRouter for MultiProviderRouter {
    fn resolve(
        &self,
        selector: &ModelSelector,
        privacy: PrivacyClass,
    ) -> Result<ResolvedModel, ProviderError> {
        match &selector.mode {
            SelectorMode::Pinned => self.resolve_key(&selector.model_key, privacy),
            // Inherit defers to the parent: the daemon substitutes the parent's
            // concrete selector before calling; absent that, use the default.
            SelectorMode::InheritFromParent => self.resolve_key("", privacy),
            SelectorMode::Policy(policy) => {
                // Pick the first preference that is reachable (breaker closed) and
                // privacy-permitted; remember the last error for diagnostics.
                let mut last_err = ProviderError::NoSuchModel("empty policy".to_string());
                for key in &policy.prefer {
                    let (provider, _) = self.split_key(key);
                    if self.is_tripped(&provider) {
                        last_err = ProviderError::NoSuchModel(format!(
                            "provider '{provider}' breaker open"
                        ));
                        continue;
                    }
                    match self.resolve_key(key, privacy) {
                        Ok(resolved) => return Ok(resolved),
                        Err(e) => last_err = e,
                    }
                }
                Err(last_err)
            }
        }
    }

    fn next_fallback(&self, failed: &ResolvedModel) -> Option<ResolvedModel> {
        // The failed provider trips open, then we look for the next registered
        // provider (in order) that is still closed and permitted by the original
        // node's privacy class.
        self.trip(&failed.provider);
        let failed_idx = self
            .bindings
            .iter()
            .position(|b| b.provider == failed.provider);
        let start = failed_idx.map_or(0, |i| i + 1);
        for binding in self.bindings.iter().skip(start) {
            if self.is_tripped(&binding.provider) {
                continue;
            }
            if !failed.privacy.permits(binding.locality) {
                continue;
            }
            return Some(ResolvedModel {
                provider: binding.provider.clone(),
                locality: binding.locality,
                capabilities: self.capabilities.get(&binding.default_model),
                privacy: failed.privacy,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCalling;

    #[test]
    fn resolves_provider_slash_model() {
        let r = MultiProviderRouter::with_builtin_providers();
        let m = r
            .resolve(&ModelSelector::pinned("openai/gpt-4o"), PrivacyClass::Any)
            .unwrap();
        assert_eq!(m.provider, OPENAI_PROVIDER_KEY);
        assert_eq!(m.locality, Locality::FirstPartyCloud);
        assert_eq!(m.capabilities.tool_calling, ToolCalling::Native);
    }

    #[test]
    fn bare_key_routes_to_default_provider() {
        let r = MultiProviderRouter::with_builtin_providers();
        let m = r
            .resolve(
                &ModelSelector::pinned("claude-3-5-sonnet"),
                PrivacyClass::Any,
            )
            .unwrap();
        assert_eq!(m.provider, ANTHROPIC_PROVIDER_KEY);
    }

    #[test]
    fn local_only_refuses_cloud_but_permits_local() {
        let r = MultiProviderRouter::with_builtin_providers();
        // Cloud is refused for a local_only node...
        assert!(matches!(
            r.resolve(
                &ModelSelector::pinned("openai/gpt-4o"),
                PrivacyClass::LocalOnly
            ),
            Err(ProviderError::PrivacyViolation { .. })
        ));
        // ...and the local provider is permitted, with json-emulated tools.
        let local = r
            .resolve(
                &ModelSelector::pinned("local/llama3.1"),
                PrivacyClass::LocalOnly,
            )
            .unwrap();
        assert_eq!(local.locality, Locality::Local);
        assert_eq!(local.capabilities.tool_calling, ToolCalling::JsonEmulated);
    }

    #[test]
    fn no_aggregator_refuses_openrouter_but_permits_first_party() {
        let r = MultiProviderRouter::with_builtin_providers();
        assert!(matches!(
            r.resolve(
                &ModelSelector::pinned("openrouter/auto"),
                PrivacyClass::NoThirdPartyAggregator
            ),
            Err(ProviderError::PrivacyViolation { .. })
        ));
        assert!(r
            .resolve(
                &ModelSelector::pinned("openai/gpt-4o"),
                PrivacyClass::NoThirdPartyAggregator
            )
            .is_ok());
    }

    #[test]
    fn hot_swap_resolves_two_providers_for_the_same_node() {
        // The DoD hot-swap: a node on a frontier model later resolves a local one.
        let r = MultiProviderRouter::with_builtin_providers();
        let first = r
            .resolve(&ModelSelector::pinned("openai/gpt-4o"), PrivacyClass::Any)
            .unwrap();
        let swapped = r
            .resolve(&ModelSelector::pinned("local/llama3.1"), PrivacyClass::Any)
            .unwrap();
        assert_eq!(first.provider, OPENAI_PROVIDER_KEY);
        assert_eq!(swapped.provider, LOCAL_PROVIDER_KEY);
        assert_eq!(swapped.capabilities.tool_calling, ToolCalling::JsonEmulated);
    }

    #[test]
    fn policy_skips_aggregator_under_no_aggregator_and_picks_first_party() {
        let r = MultiProviderRouter::with_builtin_providers();
        let m = r
            .resolve(
                &ModelSelector::policy(["openrouter/auto", "openai/gpt-4o"]),
                PrivacyClass::NoThirdPartyAggregator,
            )
            .unwrap();
        assert_eq!(m.provider, OPENAI_PROVIDER_KEY);
    }

    #[test]
    fn fallback_trips_breaker_and_advances_then_stops() {
        let r = MultiProviderRouter::with_builtin_providers();
        let openai = r
            .resolve(&ModelSelector::pinned("openai/gpt-4o"), PrivacyClass::Any)
            .unwrap();
        // Fallback after OpenAI trips it and advances to the next provider.
        let next = r.next_fallback(&openai).unwrap();
        assert!(r.is_tripped(OPENAI_PROVIDER_KEY));
        assert_eq!(next.provider, AGGREGATOR_PROVIDER_KEY);
        // Keep falling back to the end of the chain, then None.
        let n2 = r.next_fallback(&next).unwrap();
        assert_eq!(n2.provider, LOCAL_PROVIDER_KEY);
        let n3 = r.next_fallback(&n2).unwrap();
        assert_eq!(n3.provider, CLI_PROVIDER_KEY);
        assert!(r.next_fallback(&n3).is_none());
    }

    #[test]
    fn breaker_half_opens_after_the_cooldown() {
        // A tripped provider must recover (half-open) after BREAKER_COOLDOWN — a
        // breaker that never closes is a latch that permanently degrades a
        // long-lived daemon. Verified without sleeping via the `is_tripped_at` seam.
        let r = MultiProviderRouter::with_builtin_providers();
        r.trip(LOCAL_PROVIDER_KEY);
        let now = Instant::now();
        // Still open immediately and within the cooldown window.
        assert!(r.is_tripped_at(LOCAL_PROVIDER_KEY, now));
        assert!(r.is_tripped_at(
            LOCAL_PROVIDER_KEY,
            now + BREAKER_COOLDOWN - Duration::from_millis(1)
        ));
        // Past the cooldown it half-opens (and is pruned), so it is retried.
        assert!(!r.is_tripped_at(
            LOCAL_PROVIDER_KEY,
            now + BREAKER_COOLDOWN + Duration::from_secs(1)
        ));
        assert!(
            !r.is_tripped(LOCAL_PROVIDER_KEY),
            "entry pruned after half-open"
        );
    }

    #[test]
    fn local_only_fallback_skips_cloud_providers() {
        let r = MultiProviderRouter::with_builtin_providers();
        // A local_only node resolved to the local provider; falling back must
        // only ever consider other LOCAL providers (the CLI agent), never cloud.
        let local = r
            .resolve(
                &ModelSelector::pinned("local/llama3.1"),
                PrivacyClass::LocalOnly,
            )
            .unwrap();
        let next = r.next_fallback(&local).unwrap();
        assert_eq!(next.provider, CLI_PROVIDER_KEY);
        assert_eq!(next.locality, Locality::Local);
        assert!(r.next_fallback(&next).is_none());
    }

    #[test]
    fn inherit_falls_back_to_default_without_a_parent() {
        let r = MultiProviderRouter::with_builtin_providers();
        let m = r
            .resolve(&ModelSelector::inherit(), PrivacyClass::Any)
            .unwrap();
        assert_eq!(m.provider, ANTHROPIC_PROVIDER_KEY);
    }
}
