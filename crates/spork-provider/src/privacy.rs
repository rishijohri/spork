//! The [`PrivacyClass`] constraint the router enforces.
//!
//! Spork is local-first: a node may declare that its prompt and conversation
//! must never leave the machine, or never reach a hosted aggregator. The router
//! enforces this *before* it resolves a model, so a privacy guarantee is a
//! routing precondition rather than an after-the-fact audit (DESIGN.md §12.3,
//! §12.5).

use serde::{Deserialize, Serialize};

/// How far a node's data is permitted to travel when a model is resolved for it.
///
/// This is a per-node policy the [`ModelRouter`](crate::ModelRouter) honors. The
/// variants form a widening ladder of permission — `LocalOnly` is the
/// strictest, `Any` the most permissive — and the router refuses any resolution
/// that would exceed a node's declared class (DESIGN.md §12.5: "routing all
/// traffic through a third party conflicts with Spork's local-first/privacy
/// wedge").
///
/// `PrivacyClass` is plain config, not identity-bearing on its own, but it
/// serializes (it is recorded on a node's selector) so it round-trips through
/// the same canonical encoding as the rest of the model selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    /// Data must never leave the local machine.
    ///
    /// Only a local model server satisfies this. The v1 router has no local
    /// provider, so it *refuses* a `LocalOnly` resolution with
    /// [`ProviderError::PrivacyViolation`](crate::ProviderError::PrivacyViolation)
    /// rather than silently sending the prompt to the cloud.
    LocalOnly,

    /// Data may reach a first-party provider's API, but never a hosted
    /// multi-tenant aggregator (e.g. OpenRouter) that re-routes to third
    /// parties.
    ///
    /// A direct first-party cloud provider such as Anthropic satisfies this; a
    /// hosted aggregator does not.
    NoThirdPartyAggregator,

    /// No privacy restriction: any reachable provider may serve the node.
    Any,
}

impl PrivacyClass {
    /// Whether a model hosted by `provider` with the given
    /// [`Locality`] is permitted under this privacy class.
    ///
    /// This is the single decision the router consults. It is intentionally
    /// total and side-effect-free so the policy is auditable in isolation.
    ///
    /// # Example
    /// ```
    /// use spork_provider::{PrivacyClass, Locality};
    ///
    /// // LocalOnly admits only a local model.
    /// assert!(PrivacyClass::LocalOnly.permits(Locality::Local));
    /// assert!(!PrivacyClass::LocalOnly.permits(Locality::FirstPartyCloud));
    ///
    /// // NoThirdPartyAggregator admits a first-party cloud but not an aggregator.
    /// assert!(PrivacyClass::NoThirdPartyAggregator.permits(Locality::FirstPartyCloud));
    /// assert!(!PrivacyClass::NoThirdPartyAggregator.permits(Locality::ThirdPartyAggregator));
    ///
    /// // Any admits everything reachable.
    /// assert!(PrivacyClass::Any.permits(Locality::ThirdPartyAggregator));
    /// ```
    #[must_use]
    pub fn permits(self, locality: Locality) -> bool {
        match self {
            PrivacyClass::LocalOnly => locality == Locality::Local,
            PrivacyClass::NoThirdPartyAggregator => locality != Locality::ThirdPartyAggregator,
            PrivacyClass::Any => true,
        }
    }

    /// A short, stable label for diagnostics and error messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PrivacyClass::LocalOnly => "local_only",
            PrivacyClass::NoThirdPartyAggregator => "no_third_party_aggregator",
            PrivacyClass::Any => "any",
        }
    }
}

/// Where a provider physically runs, from the data-egress point of view.
///
/// This is the axis [`PrivacyClass::permits`] checks. It is a property of a
/// resolved provider, not of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// Runs on the user's machine (e.g. Ollama / LM Studio). No data egress.
    Local,
    /// A first-party model vendor's own API (e.g. Anthropic, OpenAI). Single
    /// hop to a named vendor.
    FirstPartyCloud,
    /// A hosted multi-tenant aggregator (e.g. OpenRouter) that itself relays to
    /// other vendors.
    ThirdPartyAggregator,
}
