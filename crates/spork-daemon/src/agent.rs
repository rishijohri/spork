//! P6 dispatch: `node.agentRun` — invoke a model for a node and **attach** the
//! answer (DESIGN.md §6.6, §12.1, §12.3, §12.5).
//!
//! This is the additive P6 wiring behind the frozen F3 seams (CLAUDE.md C2/C3):
//! it adds one dispatch arm, one built-in **context** node type, and the
//! daemon-side transport configuration — without changing any existing command,
//! event, or contract.
//!
//! # The read-only agent run (auto-*attach*, never fork)
//!
//! A P6 agent run is **read-only** (ask / plan / analysis): it resolves the
//! node's [`ModelSelector`] through the [`MultiProviderRouter`] (enforcing the
//! node's [`PrivacyClass`] before any byte leaves), invokes the model over the
//! configured [`Transport`](spork_transport::Transport), prices the turn through
//! the [`CostAccountant`], and **attaches** the answer as an observing
//! [`Family::Context`] node linked to the target by a *dotted*
//! [`DerivedFrom`](spork_graph::EdgeType::DerivedFrom) edge. The target snapshot
//! is never mutated and **no branch is forked** — a read-only run attaches; only
//! a *code-changing* run forks-on-divergence, and that needs the tiered executors
//! (P8), so it is out of scope here (DESIGN.md §6.6). The attached node records
//! the `model` and `cost` in its payload, which the projection lifts into the
//! materialized envelope (per-node attribution; the per-branch ledger is the sum
//! over a branch's nodes).
//!
//! # Capability gating (deny-by-default)
//!
//! Model invocation reaches the network / spends money, so it stays **denied by
//! default** (DESIGN.md §15.1): `node.agentRun` authorizes
//! [`Capability::ModelInvoke`] (and [`Capability::NetConnect`] for a local HTTP
//! server, [`Capability::ProcessSpawn`] for a CLI agent) before running. A daemon
//! grants model access opt-in via
//! [`DaemonBuilder::grant_model_access`](crate::DaemonBuilder::grant_model_access);
//! without it the run is refused, exactly like any other ungranted capability.

use semver::Version;
use serde::{Deserialize, Serialize};
use spork_agent::{run_turn, user_prompt, TransportResolver};
use spork_broker::{Capability, RequestedScope};
use spork_cost::CostAccountant;
use spork_graph::EdgeType;
use spork_ipc::{AgentRunIntent, CommandResult, OpLogEvent};
use spork_provider::{
    CanonicalTranscript, ContentBlock, ModelSelector, MultiProviderRouter, PrivacyClass,
    ProviderError, ResolvedModel, Role, CLI_PROVIDER_KEY, LOCAL_PROVIDER_KEY,
};
use spork_registry::{Family, NodeTypeDescriptor, StalenessRule};
use spork_transport::{HttpTransport, SubprocessTransport, Transport};
use ulid::Ulid;

use crate::core::Daemon;
use crate::error::DaemonError;

/// The built-in node kind an agent run attaches its answer as (a context node).
pub const AGENT_CONTEXT_KIND: &str = "agent-context";

/// The semver the daemon stamps on the built-in agent-context node it creates.
const AGENT_CONTEXT_VERSION: &str = "1.0.0";

/// A conservative per-run model budget the dispatch *requests* from the broker
/// (the granted budget is the actual ceiling). One run never asks for more than
/// this much, so a grant that bounds spend can refuse a run that would exceed it.
const PER_RUN_TOKEN_CAP: u64 = 1_000_000;
/// The per-run USD cap requested, in micro-USD ($10).
const PER_RUN_USD_MICROS: u64 = 10_000_000;

/// The built-in **context** node type an agent run attaches (DESIGN.md §6.6,
/// §6.2). A [`Family::Context`] node that owns **no** snapshot and links to the
/// node it describes by a dotted [`DerivedFrom`](EdgeType::DerivedFrom) edge.
///
/// Registered through the *public* registry path the P5 built-ins (and a P8
/// plugin) use — no built-in-only side door (DESIGN.md §7.1, §9).
pub(crate) fn agent_context_descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: AGENT_CONTEXT_KIND.to_string(),
        type_version: Version::new(1, 0, 0),
        family: Family::Context,
        owns_snapshot: false,
        payload_schema: serde_json::json!({ "type": "object" }),
        result_schema: None,
        // A context node derives from the node it describes (dotted), and may sit
        // as a child in a context chain (PARENT_CHILD) for a multi-turn thread.
        allowed_edges: vec![EdgeType::DerivedFrom, EdgeType::ParentChild],
        ports: vec![],
        staleness_rule: StalenessRule::Never,
        capabilities_required: vec!["model.invoke".to_string()],
        ui_contributions: serde_json::json!({
            "color": "#a78bfa", "icon": "agent", "displayName": "Agent"
        }),
        revoked_provenance: None,
    }
}

/// Which transports the daemon offers, per provider (DESIGN.md §12.1).
///
/// In this slice the daemon configures the **local** OpenAI-compatible server
/// (plaintext HTTP) and a **CLI** agent (subprocess). First-party cloud providers
/// need the TLS transport, which is a documented additive impl behind the
/// [`Transport`] seam (see `spork-transport`); they are intentionally
/// unconfigured here, so a cloud resolution falls back to a configured local
/// provider rather than reaching cloud without TLS.
///
/// `Serialize`/`Deserialize` so the desktop app can persist the user's choice
/// (a small JSON under `<project>/.spork/agent_config.json`) and reload it on the
/// next open (P7.5 MVP, W3). The endpoint URL / CLI command is provider *wiring*,
/// not a secret (the renderer holds zero secrets — DESIGN.md §15.1), so it is
/// safe to persist in plaintext.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    /// The local OpenAI-compatible endpoint (e.g.
    /// `http://127.0.0.1:11434/v1/chat/completions`), if a local server is
    /// configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_endpoint: Option<String>,
    /// The CLI agent program + args, if a CLI agent is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_command: Option<(String, Vec<String>)>,
}

impl AgentConfig {
    /// The default local Ollama/LM-Studio-style endpoint a daemon assumes when no
    /// explicit local endpoint is set.
    pub const DEFAULT_LOCAL_ENDPOINT: &'static str = "http://127.0.0.1:11434/v1/chat/completions";

    /// A config pointing at the conventional local endpoint and no CLI agent.
    #[must_use]
    pub fn with_default_local() -> Self {
        AgentConfig {
            local_endpoint: Some(Self::DEFAULT_LOCAL_ENDPOINT.to_string()),
            cli_command: None,
        }
    }

    /// A config pointing at a local OpenAI-compatible HTTP `endpoint` (e.g. an
    /// Ollama / LM-Studio server) and no CLI agent (P7.5 MVP, W3).
    #[must_use]
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        AgentConfig {
            local_endpoint: Some(endpoint.into()),
            cli_command: None,
        }
    }

    /// A config pointing at a CLI agent `program` + `args` and no local endpoint
    /// (P7.5 MVP, W3). The program must be a *conforming* agent that speaks
    /// Spork's JSONL wire protocol — **not** a real coding CLI like
    /// `claude`/`copilot` (which own their own tool loop; the generic
    /// CLI-as-model route is deprecated — REALIGNMENT_PLAN.md §2).
    #[must_use]
    pub fn with_cli_agent(program: impl Into<String>, args: Vec<String>) -> Self {
        AgentConfig {
            local_endpoint: None,
            cli_command: Some((program.into(), args)),
        }
    }

    /// The host of the configured local endpoint, for the `net.connect`
    /// authorization (`None` if no local endpoint is set). `pub(crate)` so the
    /// P7.5 edit loop (`crate::edit`) authorizes the same host.
    pub(crate) fn local_host(&self) -> Option<String> {
        let ep = self.local_endpoint.as_deref()?;
        let rest = ep
            .strip_prefix("http://")
            .or_else(|| ep.strip_prefix("https://"))?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
        if host.is_empty() {
            None
        } else {
            Some(host.to_string())
        }
    }
}

/// The daemon's [`TransportResolver`] over its [`AgentConfig`]. `pub(crate)` so
/// the P7.5 edit loop (`crate::edit`) resolves transports the same way.
pub(crate) struct DaemonTransports<'a>(pub(crate) &'a AgentConfig);

impl TransportResolver for DaemonTransports<'_> {
    fn transport_for(
        &self,
        resolved: &ResolvedModel,
    ) -> Result<Box<dyn Transport>, spork_agent::AgentError> {
        match resolved.provider.as_str() {
            LOCAL_PROVIDER_KEY => self
                .0
                .local_endpoint
                .as_ref()
                .map(|e| Box::new(HttpTransport::new(e.clone())) as Box<dyn Transport>)
                .ok_or_else(|| spork_agent::AgentError::NoTransport(resolved.provider.clone())),
            CLI_PROVIDER_KEY => self
                .0
                .cli_command
                .as_ref()
                .map(|(p, a)| {
                    Box::new(SubprocessTransport::new(p.clone(), a.clone())) as Box<dyn Transport>
                })
                .ok_or_else(|| spork_agent::AgentError::NoTransport(resolved.provider.clone())),
            // First-party cloud / aggregator: the TLS transport is deferred
            // (documented out-of-scope behind the Transport seam), so these are
            // unconfigured and the loop falls back to a configured local provider.
            other => Err(spork_agent::AgentError::NoTransport(other.to_string())),
        }
    }
}

impl Daemon {
    /// `node.agentRun`: invoke a model for a node and attach the answer as an
    /// observing context node, recording the model + cost (DESIGN.md §6.6).
    pub(crate) fn cmd_node_agent_run(
        &self,
        target_node_id: Ulid,
        prompt: &str,
        model_key: &str,
        privacy_str: &str,
        intent: AgentRunIntent,
    ) -> Result<CommandResult, DaemonError> {
        // The target must exist (we attach to it).
        let branch_id = {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            core.graph
                .get_node(target_node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .ok_or_else(|| DaemonError::NotFound(format!("node {target_node_id}")))?
                .branch_id
        };

        // Snapshot the agent config once (it may be swapped at runtime via
        // `set_agent_config`) so the long, networked turn below never holds the
        // config lock (P7.5 MVP, W3).
        let agent_config = self
            .agent_config
            .lock()
            .expect("agent config mutex poisoned")
            .clone();

        // Authorize model invocation (deny-by-default): the model spend itself,
        // plus the local server's network host if one is configured. A CLI agent
        // additionally spawns a process (covered by the default `process.spawn`).
        self.authorize(
            Capability::ModelInvoke,
            &RequestedScope::model(PER_RUN_TOKEN_CAP, PER_RUN_USD_MICROS),
        )?;
        if let Some(host) = agent_config.local_host() {
            self.authorize(Capability::NetConnect, &RequestedScope::host(host))?;
        }
        if agent_config.cli_command.is_some() {
            self.authorize(
                Capability::ProcessSpawn,
                &RequestedScope::path(crate::core::WORKTREE_GLOB),
            )?;
        }

        let privacy = parse_privacy(privacy_str)?;
        let selector = if model_key.is_empty() {
            ModelSelector::default_model()
        } else {
            ModelSelector::pinned(model_key)
        };
        let input = user_prompt(prompt);

        // Run the turn end to end (resolve → render → carry → parse → price),
        // with the router's fallback chain engaged on transport failure. A privacy
        // violation surfaces here as a hard error (nothing left the machine).
        let transports = DaemonTransports(&agent_config);
        let result = run_turn(
            &self.router,
            &transports,
            &self.accountant,
            &selector,
            privacy,
            &input,
        )
        .map_err(|e| map_agent_error_for(e, &agent_config))?;

        let answer = extract_answer_text(&result.output);
        let cost_value = serde_json::to_value(&result.cost)
            .map_err(|e| DaemonError::Agent(format!("encode cost: {e}")))?;
        // Bind the canonical transcript as a content-addressed `conversation_ref`
        // so the node's conversation is retrievable via the History MCP
        // (`get_node_transcript` reads `conversation_ref`) and the UI conversation
        // surface — the same code+conversation binding the dual-restore guard
        // understands (P7.5 MVP, W5; DESIGN.md §13.4, §13.7, §10.3). Without this
        // the answer was stored inline only and `get_node_transcript` returned
        // `None`.
        let conversation_ref = self.store_transcript(&result.output)?;
        // The full provider/model key, for display + the cost ledger.
        let model_label = format!("{}/{}", result.provider, result.model_key);
        let intent_str = intent_label(intent);
        let payload = serde_json::json!({
            "schema_version": 1,
            "target_node_id": target_node_id.to_string(),
            "intent": intent_str,
            "prompt": prompt,
            "model": model_label,
            "provider": result.provider,
            "answer": answer,
            "conversation_ref": conversation_ref.to_hex(),
            "cost": cost_value,
        });

        let node_id = self.attach_context_node(target_node_id, &branch_id, payload)?;

        Ok(self.record_mutation(serde_json::json!({
            "nodeId": node_id.to_string(),
            "provider": result.provider,
            "model": model_label,
            "costMicroUsd": result.cost.micro_usd,
            "inputTokens": result.cost.input_tokens,
            "outputTokens": result.cost.output_tokens,
            "fallbacks": result.fallbacks,
            "intent": intent_str,
        })))
    }

    /// Create the attached context node (no snapshot, no lineage parent) linked to
    /// `target` by a dotted [`DerivedFrom`](EdgeType::DerivedFrom) edge, and
    /// publish the node + edge. A read-only attach: **no** solid `PARENT_CHILD`
    /// lineage edge and **no** branch fork (DESIGN.md §6.6, §6.3).
    fn attach_context_node(
        &self,
        target: Ulid,
        branch_id: &str,
        payload: serde_json::Value,
    ) -> Result<Ulid, DaemonError> {
        let version = Version::parse(AGENT_CONTEXT_VERSION)
            .map_err(|e| DaemonError::Graph(format!("bad agent-context version: {e}")))?;

        let (node_id, schema_version) = {
            let mut core = self.core.lock().expect("daemon core mutex poisoned");
            let env = core
                .graph
                .create_node(
                    AGENT_CONTEXT_KIND,
                    Some(&version),
                    Vec::new(), // attach-only: no lineage parent (dotted edge only)
                    branch_id,
                    payload,
                    false, // a context node owns no snapshot
                    None,
                )
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            let node_id = env.id;
            // The dotted attachment edge: context -> target (DERIVED_FROM). The
            // agent-context descriptor declares DERIVED_FROM in `allowed_edges`.
            core.graph
                .add_edge(node_id, target, EdgeType::DerivedFrom)
                .map_err(|e| DaemonError::Graph(e.to_string()))?;
            (node_id, env.payload_schema_version)
        };

        self.publish_event(|seq| OpLogEvent::NodeCreated {
            seq,
            node_id,
            schema_version,
        })?;
        self.publish_event(|seq| OpLogEvent::EdgeAdded {
            seq,
            from: node_id,
            to: target,
            edge: EdgeType::DerivedFrom,
        })?;
        Ok(node_id)
    }

    /// Replace the daemon's agent transport configuration at runtime (P7.5 MVP,
    /// W3): which provider an agent run reaches — a local OpenAI-compatible HTTP
    /// endpoint (the default route) and/or a conforming CLI agent. Takes effect on the next
    /// run with no project reopen. The config is provider *wiring*, not a secret
    /// (a URL / CLI command is config; the renderer holds zero secrets —
    /// DESIGN.md §15.1), so the desktop app may persist it in plaintext.
    pub fn set_agent_config(&self, config: AgentConfig) {
        *self
            .agent_config
            .lock()
            .expect("agent config mutex poisoned") = config;
    }

    /// Serialize a [`CanonicalTranscript`] to JSON and store it in the CAS,
    /// returning the `conversation_ref` content hash the node payload binds
    /// (DESIGN.md §13.4, §10.3). The stored JSON is what the History MCP's
    /// `get_node_transcript` returns and the UI conversation surface renders.
    fn store_transcript(
        &self,
        transcript: &CanonicalTranscript,
    ) -> Result<spork_hash::Hash, DaemonError> {
        let bytes = serde_json::to_vec(transcript)
            .map_err(|e| DaemonError::Agent(format!("encode transcript: {e}")))?;
        self.put_conversation(&bytes)
    }
}

/// Map a [`spork_agent::AgentError`] into the daemon's taxonomy, preserving a
/// privacy violation as a distinct, surfaced refusal (DESIGN.md §12.5).
pub(crate) fn map_agent_error(err: spork_agent::AgentError) -> DaemonError {
    match err {
        spork_agent::AgentError::Provider(ProviderError::PrivacyViolation {
            requested,
            provider,
        }) => DaemonError::Agent(format!(
            "privacy class {requested} forbids resolving to provider '{provider}'"
        )),
        other => DaemonError::Agent(other.to_string()),
    }
}

/// The actionable guidance surfaced when an agent run fails while the daemon is
/// configured to drive a **CLI agent as a model** — the deprecated
/// generic-CLI-as-model route (REALIGNMENT_PLAN.md §2). Replaces the cryptic
/// `... exited with status 1` a real coding CLI (`claude`/`copilot`) emits when
/// fed Spork's private JSONL it cannot parse, pointing the user at the two real
/// connectivity arrows instead.
pub(crate) const CLI_AS_MODEL_GUIDANCE: &str =
    "This looks like an agentic CLI, not a model endpoint. A coding-agent CLI \
     (claude/copilot/cursor) owns its own tool loop and does not speak Spork's \
     model wire protocol. Either connect it via the Orchestration MCP so your \
     agent drives Spork, or point Spork at a local OpenAI-compatible HTTP \
     endpoint (e.g. Ollama/LM Studio) so Spork drives a model.";

/// [`map_agent_error`], but **fails loud with guidance** when the configured
/// route is a CLI agent driven as a model and the failure is anything other than
/// a (self-explanatory) privacy refusal. This is the daemon-side realization of
/// the deprecated `cli` arm "failing loud" (REALIGNMENT_PLAN.md §2, R1): a
/// *conforming* program still succeeds and never reaches here; only a misuse
/// (wiring a real coding CLI as a model) hits it and gets steered to the two
/// real connectivity arrows. Purely additive — the frozen `Transport`/
/// `ProviderAdapter`/`CLI_PROVIDER_KEY` seams are untouched.
pub(crate) fn map_agent_error_for(
    err: spork_agent::AgentError,
    config: &AgentConfig,
) -> DaemonError {
    let cli_as_model = config.cli_command.is_some() && config.local_endpoint.is_none();
    let is_privacy = matches!(
        &err,
        spork_agent::AgentError::Provider(ProviderError::PrivacyViolation { .. })
    );
    let base = map_agent_error(err);
    if cli_as_model && !is_privacy {
        if let DaemonError::Agent(msg) = base {
            return DaemonError::Agent(format!("{msg}\n\n{CLI_AS_MODEL_GUIDANCE}"));
        }
    }
    base
}

/// Parse a node's privacy class from its `snake_case` token (the IPC string).
///
/// Empty/`"any"` → [`PrivacyClass::Any`]; an unrecognized non-empty token is
/// **refused** (fail-closed) rather than silently treated as permissive.
pub(crate) fn parse_privacy(s: &str) -> Result<PrivacyClass, DaemonError> {
    match s {
        "" | "any" => Ok(PrivacyClass::Any),
        "local_only" => Ok(PrivacyClass::LocalOnly),
        "no_third_party_aggregator" => Ok(PrivacyClass::NoThirdPartyAggregator),
        other => Err(DaemonError::Agent(format!(
            "unknown privacy class {other:?}"
        ))),
    }
}

/// The `snake_case` label for an [`AgentRunIntent`], via its serde rename.
///
/// Derived from the enum's own serialization so a future (`#[non_exhaustive]`)
/// intent variant carries its own correct label with no match to update; falls
/// back to `"ask"` only if serialization somehow yields no string.
fn intent_label(intent: AgentRunIntent) -> String {
    serde_json::to_value(intent)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "ask".to_string())
}

/// Flatten the assistant turns' text into one answer string (the model's reply).
fn extract_answer_text(transcript: &CanonicalTranscript) -> String {
    let mut out = String::new();
    for turn in &transcript.turns {
        if turn.role != Role::Assistant {
            continue;
        }
        for block in &turn.content {
            if let ContentBlock::Text(text) = block {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

/// A new [`MultiProviderRouter`] with the built-in providers, for the daemon.
pub(crate) fn default_router() -> MultiProviderRouter {
    MultiProviderRouter::with_builtin_providers()
}

/// A new [`CostAccountant`] with built-in pricing, for the daemon.
pub(crate) fn default_accountant() -> CostAccountant {
    CostAccountant::with_builtin_pricing()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_privacy_known_and_unknown() {
        assert_eq!(parse_privacy("").unwrap(), PrivacyClass::Any);
        assert_eq!(parse_privacy("any").unwrap(), PrivacyClass::Any);
        assert_eq!(
            parse_privacy("local_only").unwrap(),
            PrivacyClass::LocalOnly
        );
        assert_eq!(
            parse_privacy("no_third_party_aggregator").unwrap(),
            PrivacyClass::NoThirdPartyAggregator
        );
        assert!(parse_privacy("garbage").is_err());
    }

    #[test]
    fn local_host_extracts_from_endpoint() {
        let cfg = AgentConfig {
            local_endpoint: Some("http://127.0.0.1:11434/v1/chat/completions".into()),
            cli_command: None,
        };
        assert_eq!(cfg.local_host().as_deref(), Some("127.0.0.1"));
        let none = AgentConfig::default();
        assert_eq!(none.local_host(), None);
    }

    #[test]
    fn agent_context_descriptor_is_context_family_with_dotted_edge() {
        let d = agent_context_descriptor();
        assert_eq!(d.id, AGENT_CONTEXT_KIND);
        assert_eq!(d.family, Family::Context);
        assert!(!d.owns_snapshot);
        assert!(d.allowed_edges.contains(&EdgeType::DerivedFrom));
    }

    #[test]
    fn extract_answer_concatenates_assistant_text() {
        use spork_provider::CanonicalTurn;
        let t = CanonicalTranscript::new(vec![
            CanonicalTurn::text(Role::User, "q"),
            CanonicalTurn::text(Role::Assistant, "line one"),
            CanonicalTurn::text(Role::Assistant, "line two"),
        ]);
        assert_eq!(extract_answer_text(&t), "line one\nline two");
    }
}
