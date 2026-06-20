//! The lineage-material seams: [`ContextSource`] and [`HandoffSource`].
//!
//! Context is **compiled from DAG lineage** (DESIGN.md §13.2): the compiler and
//! handoff generator need a node's system prompt, project memory, repo map,
//! ancestor materials, current diff, tool results, and latest user message. How
//! those materials are fetched (from the F2 graph projection, the content-addressed
//! transcripts of §12.3, or the §13.6 memory store) is a *separate* concern from
//! *how they are ordered and cache-keyed*, which is what this crate freezes.
//!
//! These two traits are that boundary. F4 keeps the compiler offline-testable by
//! depending only on the trait; a [`GraphHandoffSource`] adapter wires a real
//! [`spork_graph::GraphProjection`] to [`HandoffSource`] so a generated handoff
//! binds to the node's actual `lineage_hash` (DESIGN.md §6.3, §13.5). P7 adds the
//! lineage-*walking* source (which ancestors, how deep) additively behind the
//! same traits.

use spork_hash::Hash;
use ulid::Ulid;

use crate::error::ContextError;
use crate::handoff::FileTouched;

/// The raw, unordered materials a single node contributes to its context.
///
/// Every field is optional content the source *may* supply for a node; the
/// compiler decides — per the [`ContextPolicy`](crate::ContextPolicy) — which to
/// emit, in what frozen order, and how to fold ancestors. Keeping the materials
/// flat and order-free here is what lets the compiler own the
/// stable→volatile ordering and the `prefix_hash` (DESIGN.md §13.2).
///
/// A `None` field means the source has nothing of that kind for the node (and
/// the compiler records a `Dropped` [`SelectionDecision`](crate::SelectionDecision)
/// for it); an empty `Vec` means likewise for the repeated materials.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeMaterials {
    /// The system / developer instruction prompt (rank 0).
    pub system_prompt: Option<String>,
    /// Project-scoped durable memory (rank 0).
    pub project_memory: Option<String>,
    /// The tree-sitter repo map (rank 1).
    pub repo_map: Option<String>,
    /// Distilled handoff text for ancestors, each tagged with its source node id
    /// (rank 2) — the input to the
    /// [`HandoffOnly`](crate::AncestorStrategy::HandoffOnly) strategy.
    pub ancestor_handoffs: Vec<AncestorText>,
    /// Summarized ancestor discussion, each tagged with its source node id
    /// (rank 2) — the input to the
    /// [`Summarize`](crate::AncestorStrategy::Summarize) strategy.
    pub ancestor_summaries: Vec<AncestorText>,
    /// Verbatim ancestor transcripts, each tagged with its source node id
    /// (rank 3) — the input to the
    /// [`Verbatim`](crate::AncestorStrategy::Verbatim) strategy.
    pub ancestor_verbatim: Vec<AncestorText>,
    /// The current node's code diff (rank 4).
    pub current_diff: Option<String>,
    /// Tool-call results for the current turn (rank 4).
    pub tool_results: Option<String>,
    /// The latest user message (rank 5).
    pub user_msg: Option<String>,
}

/// A piece of ancestor-derived text tagged with the ancestor node it came from.
///
/// The `source` id is what the [`SelectionDecision`](crate::SelectionDecision)
/// trace names so the audit panel can point at the exact ancestor (DESIGN.md
/// §13.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AncestorText {
    /// The ancestor node id this text was derived from.
    pub source: Ulid,
    /// The derived text (a handoff distillation, a summary, or a verbatim
    /// transcript rendering).
    pub text: String,
}

impl AncestorText {
    /// Construct an [`AncestorText`] from a source node id and its text.
    #[must_use]
    pub fn new(source: Ulid, text: impl Into<String>) -> Self {
        AncestorText {
            source,
            text: text.into(),
        }
    }
}

/// The seam that supplies a node's [`NodeMaterials`] to the compiler.
///
/// The v1 compiler depends only on this trait, so it is fully offline-testable
/// against an in-memory source while a production source draws from the graph
/// projection, the content-addressed transcripts, and the memory store
/// (DESIGN.md §13.2, §13.6).
pub trait ContextSource {
    /// Fetch the materials a node contributes to its context, or `None` if the
    /// node is unknown.
    ///
    /// # Errors
    ///
    /// Returns a [`ContextError`] if the underlying store fails (e.g. a graph
    /// projection query error surfaced as [`ContextError::NodeNotFound`] or a
    /// canonicalization failure).
    fn node_materials(&self, node: Ulid) -> Result<Option<NodeMaterials>, ContextError>;
}

/// The raw distillation materials a single node contributes to its handoff.
///
/// These are the §13.5 fields a [`HandoffGenerator`](crate::HandoffGenerator)
/// distills into a [`HandoffDocument`](crate::HandoffDocument). The
/// `lineage_hash` is mandatory — it binds the handoff to the exact ancestry it
/// was distilled from so it can be regenerated when an ancestor is edited
/// (DESIGN.md §13.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffMaterials {
    /// A prose summary of what the node accomplished.
    pub summary: String,
    /// The key decisions made.
    pub key_decisions: Vec<String>,
    /// The files touched, with rationale.
    pub files_touched: Vec<FileTouched>,
    /// Threads left open for the next agent.
    pub open_threads: Vec<String>,
    /// Constraints the next agent must respect.
    pub constraints: Vec<String>,
    /// The test state at this node.
    pub test_state: String,
    /// The node's lineage hash — binds the handoff to its ancestry (§13.5).
    pub lineage_hash: Hash,
}

/// The seam that supplies a node's [`HandoffMaterials`] to the generator.
///
/// Separated from [`ContextSource`] because a handoff is generated *once* at node
/// completion from completion-time materials, whereas context is compiled fresh
/// every turn; keeping the two seams distinct keeps each minimal (DESIGN.md
/// §13.5).
pub trait HandoffSource {
    /// Fetch the handoff materials for a node, or `None` if the node is unknown.
    ///
    /// # Errors
    ///
    /// Returns a [`ContextError`] if the underlying store fails.
    fn handoff_materials(&self, node: Ulid) -> Result<Option<HandoffMaterials>, ContextError>;
}

/// A [`HandoffSource`] adapter backed by a real [`spork_graph::GraphProjection`].
///
/// It reads a node's envelope from the materialized graph projection and binds
/// the generated handoff to the node's authoritative `lineage_hash` (DESIGN.md
/// §6.3). The *distillation* materials (summary, decisions, files, …) are
/// supplied by a caller-provided closure, because the prose distillation itself
/// is a model-driven step deferred to a later phase; the structural binding —
/// node existence and `lineage_hash` — is what this adapter freezes against the
/// real graph (CLAUDE.md C3: the seam is real, the model step is additive).
pub struct GraphHandoffSource<'a, F>
where
    F: Fn(&spork_graph::NodeEnvelope) -> HandoffDistillation,
{
    projection: &'a spork_graph::GraphProjection,
    distill: F,
}

/// The model-distilled portion of a handoff — everything except the structural
/// `lineage_hash`, which the [`GraphHandoffSource`] supplies from the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandoffDistillation {
    /// A prose summary of what the node accomplished.
    pub summary: String,
    /// The key decisions made.
    pub key_decisions: Vec<String>,
    /// The files touched, with rationale.
    pub files_touched: Vec<FileTouched>,
    /// Threads left open for the next agent.
    pub open_threads: Vec<String>,
    /// Constraints the next agent must respect.
    pub constraints: Vec<String>,
    /// The test state at this node.
    pub test_state: String,
}

impl<'a, F> GraphHandoffSource<'a, F>
where
    F: Fn(&spork_graph::NodeEnvelope) -> HandoffDistillation,
{
    /// Construct a graph-backed handoff source over a projection and a
    /// distillation function.
    #[must_use]
    pub fn new(projection: &'a spork_graph::GraphProjection, distill: F) -> Self {
        GraphHandoffSource {
            projection,
            distill,
        }
    }
}

impl<F> HandoffSource for GraphHandoffSource<'_, F>
where
    F: Fn(&spork_graph::NodeEnvelope) -> HandoffDistillation,
{
    fn handoff_materials(&self, node: Ulid) -> Result<Option<HandoffMaterials>, ContextError> {
        let envelope = match self
            .projection
            .get_node(node)
            .map_err(|e| ContextError::NodeNotFound(format!("{node}: {e}")))?
        {
            Some(env) => env,
            None => return Ok(None),
        };
        let d = (self.distill)(&envelope);
        Ok(Some(HandoffMaterials {
            summary: d.summary,
            key_decisions: d.key_decisions,
            files_touched: d.files_touched,
            open_threads: d.open_threads,
            constraints: d.constraints,
            test_state: d.test_state,
            // The authoritative lineage hash comes from the graph, never the
            // distillation — that is what makes the handoff regenerable (§13.5).
            lineage_hash: envelope.lineage_hash,
        }))
    }
}
