//! The per-node [`ContextPolicy`]: budget, ancestor strategy, and degrade mode.
//!
//! Each node type carries a `ContextPolicy` (DESIGN.md §13.3): Edit nodes get a
//! large hybrid budget; Validation / Stress get managed, `handoff_only`
//! ancestors; deterministic Sanity-Check nodes get near-zero model context. The
//! tuning hazard is sharp — too tight starves the agent (worse than RAG), too
//! loose burns tokens and rots context — so a policy **degrades gracefully**
//! (request-more vs. compact vs. fail) rather than silently truncating. The
//! policy is the single knob the compiler consults; everything else about the
//! assembled context is derived from it and the node's lineage materials.

use serde::{Deserialize, Serialize};

/// The frozen schema version of the [`ContextPolicy`] shape (CLAUDE.md C5).
///
/// The policy is a persisted, per-node-type configuration; versioning it lets
/// the budget / strategy / degrade vocabulary grow additively in a later
/// generation without reinterpreting a stored policy (DESIGN.md §13.3, A.7).
pub const CONTEXT_POLICY_SCHEMA_VERSION: u16 = 1;

/// How a node's ancestor lineage is folded into its context.
///
/// This is the policy lever §13.3 calls out: distillation is lossy, so the
/// strategy trades fidelity against token cost. F4's single-turn compiler honors
/// each strategy for the one node it compiles; P7 adds the lineage *walk* (which
/// ancestors, how deep) additively behind the same policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AncestorStrategy {
    /// Carry ancestor discussion nodes **verbatim** (highest fidelity, highest
    /// cost). Emits [`AncestorVerbatim`](crate::ContextLayerKind::AncestorVerbatim)
    /// layers.
    Verbatim,
    /// **Summarize** ancestor discussion nodes. Emits
    /// [`AncestorSummary`](crate::ContextLayerKind::AncestorSummary) layers
    /// (rank 2, cacheable).
    Summarize,
    /// Use only the distilled [`HandoffDocument`](crate::HandoffDocument) for
    /// ancestors — the cheapest defense against context rot on deep branches.
    /// Emits [`Handoff`](crate::ContextLayerKind::Handoff) layers (rank 2).
    HandoffOnly,
    /// **Drop** ancestor lineage entirely (e.g. a deterministic Sanity-Check
    /// node that needs near-zero model context).
    Drop,
}

/// How the compiler reacts when the assembled context exceeds the budget.
///
/// Graceful degradation is the §13.3 mitigation for the tuning hazard: never
/// silently truncate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Degrade {
    /// Surface an over-budget condition to the caller (so the agent can ask for
    /// more budget via the `loadAncestorContext` escape hatch) — raised as
    /// [`ContextError::BudgetExceeded`](crate::ContextError::BudgetExceeded).
    RequestMore,
    /// Apply a compaction summary, placed **after** the cached stable prefix so
    /// the warm cache is never invalidated (DESIGN.md §13.2). The prefix hash is
    /// unchanged; only the volatile tail shrinks.
    Compact,
    /// Fail closed — refuse to compile an over-budget context, as
    /// [`ContextError::BudgetExceeded`](crate::ContextError::BudgetExceeded).
    Fail,
}

/// The per-node context-compilation policy.
///
/// `exploration_budget_tokens` bounds the assembled context;
/// `ancestor_strategy` chooses how lineage is folded in; `degrade` decides what
/// happens when the budget is exceeded. All three are consulted by the v1
/// compiler (DESIGN.md §13.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPolicy {
    /// The schema version of this policy shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// The token budget the assembled context must fit within.
    pub exploration_budget_tokens: u64,
    /// How ancestor lineage is folded into the context.
    pub ancestor_strategy: AncestorStrategy,
    /// What to do when the assembled context exceeds the budget.
    pub degrade: Degrade,
}

impl ContextPolicy {
    /// Construct a policy stamped with the current schema version.
    #[must_use]
    pub fn new(
        exploration_budget_tokens: u64,
        ancestor_strategy: AncestorStrategy,
        degrade: Degrade,
    ) -> Self {
        ContextPolicy {
            schema_version: CONTEXT_POLICY_SCHEMA_VERSION,
            exploration_budget_tokens,
            ancestor_strategy,
            degrade,
        }
    }
}

impl Default for ContextPolicy {
    /// A generous, summarize-then-compact default suitable for an Edit node: a
    /// large budget, summarized ancestors, and graceful compaction (DESIGN.md
    /// §13.3, "Edit nodes get a large hybrid budget").
    fn default() -> Self {
        ContextPolicy::new(64_000, AncestorStrategy::Summarize, Degrade::Compact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(CONTEXT_POLICY_SCHEMA_VERSION, 1);
    }

    #[test]
    fn policy_round_trips_and_is_versioned() {
        let p = ContextPolicy::new(1000, AncestorStrategy::HandoffOnly, Degrade::Fail);
        assert_eq!(p.schema_version, CONTEXT_POLICY_SCHEMA_VERSION);
        let v = serde_json::to_value(&p).unwrap();
        let back: ContextPolicy = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn default_is_edit_node_shaped() {
        let p = ContextPolicy::default();
        assert_eq!(p.ancestor_strategy, AncestorStrategy::Summarize);
        assert_eq!(p.degrade, Degrade::Compact);
        assert!(p.exploration_budget_tokens > 0);
    }

    #[test]
    fn strategy_and_degrade_serialize_snake_case() {
        assert_eq!(
            serde_json::to_value(AncestorStrategy::HandoffOnly).unwrap(),
            serde_json::json!("handoff_only")
        );
        assert_eq!(
            serde_json::to_value(Degrade::RequestMore).unwrap(),
            serde_json::json!("request_more")
        );
    }

    #[test]
    fn policy_canonicalizes_integers_only() {
        let p = ContextPolicy::default();
        assert!(spork_canon::canonicalize(&p).is_ok());
    }
}
