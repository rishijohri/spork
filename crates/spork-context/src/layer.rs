//! The frozen context-layer taxonomy and its volatility ranks.
//!
//! Context is **compiled from DAG ancestry, ordered stable→volatile and
//! hash-addressed for reuse** (DESIGN.md §13.2). The order is not a heuristic:
//! each [`ContextLayerKind`] has a **frozen** [`volatility_rank`], and the
//! compiler emits layers in ascending rank so the *stable prefix* (ranks 0..=2)
//! is byte-identical across sibling branches and successive turns. That prefix
//! is keyed by a `prefix_hash` ([`crate::CompiledContext::prefix_hash`]) so the
//! provider's warm KV cache is reused — the existential cache-economics property
//! of §13.2 (cache reads bill at 0.1x input; a reordered prefix silently
//! destroys the ~80% cost reduction).
//!
//! The rank table is the one in DESIGN.md §13.2 and is frozen for this
//! generation (CLAUDE.md C5 — the ranks are part of the hashed prefix's
//! identity, so changing them is a new generation, never an in-place edit):
//!
//! | Kind | Rank |
//! |---|---|
//! | `System` / `ProjectMemory` | 0 (most stable) |
//! | `RepoMap` | 1 |
//! | `Handoff` / `AncestorSummary` | 2 |
//! | `AncestorVerbatim` | 3 |
//! | `CurrentDiff` / `ToolResults` | 4 |
//! | `UserMsg` | 5 (most volatile) |

use serde::{Deserialize, Serialize};

/// The frozen schema version of the [`ContextLayer`] shape (CLAUDE.md C5).
///
/// A [`ContextLayer`] is serialized into the canonical bytes the `prefix_hash`
/// is computed over, so its shape is part of that hash's identity. Bumping this
/// is a deliberate, generation-introducing event — never an in-place edit that
/// would silently re-key every warm cache (DESIGN.md §13.2, A.7).
pub const CONTEXT_LAYER_SCHEMA_VERSION: u16 = 1;

/// The highest volatility rank a stable, cacheable prefix layer may carry.
///
/// Layers with rank `0..=PREFIX_MAX_RANK` form the stable prefix the
/// `prefix_hash` is computed over; layers above it are the volatile tail
/// (current diff, tool results, the latest user message) that must *not*
/// influence the prefix hash, so a new turn reuses the same warm cache
/// (DESIGN.md §13.2).
pub const PREFIX_MAX_RANK: u8 = 2;

/// The kind of a single context layer, in the frozen DESIGN.md §13.2 taxonomy.
///
/// The variants are ordered here in ascending volatility for readability, but
/// the authoritative ordering is [`volatility_rank`] — never the declaration
/// order or the derived `enum` discriminant — so a future additive variant can
/// be slotted at its correct rank without disturbing the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextLayerKind {
    /// The system / developer instruction prompt. Most stable (rank 0).
    System,
    /// Project-scoped durable memory (stable facts). Rank 0.
    ProjectMemory,
    /// The tree-sitter repo map (PageRank-ranked, bounded). Rank 1.
    RepoMap,
    /// A distilled [`crate::HandoffDocument`] for an ancestor. Rank 2.
    Handoff,
    /// A summarized ancestor discussion node. Rank 2.
    AncestorSummary,
    /// An ancestor transcript carried verbatim. Rank 3 (partially cacheable).
    AncestorVerbatim,
    /// The current node's code diff. Rank 4 (not cacheable).
    CurrentDiff,
    /// Tool-call results for the current turn. Rank 4 (not cacheable).
    ToolResults,
    /// The latest user message. Most volatile (rank 5).
    UserMsg,
}

impl ContextLayerKind {
    /// Every [`ContextLayerKind`] variant, for exhaustive iteration in tests and
    /// in the compiler's stable layer ordering.
    pub const ALL: [ContextLayerKind; 9] = [
        ContextLayerKind::System,
        ContextLayerKind::ProjectMemory,
        ContextLayerKind::RepoMap,
        ContextLayerKind::Handoff,
        ContextLayerKind::AncestorSummary,
        ContextLayerKind::AncestorVerbatim,
        ContextLayerKind::CurrentDiff,
        ContextLayerKind::ToolResults,
        ContextLayerKind::UserMsg,
    ];

    /// Whether a layer of this kind belongs to the stable, cacheable prefix.
    ///
    /// True for ranks `0..=`[`PREFIX_MAX_RANK`]; these layers are folded into the
    /// `prefix_hash`. False for the volatile tail (current diff, tool results,
    /// user message), which is deliberately excluded from the prefix hash so a
    /// new turn reuses the warm cache (DESIGN.md §13.2).
    #[must_use]
    pub fn is_stable_prefix(self) -> bool {
        volatility_rank(self) <= PREFIX_MAX_RANK
    }
}

/// The **frozen** volatility rank for a [`ContextLayerKind`] (DESIGN.md §13.2).
///
/// This is a total function over every variant — a fresh additive variant must
/// be assigned a rank here, never left to fall through. The ranks are part of
/// the hashed prefix's identity (CLAUDE.md C5): the stable prefix is exactly the
/// layers whose rank is `<=`[`PREFIX_MAX_RANK`].
///
/// # Example
/// ```
/// use spork_context::{volatility_rank, ContextLayerKind};
/// // System and project memory are the most stable.
/// assert_eq!(volatility_rank(ContextLayerKind::System), 0);
/// assert_eq!(volatility_rank(ContextLayerKind::ProjectMemory), 0);
/// // The user message is the most volatile.
/// assert_eq!(volatility_rank(ContextLayerKind::UserMsg), 5);
/// // The prefix cutoff is rank 2 (handoff / ancestor summary).
/// assert!(ContextLayerKind::Handoff.is_stable_prefix());
/// assert!(!ContextLayerKind::CurrentDiff.is_stable_prefix());
/// ```
#[must_use]
pub fn volatility_rank(kind: ContextLayerKind) -> u8 {
    match kind {
        ContextLayerKind::System | ContextLayerKind::ProjectMemory => 0,
        ContextLayerKind::RepoMap => 1,
        ContextLayerKind::Handoff | ContextLayerKind::AncestorSummary => 2,
        ContextLayerKind::AncestorVerbatim => 3,
        ContextLayerKind::CurrentDiff | ContextLayerKind::ToolResults => 4,
        ContextLayerKind::UserMsg => 5,
    }
}

/// One assembled layer of compiled context.
///
/// A layer pairs its [`ContextLayerKind`] with the rendered text the model will
/// see plus a `source_ref` — the id of the node (or memory entry) the content
/// came from — so the [`SelectionDecision`](crate::SelectionDecision) trace can
/// explain *why* this content is present (DESIGN.md §13.6, the auditable
/// "expand context" affordance).
///
/// `token_estimate` is a deterministic, model-independent size estimate (not a
/// real tokenizer call, which would couple this offline crate to a provider);
/// it is what the budget check and degrade policy reason about. It is an
/// integer so the layer canonicalizes (the canonical encoder forbids floats).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextLayer {
    /// The schema version of this layer shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// What kind of layer this is — fixes its volatility rank.
    pub kind: ContextLayerKind,
    /// The rendered content the model sees for this layer.
    pub content: String,
    /// The node / memory id this content was drawn from, for the audit trace.
    pub source_ref: String,
    /// A deterministic token-size estimate for budget accounting.
    pub token_estimate: u64,
}

impl ContextLayer {
    /// Construct a layer stamped with the current schema version, computing the
    /// deterministic [`token_estimate`](ContextLayer::token_estimate) from the
    /// content.
    #[must_use]
    pub fn new(
        kind: ContextLayerKind,
        content: impl Into<String>,
        source_ref: impl Into<String>,
    ) -> Self {
        let content = content.into();
        let token_estimate = estimate_tokens(&content);
        ContextLayer {
            schema_version: CONTEXT_LAYER_SCHEMA_VERSION,
            kind,
            content,
            source_ref: source_ref.into(),
            token_estimate,
        }
    }

    /// The frozen volatility rank of this layer's kind.
    #[must_use]
    pub fn rank(&self) -> u8 {
        volatility_rank(self.kind)
    }

    /// Whether this layer belongs to the stable, cacheable prefix.
    #[must_use]
    pub fn is_stable_prefix(&self) -> bool {
        self.kind.is_stable_prefix()
    }
}

/// A deterministic, model-independent token-size estimate for `text`.
///
/// Spork's budget arithmetic must be reproducible offline and on every machine,
/// so it cannot depend on a provider's live tokenizer (a P6 concern). This uses
/// the widely-cited ~4-bytes-per-token heuristic over the UTF-8 byte length,
/// rounding up so non-empty content always costs at least one token. It is a
/// budget *estimate*, deliberately conservative and stable — not a billing
/// figure (DESIGN.md §13.3, the budget-bounded retrieval model).
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    let bytes = text.len() as u64;
    bytes.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(CONTEXT_LAYER_SCHEMA_VERSION, 1);
    }

    #[test]
    fn frozen_ranks_match_design_table() {
        assert_eq!(volatility_rank(ContextLayerKind::System), 0);
        assert_eq!(volatility_rank(ContextLayerKind::ProjectMemory), 0);
        assert_eq!(volatility_rank(ContextLayerKind::RepoMap), 1);
        assert_eq!(volatility_rank(ContextLayerKind::Handoff), 2);
        assert_eq!(volatility_rank(ContextLayerKind::AncestorSummary), 2);
        assert_eq!(volatility_rank(ContextLayerKind::AncestorVerbatim), 3);
        assert_eq!(volatility_rank(ContextLayerKind::CurrentDiff), 4);
        assert_eq!(volatility_rank(ContextLayerKind::ToolResults), 4);
        assert_eq!(volatility_rank(ContextLayerKind::UserMsg), 5);
    }

    #[test]
    fn ranks_are_monotonic_stable_to_volatile() {
        // Walking the canonical ALL order, rank is non-decreasing: the taxonomy
        // is genuinely stable->volatile, never a tangle.
        let ranks: Vec<u8> = ContextLayerKind::ALL
            .into_iter()
            .map(volatility_rank)
            .collect();
        assert!(
            ranks.windows(2).all(|w| w[0] <= w[1]),
            "ALL must list kinds in non-decreasing rank: {ranks:?}"
        );
        // And the full 0..=5 range is covered.
        assert_eq!(*ranks.iter().min().unwrap(), 0);
        assert_eq!(*ranks.iter().max().unwrap(), 5);
    }

    #[test]
    fn prefix_cutoff_is_rank_two() {
        for kind in ContextLayerKind::ALL {
            assert_eq!(
                kind.is_stable_prefix(),
                volatility_rank(kind) <= PREFIX_MAX_RANK
            );
        }
        // The stable prefix is exactly {System, ProjectMemory, RepoMap, Handoff,
        // AncestorSummary}.
        let stable: Vec<_> = ContextLayerKind::ALL
            .into_iter()
            .filter(|k| k.is_stable_prefix())
            .collect();
        assert_eq!(
            stable,
            vec![
                ContextLayerKind::System,
                ContextLayerKind::ProjectMemory,
                ContextLayerKind::RepoMap,
                ContextLayerKind::Handoff,
                ContextLayerKind::AncestorSummary,
            ]
        );
    }

    #[test]
    fn token_estimate_is_deterministic_and_ceil() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1); // 1 byte -> ceil(1/4) = 1
        assert_eq!(estimate_tokens("abcd"), 1); // 4 bytes -> 1
        assert_eq!(estimate_tokens("abcde"), 2); // 5 bytes -> 2
                                                 // Stable across calls.
        let s = "the quick brown fox jumps over the lazy dog";
        assert_eq!(estimate_tokens(s), estimate_tokens(s));
    }

    #[test]
    fn layer_serde_round_trip_and_rank() {
        let layer = ContextLayer::new(ContextLayerKind::System, "you are an agent", "node-1");
        let v = serde_json::to_value(&layer).unwrap();
        let back: ContextLayer = serde_json::from_value(v).unwrap();
        assert_eq!(layer, back);
        assert_eq!(layer.rank(), 0);
        assert!(layer.is_stable_prefix());
        assert_eq!(layer.token_estimate, estimate_tokens("you are an agent"));
    }

    #[test]
    fn layer_canonicalizes_integers_only() {
        let layer = ContextLayer::new(ContextLayerKind::UserMsg, "hello", "node-9");
        assert!(spork_canon::canonicalize(&layer).is_ok());
    }
}
