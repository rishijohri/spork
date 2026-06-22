//! Spork P6 **agent-turn orchestration** — the loop that runs one model turn end
//! to end across the frozen F4 model seam.
//!
//! F4 froze the pieces (canonical transcript, [`ProviderAdapter`], [`ModelRouter`],
//! [`PrivacyClass`]); P6 added the multi-provider router, the per-provider
//! adapters, the [`CostAccountant`](spork_cost::CostAccountant), and the
//! [`Transport`](spork_transport::Transport) seam. This crate ties them together
//! into the single operation the daemon's agent-run path calls:
//!
//! 1. **Resolve** the [`ModelSelector`] under the node's [`PrivacyClass`] through
//!    the [`ModelRouter`] — privacy is enforced here, *before* any byte leaves
//!    (a `local_only` node can never resolve to cloud, DESIGN §12.3/§12.5).
//! 2. **Render** the canonical transcript with the resolved provider's adapter
//!    ([`builtin_adapter`]).
//! 3. **Carry** the request over the resolved provider's [`Transport`].
//! 4. **Parse** the reply back to canonical turns with the same adapter.
//! 5. **Price** the turn: extract token [`usage`] and run it through the
//!    [`CostAccountant`] into a per-node [`CostRecord`](spork_graph::CostRecord).
//!
//! On a **transport-level** failure (the provider couldn't be reached, or has no
//! configured transport here) the loop walks the router's
//! [`next_fallback`](ModelRouter::next_fallback) chain — which trips the failed
//! provider's breaker and advances to the next reachable, **privacy-permitted**
//! provider — until one answers or the chain is exhausted (DESIGN §12.3). A
//! *deterministic* failure (privacy violation, unmappable content, a malformed
//! reply) is returned immediately: retrying the same mapping would fail
//! identically.
//!
//! This crate has **no node/graph knowledge**: it returns an [`AgentTurnResult`]
//! (the output transcript + the model attributed + the cost); the daemon decides
//! how that becomes a node (the §6.6 attach/branch policy lives there, not here).
//!
//! Design references: DESIGN §5.4, §12.1, §12.3, §12.5.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
pub mod usage;

pub use error::AgentError;

use spork_cost::{CostAccountant, Usage};
use spork_graph::CostRecord;
use spork_provider::{
    AnthropicAdapter, CanonicalTranscript, CanonicalTurn, CliAdapter, Locality, ModelRouter,
    ModelSelector, OpenAiAdapter, PrivacyClass, ProviderAdapter, ResolvedModel, Role,
    AGGREGATOR_PROVIDER_KEY, ANTHROPIC_PROVIDER_KEY, CLI_PROVIDER_KEY, LOCAL_PROVIDER_KEY,
    OPENAI_PROVIDER_KEY,
};
use spork_transport::Transport;

/// Supplies the [`Transport`] for a resolved provider.
///
/// The daemon implements this from its configuration (a local server's
/// `http://` endpoint, a CLI agent's program + args), so this crate stays free of
/// any I/O-config knowledge. Returning `Err` for a provider that has no transport
/// configured here is treated as a *reachability* failure: the loop falls back to
/// the next provider, exactly as a connection failure would (so a daemon that
/// only configures local + CLI transports gracefully declines a cloud
/// resolution).
pub trait TransportResolver {
    /// The transport to reach `resolved`, or an error if none is configured.
    ///
    /// # Errors
    /// Returns [`AgentError::NoTransport`] when this resolver has no transport for
    /// the resolved provider; the agent loop treats that as a fallbackable
    /// reachability failure.
    fn transport_for(&self, resolved: &ResolvedModel) -> Result<Box<dyn Transport>, AgentError>;
}

/// The result of one completed agent turn.
#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    /// The provider key that actually answered (after any fallback).
    pub provider: String,
    /// The concrete model that answered (the cost/pricing key).
    pub model_key: String,
    /// Where that provider ran (the privacy axis), for attribution/auditing.
    pub locality: Locality,
    /// The model's reply, parsed back into canonical turns.
    pub output: CanonicalTranscript,
    /// The token usage the provider reported (zero if it reported none).
    pub usage: Usage,
    /// The priced cost of the turn — the record the node envelope carries.
    pub cost: CostRecord,
    /// How many fallbacks were taken before a provider answered (0 = the first
    /// provider answered).
    pub fallbacks: u32,
}

/// The built-in [`ProviderAdapter`] for a resolved `provider`/`model_key`.
///
/// Covers every provider the P6 [`MultiProviderRouter`](spork_provider::MultiProviderRouter)
/// can resolve to. A future (P8) plugin provider would add its own adapter; until
/// then an unrecognized provider key returns `None`, which the loop surfaces as
/// [`AgentError::NoAdapter`] rather than silently mis-routing.
#[must_use]
pub fn builtin_adapter(provider: &str, model_key: &str) -> Option<Box<dyn ProviderAdapter>> {
    match provider {
        ANTHROPIC_PROVIDER_KEY => Some(Box::new(AnthropicAdapter::with_model(model_key))),
        // OpenAI and an OpenAI-compatible aggregator (OpenRouter) share the wire
        // shape and support native tool-calls.
        OPENAI_PROVIDER_KEY | AGGREGATOR_PROVIDER_KEY => {
            Some(Box::new(OpenAiAdapter::with_model(model_key)))
        }
        // A local OpenAI-compatible server (Ollama/LM Studio/vLLM): same wire,
        // json-emulated tools by default (DESIGN §12.4).
        LOCAL_PROVIDER_KEY => Some(Box::new(OpenAiAdapter::local(model_key))),
        CLI_PROVIDER_KEY => Some(Box::new(CliAdapter::with_model(model_key))),
        _ => None,
    }
}

/// Build a single-user-turn canonical transcript from a prompt — the common
/// shape for an "ask the agent" run before a full tool loop exists (P8).
#[must_use]
pub fn user_prompt(text: impl Into<String>) -> CanonicalTranscript {
    CanonicalTranscript::new(vec![CanonicalTurn::text(Role::User, text)])
}

/// Run one agent turn: resolve → render → carry → parse → price, with the
/// router's fallback chain engaged on transport failure.
///
/// See the crate docs for the full contract. `input` is the conversation so far
/// (for a first "ask" that is a single user turn — see [`user_prompt`]).
///
/// # Errors
/// - [`AgentError::Provider`] for a deterministic seam failure (privacy
///   violation, unknown model, unmappable content, malformed reply) — returned
///   immediately, no fallback.
/// - [`AgentError::NoAdapter`] if a resolved provider has no built-in adapter.
/// - [`AgentError::AllProvidersFailed`] if every provider in the fallback chain
///   failed at the transport level.
pub fn run_turn(
    router: &dyn ModelRouter,
    transports: &dyn TransportResolver,
    accountant: &CostAccountant,
    selector: &ModelSelector,
    privacy: PrivacyClass,
    input: &CanonicalTranscript,
) -> Result<AgentTurnResult, AgentError> {
    // (1) Resolve under privacy — a violation propagates as a hard error here,
    // before any byte is rendered or sent.
    let mut resolved = router.resolve(selector, privacy)?;
    let mut attempts: u32 = 0;

    loop {
        let model_key = resolved.capabilities.model_key.clone();
        // (2) Pick the adapter. An unknown provider key is a config bug, not a
        // reachability problem — surface it, don't fall back into a loop.
        let adapter = builtin_adapter(&resolved.provider, &model_key)
            .ok_or_else(|| AgentError::NoAdapter(resolved.provider.clone()))?;
        // Render the request (deterministic; a mapping failure is hard).
        let request = adapter.to_wire(input)?;

        attempts += 1;
        // (3) Carry it. A missing transport for this provider is a reachability
        // failure, treated identically to a connection failure → fall back.
        let outcome = match transports.transport_for(&resolved) {
            Ok(transport) => transport.invoke(&request).map_err(|e| e.to_string()),
            Err(AgentError::NoTransport(msg)) => Err(msg),
            Err(other) => return Err(other),
        };

        match outcome {
            Ok(response) => {
                // (4) Parse back — a malformed reply is a hard error (the provider
                // answered, but with garbage; retrying won't help).
                let output = adapter.from_wire(response.clone())?;
                // (5) Price the turn.
                let turn_usage = usage::extract_usage(&resolved.provider, &response);
                let cost = accountant.cost_for(&model_key, &turn_usage);
                return Ok(AgentTurnResult {
                    provider: resolved.provider,
                    model_key,
                    locality: resolved.locality,
                    output,
                    usage: turn_usage,
                    cost,
                    fallbacks: attempts - 1,
                });
            }
            Err(msg) => match router.next_fallback(&resolved) {
                Some(next) => {
                    resolved = next;
                    continue;
                }
                None => {
                    return Err(AgentError::AllProvidersFailed {
                        attempts,
                        last: msg,
                    })
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use spork_provider::MultiProviderRouter;
    use spork_transport::{Transport, TransportError};

    /// A transport that returns a canned response, or errors, per provider — lets
    /// the loop be exercised fully offline without any real model.
    struct ScriptedTransports {
        /// provider key → Ok(response) or Err(message).
        replies: std::collections::HashMap<String, Result<Value, String>>,
    }
    impl ScriptedTransports {
        fn new() -> Self {
            ScriptedTransports {
                replies: std::collections::HashMap::new(),
            }
        }
        fn ok(mut self, provider: &str, resp: Value) -> Self {
            self.replies.insert(provider.into(), Ok(resp));
            self
        }
        fn fail(mut self, provider: &str, msg: &str) -> Self {
            self.replies.insert(provider.into(), Err(msg.into()));
            self
        }
    }
    struct CannedTransport(Result<Value, String>);
    impl Transport for CannedTransport {
        fn invoke(&self, _request: &Value) -> Result<Value, TransportError> {
            self.0.clone().map_err(TransportError::Io)
        }
    }
    impl TransportResolver for ScriptedTransports {
        fn transport_for(
            &self,
            resolved: &ResolvedModel,
        ) -> Result<Box<dyn Transport>, AgentError> {
            match self.replies.get(&resolved.provider) {
                Some(reply) => Ok(Box::new(CannedTransport(reply.clone()))),
                None => Err(AgentError::NoTransport(resolved.provider.clone())),
            }
        }
    }

    /// An OpenAI-compatible response carrying one assistant message and usage.
    fn openai_reply() -> Value {
        json!({
            "choices": [{ "message": { "role": "assistant", "content": "done" } }],
            "usage": { "prompt_tokens": 1000, "completion_tokens": 200 }
        })
    }

    #[test]
    fn runs_a_turn_and_prices_it() {
        let router = MultiProviderRouter::with_builtin_providers();
        let transports = ScriptedTransports::new().ok(OPENAI_PROVIDER_KEY, openai_reply());
        let acc = CostAccountant::with_builtin_pricing();
        let out = run_turn(
            &router,
            &transports,
            &acc,
            &ModelSelector::pinned("openai/gpt-4o"),
            PrivacyClass::Any,
            &user_prompt("hello"),
        )
        .unwrap();
        assert_eq!(out.provider, OPENAI_PROVIDER_KEY);
        assert_eq!(out.model_key, "gpt-4o");
        assert_eq!(out.fallbacks, 0);
        // gpt-4o: 1000 in @ $2.5/Mtok = 2500 micro; 200 out @ $10/Mtok = 2000.
        assert_eq!(out.cost.micro_usd, 2_500 + 2_000);
        assert_eq!(out.cost.input_tokens, 1_000);
        assert_eq!(out.cost.output_tokens, 200);
    }

    #[test]
    fn local_only_is_refused_cloud_before_any_transport() {
        let router = MultiProviderRouter::with_builtin_providers();
        // No transport would even be consulted: the privacy check fails at resolve.
        let transports = ScriptedTransports::new().ok(OPENAI_PROVIDER_KEY, openai_reply());
        let acc = CostAccountant::with_builtin_pricing();
        let err = run_turn(
            &router,
            &transports,
            &acc,
            &ModelSelector::pinned("openai/gpt-4o"),
            PrivacyClass::LocalOnly,
            &user_prompt("secret"),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AgentError::Provider(spork_provider::ProviderError::PrivacyViolation { .. })
        ));
    }

    #[test]
    fn falls_back_when_the_first_provider_transport_fails() {
        let router = MultiProviderRouter::with_builtin_providers();
        // Anthropic (default/first) fails at transport; OpenRouter has no
        // transport (skipped); local answers. The chain is
        // anthropic → openai → openrouter → local → cli.
        let transports = ScriptedTransports::new()
            .fail(ANTHROPIC_PROVIDER_KEY, "503 upstream")
            .fail(OPENAI_PROVIDER_KEY, "500 boom")
            .ok(LOCAL_PROVIDER_KEY, openai_reply());
        let acc = CostAccountant::with_builtin_pricing();
        let out = run_turn(
            &router,
            &transports,
            &acc,
            &ModelSelector::default_model(), // routes to the default (anthropic)
            PrivacyClass::Any,
            &user_prompt("hi"),
        )
        .unwrap();
        assert_eq!(out.provider, LOCAL_PROVIDER_KEY);
        assert!(out.fallbacks >= 1);
        // local is free.
        assert_eq!(out.cost.micro_usd, 0);
    }

    #[test]
    fn all_failing_transports_exhaust_the_chain() {
        let router = MultiProviderRouter::with_builtin_providers();
        // Every provider fails → AllProvidersFailed with the last error.
        let transports = ScriptedTransports::new()
            .fail(ANTHROPIC_PROVIDER_KEY, "a")
            .fail(OPENAI_PROVIDER_KEY, "b")
            .fail(AGGREGATOR_PROVIDER_KEY, "c")
            .fail(LOCAL_PROVIDER_KEY, "d")
            .fail(CLI_PROVIDER_KEY, "last-one");
        let acc = CostAccountant::with_builtin_pricing();
        let err = run_turn(
            &router,
            &transports,
            &acc,
            &ModelSelector::default_model(),
            PrivacyClass::Any,
            &user_prompt("hi"),
        )
        .unwrap_err();
        match err {
            AgentError::AllProvidersFailed { attempts, last } => {
                assert_eq!(attempts, 5);
                assert!(last.contains("last-one"), "{last}");
            }
            other => panic!("expected AllProvidersFailed, got {other:?}"),
        }
    }

    #[test]
    fn hot_swap_same_node_two_providers() {
        // The DoD hot-swap, end to end: the same node resolved to a cloud model,
        // then to a local one, each pricing correctly through the loop.
        let router = MultiProviderRouter::with_builtin_providers();
        let transports = ScriptedTransports::new()
            .ok(OPENAI_PROVIDER_KEY, openai_reply())
            .ok(LOCAL_PROVIDER_KEY, openai_reply());
        let acc = CostAccountant::with_builtin_pricing();
        let cloud = run_turn(
            &router,
            &transports,
            &acc,
            &ModelSelector::pinned("openai/gpt-4o"),
            PrivacyClass::Any,
            &user_prompt("x"),
        )
        .unwrap();
        let local = run_turn(
            &router,
            &transports,
            &acc,
            &ModelSelector::pinned("local/llama3.1"),
            PrivacyClass::Any,
            &user_prompt("x"),
        )
        .unwrap();
        assert_eq!(cloud.provider, OPENAI_PROVIDER_KEY);
        assert!(cloud.cost.micro_usd > 0);
        assert_eq!(local.provider, LOCAL_PROVIDER_KEY);
        assert_eq!(local.cost.micro_usd, 0);
    }
}
