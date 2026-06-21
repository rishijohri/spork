//! The P7 lineage-walking context compiler (DESIGN.md §13.1–§13.3).
//!
//! F4 shipped the single-turn [`LayeredCompiler`](crate::LayeredCompiler) behind
//! the frozen [`ContextCompiler`](crate::ContextCompiler) trait. P7 adds the
//! **lineage walk** — *which* ancestors, *how deep* — additively behind that same
//! trait (CLAUDE.md C2/C3, PLAN §9 D-11): a [`LineageWalker`] follows a node's
//! ancestry (parent edges only, so sibling branches are never reached —
//! resolving Open Question 4, sibling context isolated), a budget-bounded
//! **ancestor selector** chooses which ancestors to fold in under the node's
//! [`ContextPolicy`], and the result is assembled by the same frozen layered
//! pipeline so the `prefix_hash` cache-reuse property is preserved unchanged.
//!
//! The compiler is purely additive: it enriches the target node's
//! [`NodeMaterials`] with selected ancestor text and delegates ordering +
//! `prefix_hash` to [`LayeredCompiler`], then augments the
//! [`SelectionDecision`](crate::SelectionDecision) trace with the
//! lineage-selection decisions (including budget/depth drops) — which never
//! affects the prefix hash (it is computed over layers, not the trace).

use ulid::Ulid;

use crate::compiled::CompiledContext;
use crate::compiler::{ContextCompiler, LayeredCompiler};
use crate::error::ContextError;
use crate::layer::{estimate_tokens, ContextLayerKind};
use crate::policy::{AncestorStrategy, ContextPolicy};
use crate::source::{AncestorText, ContextSource, NodeMaterials};
use crate::trace::{Disposition, SelectionDecision};

/// The default fraction (numerator/denominator) of the context budget reserved
/// for ancestor lineage — half, so the volatile current turn always has room.
const DEFAULT_LINEAGE_BUDGET_NUM: u64 = 1;
const DEFAULT_LINEAGE_BUDGET_DEN: u64 = 2;
/// The default maximum number of ancestors walked (depth cap against context rot).
const DEFAULT_MAX_ANCESTORS: usize = 32;

/// The lineage-walk seam: a node's ancestors, **nearest-first** (DESIGN.md §13.1).
///
/// Implementations follow only lineage (parent) edges, so a walk can never reach
/// a sibling branch — the sibling-isolation default (Open Question 4).
pub trait LineageWalker {
    /// The ancestors of `node`, nearest-first (direct parents, then their
    /// parents, …), de-duplicated.
    ///
    /// # Errors
    ///
    /// Returns a [`ContextError`] if the underlying graph cannot be read.
    fn ancestors(&self, node: Ulid) -> Result<Vec<Ulid>, ContextError>;
}

/// A [`LineageWalker`] backed by a real [`spork_graph::GraphProjection`].
///
/// It walks `parent_ids` breadth-first from the node, so the order is
/// nearest-first and it follows only structural lineage (never sibling branches).
pub struct GraphLineageWalker<'a> {
    projection: &'a spork_graph::GraphProjection,
}

impl<'a> GraphLineageWalker<'a> {
    /// Construct a walker over a graph projection.
    #[must_use]
    pub fn new(projection: &'a spork_graph::GraphProjection) -> Self {
        GraphLineageWalker { projection }
    }
}

impl LineageWalker for GraphLineageWalker<'_> {
    fn ancestors(&self, node: Ulid) -> Result<Vec<Ulid>, ContextError> {
        use std::collections::{BTreeSet, VecDeque};
        let mut out = Vec::new();
        let mut seen: BTreeSet<Ulid> = BTreeSet::new();
        seen.insert(node);
        // Nearest-first BFS over a real FIFO queue (O(1) push/pop, no O(n) shifts).
        let mut frontier: VecDeque<Ulid> = VecDeque::new();
        frontier.push_back(node);
        while let Some(current) = frontier.pop_front() {
            let env = self
                .projection
                .get_node(current)
                .map_err(|e| ContextError::NodeNotFound(format!("{current}: {e}")))?;
            let Some(env) = env else { continue };
            for parent in env.parent_ids {
                if seen.insert(parent) {
                    out.push(parent);
                    frontier.push_back(parent);
                }
            }
        }
        Ok(out)
    }
}

/// The P7 lineage-walking compiler (DESIGN.md §13.1–§13.3).
///
/// It wraps a base [`ContextSource`] (per-node raw materials) and a
/// [`LineageWalker`], walks the target's ancestry under a budget, folds the
/// selected ancestor text into the target's materials per the policy's ancestor
/// strategy, and assembles via the frozen [`LayeredCompiler`].
pub struct LineageCompiler<S: ContextSource, W: LineageWalker> {
    base: S,
    walker: W,
    lineage_budget_num: u64,
    lineage_budget_den: u64,
    max_ancestors: usize,
}

impl<S: ContextSource, W: LineageWalker> LineageCompiler<S, W> {
    /// Construct a lineage compiler over a base source and a walker, with the
    /// default lineage budget (half the context budget) and depth cap.
    #[must_use]
    pub fn new(base: S, walker: W) -> Self {
        LineageCompiler {
            base,
            walker,
            lineage_budget_num: DEFAULT_LINEAGE_BUDGET_NUM,
            lineage_budget_den: DEFAULT_LINEAGE_BUDGET_DEN,
            max_ancestors: DEFAULT_MAX_ANCESTORS,
        }
    }

    /// Override the fraction of the context budget reserved for ancestor lineage
    /// (builder style). `den` is clamped to at least 1.
    #[must_use]
    pub fn with_lineage_budget(mut self, num: u64, den: u64) -> Self {
        self.lineage_budget_num = num;
        self.lineage_budget_den = den.max(1);
        self
    }

    /// Override the maximum number of ancestors walked (builder style).
    #[must_use]
    pub fn with_max_ancestors(mut self, max: usize) -> Self {
        self.max_ancestors = max;
        self
    }

    /// Borrow the base source (read-only).
    #[must_use]
    pub fn base(&self) -> &S {
        &self.base
    }

    fn lineage_budget(&self, policy: &ContextPolicy) -> u64 {
        policy
            .exploration_budget_tokens
            .saturating_mul(self.lineage_budget_num)
            / self.lineage_budget_den
    }
}

/// The layer kind a strategy folds ancestor text into (for the audit trace).
fn ancestor_layer_kind(strategy: AncestorStrategy) -> ContextLayerKind {
    match strategy {
        AncestorStrategy::HandoffOnly => ContextLayerKind::Handoff,
        AncestorStrategy::Summarize => ContextLayerKind::AncestorSummary,
        AncestorStrategy::Verbatim => ContextLayerKind::AncestorVerbatim,
        AncestorStrategy::Drop => ContextLayerKind::AncestorVerbatim,
    }
}

/// Render an ancestor's materials into the text a strategy folds in.
///
/// Deterministic and offline (a model-driven distillation is an additive later
/// step). Returns `None` under [`AncestorStrategy::Drop`] or when the ancestor
/// has no foldable content.
fn render_ancestor(m: &NodeMaterials, strategy: AncestorStrategy) -> Option<String> {
    if strategy == AncestorStrategy::Drop {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    if let Some(u) = m.user_msg.as_deref() {
        parts.push(u);
    }
    if let Some(d) = m.current_diff.as_deref() {
        parts.push(d);
    }
    if let Some(t) = m.tool_results.as_deref() {
        parts.push(t);
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join("\n");
    match strategy {
        AncestorStrategy::Verbatim => Some(joined),
        AncestorStrategy::Summarize => Some(summarize(&joined)),
        AncestorStrategy::HandoffOnly => Some(format!("handoff: {}", summarize(&joined))),
        AncestorStrategy::Drop => None,
    }
}

/// A deterministic, lossy one-shot summary: the first line, capped at 200 bytes.
fn summarize(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.len() <= 200 {
        first_line.to_string()
    } else {
        // Cap on a char boundary so we never split a multibyte char.
        let mut end = 200;
        while !first_line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &first_line[..end])
    }
}

impl<S: ContextSource, W: LineageWalker> ContextCompiler for LineageCompiler<S, W> {
    fn compile(&self, node: Ulid, policy: &ContextPolicy) -> Result<CompiledContext, ContextError> {
        // 1. The target node's own raw materials.
        let mut materials = self
            .base
            .node_materials(node)?
            .ok_or_else(|| ContextError::NodeNotFound(node.to_string()))?;

        // 2. Walk the lineage (nearest-first; never crosses to siblings).
        let ancestors = self.walker.ancestors(node)?;
        let budget = self.lineage_budget(policy);
        let kind = ancestor_layer_kind(policy.ancestor_strategy);

        // 3. Budget-bounded ancestor selection.
        let mut selected: Vec<AncestorText> = Vec::new();
        let mut drops: Vec<SelectionDecision> = Vec::new();
        let mut used: u64 = 0;
        let mut budget_exhausted = false;
        for (depth, anc) in ancestors.iter().enumerate() {
            if depth >= self.max_ancestors {
                drops.push(SelectionDecision::new(
                    kind,
                    anc.to_string(),
                    Disposition::Dropped,
                    "ancestor dropped: depth cap reached",
                    0,
                ));
                continue;
            }
            let Some(am) = self.base.node_materials(*anc)? else {
                drops.push(SelectionDecision::new(
                    kind,
                    anc.to_string(),
                    Disposition::Dropped,
                    "ancestor dropped: no materials available",
                    0,
                ));
                continue;
            };
            let Some(text) = render_ancestor(&am, policy.ancestor_strategy) else {
                drops.push(SelectionDecision::new(
                    kind,
                    anc.to_string(),
                    Disposition::Dropped,
                    "ancestor dropped: strategy folds no text (drop) or ancestor empty",
                    0,
                ));
                continue;
            };
            let cost = estimate_tokens(&text);
            if budget_exhausted || used.saturating_add(cost) > budget {
                budget_exhausted = true;
                drops.push(SelectionDecision::new(
                    kind,
                    anc.to_string(),
                    Disposition::Dropped,
                    "ancestor dropped: lineage budget exhausted",
                    cost,
                ));
                continue;
            }
            used += cost;
            selected.push(AncestorText::new(*anc, text));
        }

        // 4. Fold the selected ancestors into the right materials field so the
        //    frozen layered compiler emits them per the strategy.
        match policy.ancestor_strategy {
            AncestorStrategy::Verbatim => materials.ancestor_verbatim = selected,
            AncestorStrategy::Summarize => materials.ancestor_summaries = selected,
            AncestorStrategy::HandoffOnly => materials.ancestor_handoffs = selected,
            AncestorStrategy::Drop => { /* selected is empty under Drop */ }
        }

        // 5. Assemble via the frozen single-turn pipeline (ordering + prefix_hash).
        let layered = LayeredCompiler::new(OneShotSource { node, materials });
        let mut compiled = layered.compile(node, policy)?;

        // 6. Augment the audit trace with lineage-selection drops (this never
        //    affects the prefix_hash, which is computed over layers only).
        compiled.selection_trace.extend(drops);
        Ok(compiled)
    }
}

/// A one-node [`ContextSource`] returning pre-assembled materials for the target.
struct OneShotSource {
    node: Ulid,
    materials: NodeMaterials,
}

impl ContextSource for OneShotSource {
    fn node_materials(&self, node: Ulid) -> Result<Option<NodeMaterials>, ContextError> {
        if node == self.node {
            Ok(Some(self.materials.clone()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Degrade;
    use std::collections::BTreeMap;

    /// An in-memory base source + a fixed parent map for the walker.
    struct MemSource {
        materials: BTreeMap<Ulid, NodeMaterials>,
    }
    impl ContextSource for MemSource {
        fn node_materials(&self, node: Ulid) -> Result<Option<NodeMaterials>, ContextError> {
            Ok(self.materials.get(&node).cloned())
        }
    }

    struct MapWalker {
        chain: Vec<Ulid>, // ancestors nearest-first for the one target
        target: Ulid,
    }
    impl LineageWalker for MapWalker {
        fn ancestors(&self, node: Ulid) -> Result<Vec<Ulid>, ContextError> {
            if node == self.target {
                Ok(self.chain.clone())
            } else {
                Ok(vec![])
            }
        }
    }

    fn node_materials(user: &str, diff: &str) -> NodeMaterials {
        NodeMaterials {
            system_prompt: Some("you are an agent".into()),
            repo_map: Some("src/lib.rs".into()),
            user_msg: Some(user.into()),
            current_diff: Some(diff.into()),
            ..Default::default()
        }
    }

    #[test]
    fn summarize_strategy_folds_ancestors_into_prefix() {
        let target = Ulid::new();
        let p1 = Ulid::new();
        let p2 = Ulid::new();
        let mut materials = BTreeMap::new();
        materials.insert(target, node_materials("do Y", "edit target"));
        materials.insert(p1, node_materials("did X", "edit p1"));
        materials.insert(p2, node_materials("did W", "edit p2"));
        let base = MemSource { materials };
        let walker = MapWalker {
            chain: vec![p1, p2],
            target,
        };
        let compiler = LineageCompiler::new(base, walker);
        let pol = ContextPolicy::new(64_000, AncestorStrategy::Summarize, Degrade::Compact);
        let cc = compiler.compile(target, &pol).unwrap();
        // Ancestor summaries appear as rank-2 prefix layers.
        assert!(cc
            .stable_prefix()
            .iter()
            .any(|l| l.kind == ContextLayerKind::AncestorSummary));
    }

    #[test]
    fn sibling_isolation_holds_by_construction() {
        // The walker only returns the target's ancestry; a sibling node is never
        // walked. Compiling the sibling yields no ancestor layers from the other
        // branch.
        let target = Ulid::new();
        let sibling = Ulid::new();
        let parent = Ulid::new();
        let mut materials = BTreeMap::new();
        materials.insert(target, node_materials("t", "t"));
        materials.insert(sibling, node_materials("s", "s"));
        materials.insert(parent, node_materials("p", "p"));
        let base = MemSource { materials };
        // Walker returns the parent only for `target`; `sibling` has no ancestry
        // here (a different lineage), so compiling it pulls nothing cross-branch.
        let walker = MapWalker {
            chain: vec![parent],
            target,
        };
        let compiler = LineageCompiler::new(base, walker);
        let pol = ContextPolicy::new(64_000, AncestorStrategy::Summarize, Degrade::Compact);
        let cc = compiler.compile(sibling, &pol).unwrap();
        assert!(!cc
            .layers
            .iter()
            .any(|l| l.kind == ContextLayerKind::AncestorSummary));
    }

    #[test]
    fn deep_branch_keeps_stable_prefix_hash_across_siblings() {
        // The P7 DoD: an Edit on a deep branch compiles context with a stable
        // prefix_hash verified to be reused across siblings sharing the lineage.
        let p1 = Ulid::new();
        let a = Ulid::new();
        let b = Ulid::new();
        let mut materials = BTreeMap::new();
        // Two siblings a, b share parent p1's summary (same stable prefix), with
        // different volatile current turns.
        materials.insert(p1, node_materials("parent did X", "parent diff"));
        materials.insert(a, node_materials("sibling A asks", "A diff"));
        materials.insert(b, node_materials("sibling B asks something else", "B diff"));
        let base = MemSource { materials };
        // Both siblings walk to the same ancestor p1.
        let walker_a = MapWalker {
            chain: vec![p1],
            target: a,
        };
        let walker_b = MapWalker {
            chain: vec![p1],
            target: b,
        };
        let pol = ContextPolicy::new(64_000, AncestorStrategy::Summarize, Degrade::Compact);
        let base_b = MemSource {
            materials: clone_materials(&base),
        };
        let ca = LineageCompiler::new(base, walker_a)
            .compile(a, &pol)
            .unwrap();
        let cb = LineageCompiler::new(base_b, walker_b)
            .compile(b, &pol)
            .unwrap();
        assert_eq!(
            ca.prefix_hash, cb.prefix_hash,
            "siblings sharing lineage reuse the warm cache"
        );
        assert_ne!(ca.volatile_tail(), cb.volatile_tail());
    }

    #[test]
    fn budget_drops_far_ancestors_and_records_them() {
        let target = Ulid::new();
        let near = Ulid::new();
        let far = Ulid::new();
        let big = "x".repeat(4000); // ~1000 tokens each
        let mut materials = BTreeMap::new();
        materials.insert(target, node_materials("now", "now diff"));
        materials.insert(near, node_materials(&big, ""));
        materials.insert(far, node_materials(&big, ""));
        let base = MemSource { materials };
        let walker = MapWalker {
            chain: vec![near, far],
            target,
        };
        // Verbatim keeps the full ~1000-token ancestor text; the lineage budget
        // fits the nearest but not the second, so the far one is budget-dropped.
        let compiler = LineageCompiler::new(base, walker).with_lineage_budget(1, 1);
        let pol = ContextPolicy::new(1100, AncestorStrategy::Verbatim, Degrade::Compact);
        let cc = compiler.compile(target, &pol).unwrap();
        // Far ancestor was dropped for budget and the drop is recorded.
        assert!(cc.selection_trace.iter().any(|d| {
            d.disposition == Disposition::Dropped && d.reason.contains("budget exhausted")
        }));
    }

    #[test]
    fn depth_cap_drops_excess_ancestors() {
        let target = Ulid::new();
        let chain: Vec<Ulid> = (0..5).map(|_| Ulid::new()).collect();
        let mut materials = BTreeMap::new();
        materials.insert(target, node_materials("now", "now"));
        for id in &chain {
            materials.insert(*id, node_materials("anc", "anc"));
        }
        let base = MemSource { materials };
        let walker = MapWalker {
            chain: chain.clone(),
            target,
        };
        let compiler = LineageCompiler::new(base, walker).with_max_ancestors(2);
        let pol = ContextPolicy::new(64_000, AncestorStrategy::Summarize, Degrade::Compact);
        let cc = compiler.compile(target, &pol).unwrap();
        assert!(cc
            .selection_trace
            .iter()
            .any(|d| d.reason.contains("depth cap")));
    }

    #[test]
    fn unknown_target_is_not_found() {
        let base = MemSource {
            materials: BTreeMap::new(),
        };
        let t = Ulid::new();
        let walker = MapWalker {
            chain: vec![],
            target: t,
        };
        let err = LineageCompiler::new(base, walker)
            .compile(t, &ContextPolicy::default())
            .unwrap_err();
        assert!(matches!(err, ContextError::NodeNotFound(_)));
    }

    fn clone_materials(src: &MemSource) -> BTreeMap<Ulid, NodeMaterials> {
        src.materials.clone()
    }
}
