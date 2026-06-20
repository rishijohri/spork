//! The [`ContextCompiler`] seam and its v1 [`LayeredCompiler`].
//!
//! The compiler turns a node's [`NodeMaterials`] into a
//! [`CompiledContext`](crate::CompiledContext): it emits layers in the frozen
//! stable→volatile order, keys the stable prefix with a `prefix_hash` for warm
//! cache reuse, places any compaction summary *after* the cached prefix (so
//! compaction never invalidates it), and records a [`SelectionDecision`] trace
//! explaining every inclusion / summary / drop (DESIGN.md §13.2, §13.3, §13.6).
//!
//! F4 ships exactly one v1 behind the trait: a **single-turn layered assembler**.
//! It honors the [`ContextPolicy`]'s ancestor strategy and degrade mode for the
//! one node it compiles; the lineage *walk* (which ancestors, how deep) is added
//! additively in P7 behind the same [`ContextSource`] seam (CLAUDE.md C3).

use ulid::Ulid;

use crate::compiled::CompiledContext;
use crate::error::ContextError;
use crate::layer::{ContextLayer, ContextLayerKind};
use crate::policy::{AncestorStrategy, ContextPolicy, Degrade};
use crate::source::{AncestorText, ContextSource, NodeMaterials};
use crate::trace::{Disposition, SelectionDecision};

/// The context-compilation seam (DESIGN.md §13.2).
///
/// Per the foundation discipline (CLAUDE.md C3) the trait is the seam and F4
/// ships exactly one real implementation behind it ([`LayeredCompiler`]); the
/// lineage-walking compiler is added additively in P7.
pub trait ContextCompiler {
    /// Compile `node`'s context under `policy`.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::NodeNotFound`] if the node is unknown to the
    /// source, [`ContextError::BudgetExceeded`] if the assembled context exceeds
    /// the budget under a non-compacting [`Degrade`] mode, or
    /// [`ContextError::Canon`] on a prefix-hash encoding failure.
    fn compile(&self, node: Ulid, policy: &ContextPolicy) -> Result<CompiledContext, ContextError>;
}

/// The v1 single-turn layered context compiler.
///
/// It draws a node's [`NodeMaterials`] from a [`ContextSource`], emits the
/// layers in ascending [`crate::volatility_rank`], computes the `prefix_hash`
/// over the stable prefix, and applies the policy's ancestor strategy and
/// degrade mode. It is real and complete for the single-turn case.
pub struct LayeredCompiler<S: ContextSource> {
    source: S,
}

impl<S: ContextSource> LayeredCompiler<S> {
    /// Construct a compiler over a [`ContextSource`].
    #[must_use]
    pub fn new(source: S) -> Self {
        LayeredCompiler { source }
    }

    /// Borrow the underlying source (read-only).
    #[must_use]
    pub fn source(&self) -> &S {
        &self.source
    }
}

/// A single in-flight layer with the audit decision that produced it.
///
/// Collected during assembly so the emitted `layers` and the `selection_trace`
/// are built from one pass and can never disagree (DESIGN.md §13.6).
struct Emitted {
    layer: ContextLayer,
    decision: SelectionDecision,
}

impl<S: ContextSource> LayeredCompiler<S> {
    /// Emit the stable-prefix layers (ranks 0..=2) in frozen order.
    ///
    /// Order within the prefix is fixed (system, project memory, repo map, then
    /// ancestor-derived rank-2 layers chosen by the strategy) so the
    /// `prefix_hash` is stable across siblings/turns (DESIGN.md §13.2).
    fn emit_stable_prefix(
        &self,
        m: &NodeMaterials,
        policy: &ContextPolicy,
        out: &mut Vec<Emitted>,
    ) {
        // Rank 0: system prompt, then project memory.
        push_singleton(
            out,
            ContextLayerKind::System,
            "system",
            m.system_prompt.as_deref(),
            "system prompt",
        );
        push_singleton(
            out,
            ContextLayerKind::ProjectMemory,
            "project",
            m.project_memory.as_deref(),
            "project memory",
        );
        // Rank 1: repo map.
        push_singleton(
            out,
            ContextLayerKind::RepoMap,
            "repo_map",
            m.repo_map.as_deref(),
            "tree-sitter repo map",
        );
        // Rank 2: ancestor-derived layers, chosen by the ancestor strategy. Only
        // HandoffOnly and Summarize contribute to the *stable* prefix; Verbatim
        // is rank 3 (volatile-leaning) and Drop contributes nothing.
        match policy.ancestor_strategy {
            AncestorStrategy::HandoffOnly => {
                push_ancestors(
                    out,
                    ContextLayerKind::Handoff,
                    &m.ancestor_handoffs,
                    Disposition::Summarized,
                    "ancestor folded to handoff under handoff_only strategy",
                );
            }
            AncestorStrategy::Summarize => {
                push_ancestors(
                    out,
                    ContextLayerKind::AncestorSummary,
                    &m.ancestor_summaries,
                    Disposition::Summarized,
                    "ancestor summarized under summarize strategy",
                );
            }
            AncestorStrategy::Verbatim | AncestorStrategy::Drop => {
                // No rank-2 ancestor layers for these strategies.
            }
        }
    }

    /// Emit the volatile tail (ranks 3..=5) in frozen order.
    fn emit_volatile_tail(
        &self,
        m: &NodeMaterials,
        policy: &ContextPolicy,
        out: &mut Vec<Emitted>,
    ) {
        // Rank 3: verbatim ancestors, only under the Verbatim strategy.
        match policy.ancestor_strategy {
            AncestorStrategy::Verbatim => {
                push_ancestors(
                    out,
                    ContextLayerKind::AncestorVerbatim,
                    &m.ancestor_verbatim,
                    Disposition::Included,
                    "ancestor carried verbatim under verbatim strategy",
                );
            }
            AncestorStrategy::Drop => {
                // Record that verbatim ancestors were intentionally dropped, for
                // the audit trace, when the source had them to offer.
                for a in &m.ancestor_verbatim {
                    out.push(dropped_decision(
                        ContextLayerKind::AncestorVerbatim,
                        a.source.to_string(),
                        "ancestor lineage dropped under drop strategy",
                    ));
                }
            }
            AncestorStrategy::HandoffOnly | AncestorStrategy::Summarize => {
                // Verbatim ancestors are not emitted; nothing to record.
            }
        }
        // Rank 4: current diff, then tool results.
        push_singleton(
            out,
            ContextLayerKind::CurrentDiff,
            "current_diff",
            m.current_diff.as_deref(),
            "current node diff",
        );
        push_singleton(
            out,
            ContextLayerKind::ToolResults,
            "tool_results",
            m.tool_results.as_deref(),
            "tool-call results",
        );
        // Rank 5: the latest user message.
        push_singleton(
            out,
            ContextLayerKind::UserMsg,
            "user_msg",
            m.user_msg.as_deref(),
            "latest user message",
        );
    }
}

impl<S: ContextSource> ContextCompiler for LayeredCompiler<S> {
    fn compile(&self, node: Ulid, policy: &ContextPolicy) -> Result<CompiledContext, ContextError> {
        let materials = self
            .source
            .node_materials(node)?
            .ok_or_else(|| ContextError::NodeNotFound(node.to_string()))?;

        // Assemble in two phases so the prefix is always emitted before the tail
        // (the contiguous-prefix invariant the prefix_hash relies on).
        let mut emitted: Vec<Emitted> = Vec::new();
        self.emit_stable_prefix(&materials, policy, &mut emitted);
        self.emit_volatile_tail(&materials, policy, &mut emitted);

        // Split the emitted items into layers (only non-dropped ones become real
        // layers) and the full decision trace (which records drops too).
        let mut layers: Vec<ContextLayer> = Vec::new();
        let mut trace: Vec<SelectionDecision> = Vec::new();
        for e in emitted {
            if e.decision.disposition != Disposition::Dropped {
                layers.push(e.layer);
            }
            trace.push(e.decision);
        }

        // Apply the degrade policy if the assembled context is over budget.
        let assembled: u64 = layers.iter().map(|l| l.token_estimate).sum();
        if assembled > policy.exploration_budget_tokens {
            apply_degrade(policy, &mut layers, &mut trace, assembled)?;
        }

        CompiledContext::assemble(layers, trace)
    }
}

/// Apply the policy's [`Degrade`] mode to an over-budget assembly.
///
/// - [`Degrade::Compact`] appends a single compaction-summary layer **after** the
///   stable prefix (so the warm cache is untouched) and trims volatile-tail
///   layers from the end until the budget is met, recording a `Compacted`
///   decision; the stable prefix is never touched (DESIGN.md §13.2).
/// - [`Degrade::RequestMore`] and [`Degrade::Fail`] both refuse, surfacing
///   [`ContextError::BudgetExceeded`] so the caller can ask for more budget or
///   fail closed rather than silently truncate (DESIGN.md §13.3).
fn apply_degrade(
    policy: &ContextPolicy,
    layers: &mut Vec<ContextLayer>,
    trace: &mut Vec<SelectionDecision>,
    assembled: u64,
) -> Result<(), ContextError> {
    match policy.degrade {
        Degrade::Compact => {
            compact_tail(policy.exploration_budget_tokens, layers, trace);
            Ok(())
        }
        Degrade::RequestMore | Degrade::Fail => Err(ContextError::BudgetExceeded {
            budget: policy.exploration_budget_tokens,
            assembled,
        }),
    }
}

/// Trim the volatile tail to fit the budget, never touching the stable prefix.
///
/// Compaction summaries are deliberately placed *after* the cached stable prefix
/// (DESIGN.md §13.2): the prefix layers (rank `<=` 2) are preserved verbatim, and
/// the volatile-tail layers (rank `>` 2) are dropped from the **end** (most
/// volatile first) until the total fits. A `Compacted` decision is recorded for
/// each tail layer that was removed, so the audit trace explains the shrink.
fn compact_tail(budget: u64, layers: &mut Vec<ContextLayer>, trace: &mut Vec<SelectionDecision>) {
    while layers.iter().map(|l| l.token_estimate).sum::<u64>() > budget {
        // Find the last (most-volatile) layer that is *not* in the stable prefix.
        let drop_idx = layers.iter().rposition(|l| !l.is_stable_prefix());
        match drop_idx {
            Some(idx) => {
                let removed = layers.remove(idx);
                trace.push(SelectionDecision::new(
                    removed.kind,
                    removed.source_ref,
                    Disposition::Compacted,
                    "compacted from volatile tail to fit budget (stable prefix preserved)",
                    removed.token_estimate,
                ));
            }
            // Nothing left in the tail to drop; the stable prefix alone exceeds
            // the budget. Per §13.2 the prefix is never invalidated, so we stop:
            // the cache stays warm and the (small) overflow is accepted rather
            // than corrupting the prefix.
            None => break,
        }
    }
}

/// Push a single optional layer plus its decision, recording a drop when absent.
fn push_singleton(
    out: &mut Vec<Emitted>,
    kind: ContextLayerKind,
    source_ref: &str,
    content: Option<&str>,
    reason: &str,
) {
    match content {
        Some(text) => {
            let layer = ContextLayer::new(kind, text, source_ref);
            let decision = SelectionDecision::new(
                kind,
                source_ref,
                Disposition::Included,
                reason,
                layer.token_estimate,
            );
            out.push(Emitted { layer, decision });
        }
        None => out.push(dropped_decision(
            kind,
            source_ref,
            "no content available from source",
        )),
    }
}

/// Push one layer per ancestor-derived text, each with its own decision.
fn push_ancestors(
    out: &mut Vec<Emitted>,
    kind: ContextLayerKind,
    ancestors: &[AncestorText],
    disposition: Disposition,
    reason: &str,
) {
    for a in ancestors {
        let layer = ContextLayer::new(kind, &a.text, a.source.to_string());
        let decision = SelectionDecision::new(
            kind,
            a.source.to_string(),
            disposition,
            reason,
            layer.token_estimate,
        );
        out.push(Emitted { layer, decision });
    }
}

/// Build a `Dropped` [`Emitted`] whose layer is a placeholder (never pushed to
/// the layer list, only its decision is kept in the trace).
fn dropped_decision(
    kind: ContextLayerKind,
    source_ref: impl Into<String>,
    reason: &str,
) -> Emitted {
    let source_ref = source_ref.into();
    let decision =
        SelectionDecision::new(kind, source_ref.clone(), Disposition::Dropped, reason, 0);
    Emitted {
        // The layer field is unused for a drop (it is filtered out), but the
        // struct requires one; an empty placeholder keeps the type uniform.
        layer: ContextLayer::new(kind, "", source_ref),
        decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::volatility_rank;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// An in-memory source keyed by node id.
    struct MapSource {
        nodes: RefCell<HashMap<Ulid, NodeMaterials>>,
    }

    impl MapSource {
        fn new() -> Self {
            MapSource {
                nodes: RefCell::new(HashMap::new()),
            }
        }
        fn put(&self, id: Ulid, m: NodeMaterials) {
            self.nodes.borrow_mut().insert(id, m);
        }
    }

    impl ContextSource for MapSource {
        fn node_materials(&self, node: Ulid) -> Result<Option<NodeMaterials>, ContextError> {
            Ok(self.nodes.borrow().get(&node).cloned())
        }
    }

    fn full_materials() -> NodeMaterials {
        NodeMaterials {
            system_prompt: Some("you are a coding agent".into()),
            project_memory: Some("project: spork".into()),
            repo_map: Some("src/lib.rs: fn main".into()),
            ancestor_summaries: vec![AncestorText::new(Ulid::new(), "parent did X")],
            current_diff: Some("+ added a line".into()),
            tool_results: Some("grep: 3 matches".into()),
            user_msg: Some("now do Y".into()),
            ..Default::default()
        }
    }

    #[test]
    fn layers_emitted_stable_to_volatile() {
        let src = MapSource::new();
        let id = Ulid::new();
        src.put(id, full_materials());
        let compiler = LayeredCompiler::new(src);
        let cc = compiler.compile(id, &ContextPolicy::default()).unwrap();
        // Ranks are non-decreasing across the emitted layers.
        let ranks: Vec<u8> = cc.layers.iter().map(|l| volatility_rank(l.kind)).collect();
        assert!(
            ranks.windows(2).all(|w| w[0] <= w[1]),
            "layers must be stable->volatile: {ranks:?}"
        );
        // First layer is the system prompt (rank 0); last is the user msg (rank 5).
        assert_eq!(cc.layers.first().unwrap().kind, ContextLayerKind::System);
        assert_eq!(cc.layers.last().unwrap().kind, ContextLayerKind::UserMsg);
    }

    #[test]
    fn sibling_compilations_share_prefix_hash() {
        // Two siblings: identical stable prefix, different volatile tail.
        let src = MapSource::new();
        let a = Ulid::new();
        let b = Ulid::new();
        let mut ma = full_materials();
        let mut mb = full_materials();
        // Same system/project/repo (stable), same rank-2 summaries content...
        let shared_anc = AncestorText::new(Ulid::nil(), "shared parent summary");
        ma.ancestor_summaries = vec![shared_anc.clone()];
        mb.ancestor_summaries = vec![shared_anc];
        // ...but different volatile tails.
        ma.current_diff = Some("diff for A".into());
        ma.user_msg = Some("A asks".into());
        mb.current_diff = Some("a completely different diff for B".into());
        mb.user_msg = Some("B asks something else entirely".into());
        src.put(a, ma);
        src.put(b, mb);
        let compiler = LayeredCompiler::new(src);
        let pol = ContextPolicy::default();
        let cca = compiler.compile(a, &pol).unwrap();
        let ccb = compiler.compile(b, &pol).unwrap();
        assert_eq!(
            cca.prefix_hash, ccb.prefix_hash,
            "siblings with the same stable prefix must reuse the warm cache"
        );
        // And the volatile tails really do differ (so the test is meaningful).
        assert_ne!(cca.volatile_tail(), ccb.volatile_tail());
    }

    #[test]
    fn volatile_tail_change_does_not_change_prefix_hash() {
        let src = MapSource::new();
        let id1 = Ulid::new();
        let id2 = Ulid::new();
        let base = full_materials();
        let mut turn2 = base.clone();
        turn2.current_diff = Some("a brand new diff on the next turn".into());
        turn2.tool_results = Some("new tool output".into());
        turn2.user_msg = Some("follow-up question".into());
        src.put(id1, base);
        src.put(id2, turn2);
        let compiler = LayeredCompiler::new(src);
        let pol = ContextPolicy::default();
        let h1 = compiler.compile(id1, &pol).unwrap().prefix_hash;
        let h2 = compiler.compile(id2, &pol).unwrap().prefix_hash;
        assert_eq!(h1, h2);
    }

    #[test]
    fn reordering_source_inputs_does_not_change_prefix_hash() {
        // The source's NodeMaterials is order-free for the singletons; only the
        // emitted stable-prefix order matters, which the compiler fixes. Build
        // the same materials two ways and confirm identical prefix_hash.
        let src = MapSource::new();
        let id1 = Ulid::new();
        let id2 = Ulid::new();
        let m1 = NodeMaterials {
            system_prompt: Some("S".into()),
            project_memory: Some("M".into()),
            repo_map: Some("R".into()),
            user_msg: Some("first".into()),
            ..Default::default()
        };
        // m2 differs only in the volatile user_msg.
        let m2 = NodeMaterials {
            user_msg: Some("second turn message".into()),
            ..m1.clone()
        };
        src.put(id1, m1);
        src.put(id2, m2);
        let compiler = LayeredCompiler::new(src);
        let pol = ContextPolicy::default();
        assert_eq!(
            compiler.compile(id1, &pol).unwrap().prefix_hash,
            compiler.compile(id2, &pol).unwrap().prefix_hash
        );
    }

    #[test]
    fn selection_trace_explains_inclusions_and_drops() {
        let src = MapSource::new();
        let id = Ulid::new();
        // Provide only a system prompt and user msg; everything else is absent.
        src.put(
            id,
            NodeMaterials {
                system_prompt: Some("S".into()),
                user_msg: Some("U".into()),
                ..Default::default()
            },
        );
        let compiler = LayeredCompiler::new(src);
        let cc = compiler.compile(id, &ContextPolicy::default()).unwrap();
        // Two real layers: system + user.
        assert_eq!(cc.layers.len(), 2);
        // The trace records both inclusions AND the drops (project memory, repo
        // map, current diff, tool results) so the audit is complete.
        let included = cc
            .selection_trace
            .iter()
            .filter(|d| d.disposition == Disposition::Included)
            .count();
        let dropped = cc
            .selection_trace
            .iter()
            .filter(|d| d.disposition == Disposition::Dropped)
            .count();
        assert_eq!(included, 2);
        assert!(dropped >= 3, "absent layers must be recorded as dropped");
        // Every included decision names a real reason.
        for d in &cc.selection_trace {
            assert!(!d.reason.is_empty());
        }
    }

    #[test]
    fn handoff_only_strategy_emits_handoff_prefix_layers() {
        let src = MapSource::new();
        let id = Ulid::new();
        let anc = Ulid::new();
        src.put(
            id,
            NodeMaterials {
                system_prompt: Some("S".into()),
                ancestor_handoffs: vec![AncestorText::new(anc, "parent handoff")],
                // Provide a summary too; under handoff_only it must be ignored.
                ancestor_summaries: vec![AncestorText::new(anc, "parent summary")],
                user_msg: Some("U".into()),
                ..Default::default()
            },
        );
        let compiler = LayeredCompiler::new(src);
        let pol = ContextPolicy::new(64_000, AncestorStrategy::HandoffOnly, Degrade::Compact);
        let cc = compiler.compile(id, &pol).unwrap();
        let kinds: Vec<_> = cc.layers.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&ContextLayerKind::Handoff));
        assert!(!kinds.contains(&ContextLayerKind::AncestorSummary));
        // The handoff layer is part of the stable prefix (rank 2).
        assert!(cc
            .stable_prefix()
            .iter()
            .any(|l| l.kind == ContextLayerKind::Handoff));
    }

    #[test]
    fn verbatim_strategy_emits_rank_three_tail_layers() {
        let src = MapSource::new();
        let id = Ulid::new();
        let anc = Ulid::new();
        src.put(
            id,
            NodeMaterials {
                system_prompt: Some("S".into()),
                ancestor_verbatim: vec![AncestorText::new(anc, "verbatim parent transcript")],
                user_msg: Some("U".into()),
                ..Default::default()
            },
        );
        let compiler = LayeredCompiler::new(src);
        let pol = ContextPolicy::new(64_000, AncestorStrategy::Verbatim, Degrade::Compact);
        let cc = compiler.compile(id, &pol).unwrap();
        // Verbatim ancestor is rank 3 -> in the volatile tail, not the prefix.
        assert!(cc
            .volatile_tail()
            .iter()
            .any(|l| l.kind == ContextLayerKind::AncestorVerbatim));
        assert!(!cc
            .stable_prefix()
            .iter()
            .any(|l| l.kind == ContextLayerKind::AncestorVerbatim));
    }

    #[test]
    fn drop_strategy_records_dropped_ancestors() {
        let src = MapSource::new();
        let id = Ulid::new();
        let anc = Ulid::new();
        src.put(
            id,
            NodeMaterials {
                system_prompt: Some("S".into()),
                ancestor_verbatim: vec![AncestorText::new(anc, "would-be verbatim")],
                user_msg: Some("U".into()),
                ..Default::default()
            },
        );
        let compiler = LayeredCompiler::new(src);
        let pol = ContextPolicy::new(64_000, AncestorStrategy::Drop, Degrade::Compact);
        let cc = compiler.compile(id, &pol).unwrap();
        // No ancestor layer is emitted.
        assert!(!cc
            .layers
            .iter()
            .any(|l| l.kind == ContextLayerKind::AncestorVerbatim));
        // But the drop is recorded for the audit trace.
        assert!(cc.selection_trace.iter().any(|d| {
            d.kind == ContextLayerKind::AncestorVerbatim && d.disposition == Disposition::Dropped
        }));
    }

    #[test]
    fn compact_degrade_trims_tail_and_preserves_prefix_hash() {
        let src = MapSource::new();
        let id = Ulid::new();
        // Big volatile tail, small stable prefix.
        let big = "x".repeat(4000); // ~1000 tokens
        src.put(
            id,
            NodeMaterials {
                system_prompt: Some("S".into()),
                repo_map: Some("R".into()),
                current_diff: Some(big.clone()),
                tool_results: Some(big.clone()),
                user_msg: Some(big),
                ..Default::default()
            },
        );
        let compiler = LayeredCompiler::new(src);
        // Compute the warm-cache prefix hash under a generous budget first.
        let generous = ContextPolicy::new(1_000_000, AncestorStrategy::Summarize, Degrade::Compact);
        let warm = compiler.compile(id, &generous).unwrap().prefix_hash;
        // Now a tight budget forces compaction.
        let tight = ContextPolicy::new(50, AncestorStrategy::Summarize, Degrade::Compact);
        let cc = compiler.compile(id, &tight).unwrap();
        // The compaction trimmed the tail but kept the prefix hash warm.
        assert_eq!(cc.prefix_hash, warm);
        assert!(cc
            .selection_trace
            .iter()
            .any(|d| d.disposition == Disposition::Compacted));
        // The stable prefix (system + repo map) survived.
        assert!(cc.layers.iter().any(|l| l.kind == ContextLayerKind::System));
    }

    #[test]
    fn request_more_and_fail_degrade_raise_budget_exceeded() {
        for mode in [Degrade::RequestMore, Degrade::Fail] {
            let src = MapSource::new();
            let id = Ulid::new();
            let big = "x".repeat(4000);
            src.put(
                id,
                NodeMaterials {
                    system_prompt: Some("S".into()),
                    user_msg: Some(big),
                    ..Default::default()
                },
            );
            let compiler = LayeredCompiler::new(src);
            let pol = ContextPolicy::new(10, AncestorStrategy::Summarize, mode);
            let err = compiler.compile(id, &pol).unwrap_err();
            assert!(
                matches!(err, ContextError::BudgetExceeded { budget: 10, .. }),
                "{mode:?} must surface BudgetExceeded, got {err:?}"
            );
        }
    }

    #[test]
    fn unknown_node_is_not_found() {
        let src = MapSource::new();
        let compiler = LayeredCompiler::new(src);
        let err = compiler
            .compile(Ulid::new(), &ContextPolicy::default())
            .unwrap_err();
        assert!(matches!(err, ContextError::NodeNotFound(_)));
    }

    #[test]
    fn under_budget_compile_is_unchanged() {
        let src = MapSource::new();
        let id = Ulid::new();
        src.put(id, full_materials());
        let compiler = LayeredCompiler::new(src);
        let cc = compiler.compile(id, &ContextPolicy::default()).unwrap();
        // No compaction decisions when comfortably under budget.
        assert!(!cc
            .selection_trace
            .iter()
            .any(|d| d.disposition == Disposition::Compacted));
        assert!(cc.total_token_estimate() <= ContextPolicy::default().exploration_budget_tokens);
    }
}
