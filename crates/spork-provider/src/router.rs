//! The [`ModelRouter`] seam and the v1 [`SingleProviderRouter`].
//!
//! Each DAG node stores a declarative [`ModelSelector`]; the router resolves it
//! to a concrete [`ResolvedModel`], enforcing the node's
//! [`PrivacyClass`](crate::PrivacyClass) *before* it commits to a provider, and
//! exposes a fallback chain for breaker/retry (DESIGN.md §12.1, §12.3). F4 ships
//! exactly one router behind the seam: [`SingleProviderRouter`], which resolves
//! every selector to the native Anthropic provider, refuses a
//! [`PrivacyClass::LocalOnly`](crate::PrivacyClass::LocalOnly) node (it has no
//! local provider), and returns `None` from
//! [`next_fallback`](ModelRouter::next_fallback) (there is nothing to fall back
//! to). Multi-provider routing tables and breakers are P6.

use serde::{Deserialize, Serialize};

use crate::{
    AnthropicAdapter, CapabilitySet, Locality, PrivacyClass, ProviderAdapter, ProviderError,
    ANTHROPIC_PROVIDER_KEY,
};

/// The frozen schema version of [`ModelSelector`] (CLAUDE.md C5).
pub const MODEL_SELECTOR_SCHEMA_VERSION: u16 = 1;

/// A node's declarative request for a model.
///
/// In v1 a selector is a `model_key` (which provider+model the node wants). In
/// P6 this widens additively to the full `pinned | policy | inheritFromParent`
/// form of DESIGN.md §12.3; the schema version is the no-domino seam that lets
/// that happen without reinterpreting a stored v1 selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSelector {
    /// The schema version of this selector shape.
    pub schema_version: u16,
    /// The requested model key. An empty key means "the router's default".
    pub model_key: String,
}

impl ModelSelector {
    /// A selector requesting a specific `model_key`.
    #[must_use]
    pub fn pinned(model_key: impl Into<String>) -> Self {
        ModelSelector {
            schema_version: MODEL_SELECTOR_SCHEMA_VERSION,
            model_key: model_key.into(),
        }
    }

    /// A selector requesting the router's default model.
    #[must_use]
    pub fn default_model() -> Self {
        ModelSelector {
            schema_version: MODEL_SELECTOR_SCHEMA_VERSION,
            model_key: String::new(),
        }
    }
}

/// A concrete model the router committed to for a node.
///
/// Records the `provider` key, its [`Locality`] (the axis privacy is checked
/// against), the resolved `capabilities`, and the originating `privacy` class so
/// downstream code can see the constraint that produced this binding. It
/// serializes so it can be recorded on the node (DESIGN.md §12.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModel {
    /// The provider key the selector resolved to (e.g. `"anthropic"`).
    pub provider: String,
    /// Where that provider runs, for the privacy check.
    pub locality: Locality,
    /// The model's declared capabilities (DESIGN.md §12.4).
    pub capabilities: CapabilitySet,
    /// The privacy class under which this model was resolved.
    pub privacy: PrivacyClass,
}

/// Resolves a [`ModelSelector`] to a [`ResolvedModel`], enforcing privacy and
/// exposing a fallback chain.
///
/// This is the seam the agent loop calls before each request. F4 ships one
/// implementation ([`SingleProviderRouter`]); the trait does not change in P6.
pub trait ModelRouter {
    /// Resolve `selector` under `privacy`.
    ///
    /// # Errors
    /// - [`ProviderError::PrivacyViolation`] if the only model the router can
    ///   provide is forbidden by `privacy` (e.g. a `LocalOnly` node and a cloud
    ///   provider).
    /// - [`ProviderError::NoSuchModel`] if no model is registered for the
    ///   selector.
    fn resolve(
        &self,
        selector: &ModelSelector,
        privacy: PrivacyClass,
    ) -> Result<ResolvedModel, ProviderError>;

    /// The next model to try after `failed` failed, if any.
    ///
    /// Returns `None` in v1 (single provider, nothing to fall back to). The
    /// breaker/fallback policy is P6.
    fn next_fallback(&self, failed: &ResolvedModel) -> Option<ResolvedModel>;
}

/// The v1 router: resolves every selector to the native Anthropic provider.
///
/// It enforces [`PrivacyClass`](crate::PrivacyClass) before resolving — a
/// `LocalOnly` node is refused because Anthropic is first-party *cloud*, not
/// local — and has no fallback chain. This is the one real
/// [`ModelRouter`](ModelRouter) F4 ships (CLAUDE.md C3); multi-provider routing
/// is P6 (DESIGN.md §12.3).
#[derive(Debug, Clone)]
pub struct SingleProviderRouter {
    adapter: AnthropicAdapter,
}

impl Default for SingleProviderRouter {
    fn default() -> Self {
        SingleProviderRouter::new()
    }
}

impl SingleProviderRouter {
    /// The locality of the one provider this router offers: first-party cloud.
    pub const PROVIDER_LOCALITY: Locality = Locality::FirstPartyCloud;

    /// Create a router backed by the default Anthropic adapter.
    #[must_use]
    pub fn new() -> Self {
        SingleProviderRouter {
            adapter: AnthropicAdapter::new(),
        }
    }

    /// Create a router backed by a specific Anthropic adapter (model).
    #[must_use]
    pub fn with_adapter(adapter: AnthropicAdapter) -> Self {
        SingleProviderRouter { adapter }
    }

    /// The provider key this router resolves to.
    #[must_use]
    pub fn provider(&self) -> &'static str {
        ANTHROPIC_PROVIDER_KEY
    }
}

impl ModelRouter for SingleProviderRouter {
    fn resolve(
        &self,
        selector: &ModelSelector,
        privacy: PrivacyClass,
    ) -> Result<ResolvedModel, ProviderError> {
        // Enforce privacy *before* committing to a provider: a LocalOnly node
        // must never resolve to a cloud provider (DESIGN.md §12.3, §12.5).
        if !privacy.permits(Self::PROVIDER_LOCALITY) {
            return Err(ProviderError::PrivacyViolation {
                requested: privacy.as_str().to_string(),
                provider: ANTHROPIC_PROVIDER_KEY.to_string(),
            });
        }

        // Resolve the model. An empty selector means "the router's default".
        let adapter = if selector.model_key.is_empty() {
            self.adapter.clone()
        } else {
            AnthropicAdapter::with_model(selector.model_key.clone())
        };

        Ok(ResolvedModel {
            provider: ANTHROPIC_PROVIDER_KEY.to_string(),
            locality: Self::PROVIDER_LOCALITY,
            capabilities: adapter.describe(),
            privacy,
        })
    }

    fn next_fallback(&self, _failed: &ResolvedModel) -> Option<ResolvedModel> {
        // v1 has a single provider; there is nothing to fall back to.
        None
    }
}
