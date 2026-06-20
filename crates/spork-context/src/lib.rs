//! Spork F4 context seam — lineage-compiled, prefix-hash-cached context.
//!
//! This crate freezes how a node's model context is compiled from its DAG
//! lineage: layers are ordered stable-to-volatile by a frozen volatility rank,
//! and the stable prefix is keyed by a `prefix_hash` so siblings and successive
//! turns reuse the warm cache. Per the foundation discipline (CLAUDE.md C3)
//! each trait is a seam and F4 ships exactly one complete v1 behind it; lineage
//! walking and ancestor selection are added additively in P7.
//!
//! # The frozen surface
//!
//! - [`ContextLayerKind`] with [`volatility_rank`] — the frozen, total ordering:
//!   System / ProjectMemory = 0, RepoMap = 1, Handoff / AncestorSummary = 2,
//!   AncestorVerbatim = 3, CurrentDiff / ToolResults = 4, UserMsg = 5.
//! - [`ContextPolicy`] — the schema-versioned budget / ancestor-strategy /
//!   degrade policy (CLAUDE.md C5).
//! - [`CompiledContext`] — the ordered [`ContextLayer`]s, the [`prefix_hash_of`]
//!   the stable prefix (ranks 0..=2), and a [`SelectionDecision`] trace
//!   explaining inclusions.
//! - [`ContextCompiler`] — the compile seam. F4 ships [`LayeredCompiler`], a
//!   single-turn layered assembler that orders layers by volatility rank, keys
//!   the stable prefix by `prefix_hash`, and places compaction summaries *after*
//!   the cached prefix so the prefix stays warm.
//! - [`HandoffGenerator`] / [`HandoffDocument`] — F4 ships
//!   [`SingleNodeHandoffGenerator`], a single-node, regenerable handoff carrying
//!   a `lineage_hash` (schema-versioned).
//!
//! # The lineage-material seam
//!
//! *How* layer/handoff materials are fetched (from the F2 graph projection, the
//! content-addressed transcripts of §12.3, or the §13.6 memory store) is held
//! behind the [`ContextSource`] / [`HandoffSource`] traits, separate from *how
//! they are ordered and cache-keyed* — which is what this crate freezes. This
//! keeps the v1 compiler fully offline-testable; a [`GraphHandoffSource`] adapter
//! wires a real [`spork_graph::GraphProjection`] so a generated handoff binds to
//! the node's authoritative `lineage_hash`.
//!
//! # The cache-reuse property (DESIGN.md §13.2)
//!
//! ```
//! use spork_context::{
//!     ContextCompiler, ContextPolicy, ContextSource, LayeredCompiler, NodeMaterials,
//!     ContextError,
//! };
//! use ulid::Ulid;
//!
//! // A tiny in-memory source: two sibling turns sharing a stable prefix but with
//! // different volatile tails (current diff + user message).
//! struct Src([(Ulid, NodeMaterials); 2]);
//! impl ContextSource for Src {
//!     fn node_materials(&self, node: Ulid) -> Result<Option<NodeMaterials>, ContextError> {
//!         Ok(self.0.iter().find(|(id, _)| *id == node).map(|(_, m)| m.clone()))
//!     }
//! }
//!
//! let a = Ulid::new();
//! let b = Ulid::new();
//! let stable = |user: &str, diff: &str| NodeMaterials {
//!     system_prompt: Some("you are an agent".into()),
//!     repo_map: Some("src/lib.rs".into()),
//!     current_diff: Some(diff.into()),
//!     user_msg: Some(user.into()),
//!     ..Default::default()
//! };
//! let compiler = LayeredCompiler::new(Src([
//!     (a, stable("turn one", "diff one")),
//!     (b, stable("turn two — entirely different", "diff two")),
//! ]));
//! let pol = ContextPolicy::default();
//! let ca = compiler.compile(a, &pol).unwrap();
//! let cb = compiler.compile(b, &pol).unwrap();
//!
//! // Same stable prefix -> same prefix_hash (the warm cache is reused)...
//! assert_eq!(ca.prefix_hash, cb.prefix_hash);
//! // ...even though the volatile tails differ.
//! assert_ne!(ca.volatile_tail(), cb.volatile_tail());
//! ```
//!
//! This realizes the context-compilation model in DESIGN.md §13.2 ("Cache-aligned,
//! on-demand compilation"), the budget-bounded retrieval policy in §13.3, the
//! handoff document in §13.5, and the auditable `SelectionDecision` trace in
//! §13.6.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod compiled;
mod compiler;
mod error;
mod handoff;
mod layer;
mod policy;
mod source;
mod trace;

pub use compiled::{
    prefix_hash_of, CompiledContext, COMPILED_CONTEXT_SCHEMA_VERSION, PREFIX_HASH_SCHEMA_VERSION,
};
pub use compiler::{ContextCompiler, LayeredCompiler};
pub use error::ContextError;
pub use handoff::{
    FileTouched, HandoffDocument, HandoffGenerator, SingleNodeHandoffGenerator,
    HANDOFF_DOCUMENT_SCHEMA_VERSION,
};
pub use layer::{
    estimate_tokens, volatility_rank, ContextLayer, ContextLayerKind, CONTEXT_LAYER_SCHEMA_VERSION,
    PREFIX_MAX_RANK,
};
pub use policy::{AncestorStrategy, ContextPolicy, Degrade, CONTEXT_POLICY_SCHEMA_VERSION};
pub use source::{
    AncestorText, ContextSource, GraphHandoffSource, HandoffDistillation, HandoffMaterials,
    HandoffSource, NodeMaterials,
};
pub use trace::{Disposition, SelectionDecision, SELECTION_DECISION_SCHEMA_VERSION};
