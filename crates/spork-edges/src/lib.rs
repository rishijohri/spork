//! Spork typed edges, ref kinds, and acyclicity.
//!
//! The Spork graph is a directed acyclic graph whose edges are *typed*
//! (parent/child, branch, derived-from, validates, checks, stresses,
//! merge-parent), and whose refs (head, branch, tag) are the garbage-collection
//! roots. This crate owns the frozen edge-type and ref-kind sets plus the
//! acyclicity guard: adding an edge `from → to` would create a cycle exactly
//! when `from` is already reachable from `to` along parent links, so a new edge
//! is admitted only when it preserves the DAG invariant.
//!
//! This realizes the edge, acyclicity, and lineage model in DESIGN.md §6.3
//! ("Edges, acyclicity, and lineage"). The edge-type set is the directional
//! relation set enumerated there (`PARENT_CHILD`, `BRANCH`, `DERIVED_FROM`,
//! `VALIDATES` / `CHECKS` / `STRESSES`, `MERGE_PARENT`); the ref kinds are the
//! mutable GC-root pointers (`HEAD`, `branch/*`, tags) described in the same
//! section.
//!
//! # Design contract
//!
//! * **Frozen sets (C2 — no domino).** [`EdgeType`] and [`RefKind`] are the
//!   complete, ordered, frozen relation/ref sets for Phase F2. They are
//!   `#[non_exhaustive]`-free *value* enums on purpose — the variant set is the
//!   contract — while [`EdgeError`] is `#[non_exhaustive]` so new failure modes
//!   can be added additively without breaking matchers.
//! * **Acyclicity is a pure read.** [`would_create_cycle`] never mutates; it
//!   reads the current parent relation through the [`Adjacency`] seam (C3 — one
//!   real impl elsewhere, the trait is the seam) and answers a yes/no question.
//!   The command layer ([`spork-graph`](../spork_graph/index.html)) validates
//!   with this guard *before* appending a `graph.edge_added` event, so the
//!   projection — a pure fold of the log — is always a DAG.
//! * **Versioned wire forms (C5).** Both enums serialize to stable string tags
//!   (`SCREAMING_SNAKE_CASE` for edges, `PascalCase` for refs); the tag schema
//!   carries an explicit [`EDGE_TYPE_SCHEMA_VERSION`] / [`REF_KIND_SCHEMA_VERSION`]
//!   so the persisted representation can evolve forward-compatibly.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Schema version of the persisted [`EdgeType`] string tag set.
///
/// Bumped only when the on-the-wire tag mapping changes in a
/// non-backward-compatible way (DESIGN.md §6.3, constraint C5). Adding a new
/// variant is an additive change that does *not* require a bump unless older
/// readers must reject it.
pub const EDGE_TYPE_SCHEMA_VERSION: u16 = 1;

/// Schema version of the persisted [`RefKind`] string tag set.
///
/// See [`EDGE_TYPE_SCHEMA_VERSION`]; the same evolution discipline applies
/// (DESIGN.md §6.3, constraint C5).
pub const REF_KIND_SCHEMA_VERSION: u16 = 1;

/// The complete, frozen set of directional edge relations in the timeline DAG.
///
/// Every edge in the graph is one of these typed relations (DESIGN.md §6.3).
/// Edges are **directional**: the orientation carries meaning, and the
/// acyclicity guard ([`would_create_cycle`]) interprets the `PARENT_CHILD`
/// orientation (and any other parent-style link surfaced through [`Adjacency`])
/// to keep the graph a DAG.
///
/// The variants, with their DESIGN.md §6.3 meanings:
///
/// * [`ParentChild`](EdgeType::ParentChild) — timeline lineage: the canonical
///   "this node descends from that node" relation.
/// * [`Branch`](EdgeType::Branch) — fork-point marker; a `forkBranch` creates a
///   head [`RefKind::Branch`] ref and copies no code until checkout.
/// * [`DerivedFrom`](EdgeType::DerivedFrom) — handoff/context provenance; how
///   the agent decides (via `includeAncestorContext`) whether it needs a prior
///   discussion node's context.
/// * [`Validates`](EdgeType::Validates) / [`Checks`](EdgeType::Checks) /
///   [`Stresses`](EdgeType::Stresses) — a test-class *observing* node targeting
///   an Edit node (validation / pattern-check / stress-test respectively).
/// * [`MergeParent`](EdgeType::MergeParent) — the additional (second-and-later)
///   parents of a merge node.
///
/// # Stable wire tags
///
/// Variants serialize to `SCREAMING_SNAKE_CASE` tags matching the DESIGN.md
/// names (`"PARENT_CHILD"`, `"BRANCH"`, `"DERIVED_FROM"`, `"VALIDATES"`,
/// `"CHECKS"`, `"STRESSES"`, `"MERGE_PARENT"`). These tags are the persisted
/// contract; see [`EDGE_TYPE_SCHEMA_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeType {
    /// Timeline lineage: a child node descends from a parent node.
    ParentChild,
    /// Fork-point marker; pairs with a [`RefKind::Branch`] head ref.
    Branch,
    /// Handoff / context provenance link.
    DerivedFrom,
    /// A validation/test observing node targets an edit node.
    Validates,
    /// A sanity/pattern-check observing node targets an edit node.
    Checks,
    /// A stress-test observing node targets an edit node.
    Stresses,
    /// An additional (≥2nd) parent of a merge node.
    MergeParent,
}

impl EdgeType {
    /// Every edge type, in declaration order.
    ///
    /// This is the frozen relation set (DESIGN.md §6.3). Iterating it lets
    /// downstream registries and UIs enumerate the legend without hardcoding the
    /// list, and lets tests assert the set is stable.
    pub const ALL: [EdgeType; 7] = [
        EdgeType::ParentChild,
        EdgeType::Branch,
        EdgeType::DerivedFrom,
        EdgeType::Validates,
        EdgeType::Checks,
        EdgeType::Stresses,
        EdgeType::MergeParent,
    ];

    /// The stable, persisted `SCREAMING_SNAKE_CASE` tag for this edge type.
    ///
    /// Matches the serde representation and the DESIGN.md §6.3 names. Provided
    /// as a `const fn` so callers can build allow-lists and error messages
    /// without a serializer round-trip.
    #[must_use]
    pub const fn as_tag(self) -> &'static str {
        match self {
            EdgeType::ParentChild => "PARENT_CHILD",
            EdgeType::Branch => "BRANCH",
            EdgeType::DerivedFrom => "DERIVED_FROM",
            EdgeType::Validates => "VALIDATES",
            EdgeType::Checks => "CHECKS",
            EdgeType::Stresses => "STRESSES",
            EdgeType::MergeParent => "MERGE_PARENT",
        }
    }
}

impl core::fmt::Display for EdgeType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_tag())
    }
}

/// The complete, frozen set of mutable ref kinds.
///
/// Refs are Git-like mutable pointers into the immutable graph and are the
/// **garbage-collection roots**: anything reachable from a live ref survives GC
/// (DESIGN.md §6.3, §6.4). A ref move is itself an event, so pointer history is
/// recoverable.
///
/// * [`Head`](RefKind::Head) — the working head pointer (`HEAD`).
/// * [`Branch`](RefKind::Branch) — a named branch pointer (`branch/*`).
/// * [`Tag`](RefKind::Tag) — an immutable-by-convention named tag.
///
/// Variants serialize to `PascalCase` tags (`"Head"`, `"Branch"`, `"Tag"`); see
/// [`REF_KIND_SCHEMA_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RefKind {
    /// The working head pointer (`HEAD`).
    Head,
    /// A named branch pointer (`branch/*`).
    Branch,
    /// A named tag pointer.
    Tag,
}

impl RefKind {
    /// Every ref kind, in declaration order.
    ///
    /// The frozen GC-root kind set (DESIGN.md §6.3).
    pub const ALL: [RefKind; 3] = [RefKind::Head, RefKind::Branch, RefKind::Tag];

    /// The stable, persisted `PascalCase` tag for this ref kind.
    #[must_use]
    pub const fn as_tag(self) -> &'static str {
        match self {
            RefKind::Head => "Head",
            RefKind::Branch => "Branch",
            RefKind::Tag => "Tag",
        }
    }
}

impl core::fmt::Display for RefKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_tag())
    }
}

/// A read-only view of the current parent relation over graph nodes.
///
/// This is the seam (constraint C3) the acyclicity guard reads through:
/// [`would_create_cycle`] walks ancestors purely via [`parents_of`]. The real
/// implementation lives in the graph projection ([`spork-graph`]), which answers
/// from indexed SQLite `edge` rows; tests in this crate supply small in-memory
/// implementations. Keeping the guard generic over `Adjacency` means the
/// cycle algorithm is unit-testable without a database and reusable over any
/// parent-link source.
///
/// # Contract
///
/// [`parents_of`] returns the **direct** parents of `node` — the set of `p`
/// such that an ancestor-style edge `node → p` exists (i.e. `node` descends
/// from `p`). It must be a finite list and must not include `node` itself
/// unless the underlying store genuinely records a self-parent (which the guard
/// will then correctly report as a cycle). Order is irrelevant to the guard.
///
/// [`parents_of`]: Adjacency::parents_of
/// [`spork-graph`]: ../spork_graph/index.html
pub trait Adjacency {
    /// Return the direct parents of `node`.
    ///
    /// A returned `p` means "`node` descends from `p`" — i.e. walking these
    /// links repeatedly enumerates `node`'s ancestors. See the trait-level
    /// contract for the precise meaning.
    fn parents_of(&self, node: Ulid) -> Vec<Ulid>;
}

/// Would adding the directed edge `from → to` introduce a cycle?
///
/// # Direction (read this carefully)
///
/// An edge `from → to` means **`from` descends from `to`** (`to` becomes an
/// ancestor of `from`), consistent with [`Adjacency::parents_of`], whose
/// returned nodes are the parents/ancestors of its argument. Adding `from → to`
/// therefore closes a cycle **iff `from` is already reachable from `to` by
/// walking parent links** — that is, `from` is already an ancestor of `to`, so
/// making `to` an ancestor of `from` would form a loop.
///
/// Concretely the function performs an **ancestor walk starting at `to`**: it
/// repeatedly follows [`Adjacency::parents_of`], and returns `true` the moment
/// it encounters `from` (or if `from == to`, a self-edge, which is always a
/// cycle).
///
/// The walk is iterative with an explicit stack and a `visited` set, so it is
/// safe even against an adjacency view that *already* contains a cycle (it
/// terminates rather than looping forever) and runs in `O(V + E)` over the
/// ancestor subgraph reachable from `to`.
///
/// # Examples
///
/// ```
/// use spork_edges::{would_create_cycle, Adjacency};
/// use ulid::Ulid;
///
/// struct Mem(std::collections::HashMap<Ulid, Vec<Ulid>>);
/// impl Adjacency for Mem {
///     fn parents_of(&self, n: Ulid) -> Vec<Ulid> {
///         self.0.get(&n).cloned().unwrap_or_default()
///     }
/// }
///
/// let a = Ulid::from_parts(1, 0);
/// let b = Ulid::from_parts(2, 0);
/// // b descends from a:  b -> a
/// let mut m = std::collections::HashMap::new();
/// m.insert(b, vec![a]);
/// let adj = Mem(m);
///
/// // Adding a -> b would close the loop (a already an ancestor of b).
/// assert!(would_create_cycle(&adj, a, b));
/// // Adding b -> a is the edge that already exists; not a *new* cycle source
/// // here because a has no path back to b.
/// assert!(!would_create_cycle(&adj, b, a));
/// ```
#[must_use]
pub fn would_create_cycle<A: Adjacency>(adj: &A, from: Ulid, to: Ulid) -> bool {
    // A self-edge is trivially a cycle.
    if from == to {
        return true;
    }

    // Ancestor walk from `to`: if we can reach `from`, then `from` is already an
    // ancestor of `to`, and adding `from -> to` (making `to` an ancestor of
    // `from`) would close the loop.
    let mut stack: Vec<Ulid> = adj.parents_of(to);
    let mut visited: std::collections::HashSet<Ulid> = std::collections::HashSet::new();
    visited.insert(to);

    while let Some(node) = stack.pop() {
        if node == from {
            return true;
        }
        if !visited.insert(node) {
            // Already explored (also guards against a pre-existing cycle in the
            // adjacency view).
            continue;
        }
        stack.extend(adj.parents_of(node));
    }

    false
}

/// Errors raised when validating a proposed edge.
///
/// This is the failure surface the command layer maps into its own error type
/// when an edge is rejected before append (DESIGN.md §6.3). It is
/// `#[non_exhaustive]` so additional rejection reasons can be added additively
/// (constraint C2 — no domino).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EdgeError {
    /// Adding `from → to` would violate the DAG invariant.
    ///
    /// Reported by callers that consult [`would_create_cycle`] before appending
    /// a `graph.edge_added` event.
    #[error("adding edge {from} -> {to} would create a cycle")]
    Cycle {
        /// The tail node of the rejected edge (the would-be descendant).
        from: Ulid,
        /// The head node of the rejected edge (the would-be ancestor).
        to: Ulid,
    },

    /// The `from`-node's type does not permit this edge type.
    ///
    /// A node type declares its `allowed_edges` in its descriptor
    /// (DESIGN.md §6.5); attempting an edge outside that allow-list is rejected.
    #[error("node kind {kind} does not allow edge type {edge}")]
    DisallowedEdgeType {
        /// The `id` of the offending node type (its registry kind).
        kind: String,
        /// The edge type that is not in the kind's allow-list.
        edge: EdgeType,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Tiny in-memory adjacency for tests: `node -> its direct parents`.
    #[derive(Default)]
    struct MemAdj {
        parents: HashMap<Ulid, Vec<Ulid>>,
    }

    impl MemAdj {
        /// Record that `child` descends from `parent` (edge `child -> parent`).
        fn link(&mut self, child: Ulid, parent: Ulid) {
            self.parents.entry(child).or_default().push(parent);
        }
    }

    impl Adjacency for MemAdj {
        fn parents_of(&self, node: Ulid) -> Vec<Ulid> {
            self.parents.get(&node).cloned().unwrap_or_default()
        }
    }

    /// Deterministic distinct ULIDs for tests.
    fn n(i: u64) -> Ulid {
        Ulid::from_parts(i, 0)
    }

    // ---- edge-type / ref-kind set stability --------------------------------

    #[test]
    fn edge_type_set_is_stable_and_complete() {
        // The frozen set, in the frozen order (DESIGN §6.3). If this changes,
        // it is a deliberate contract change, not an accident.
        assert_eq!(
            EdgeType::ALL,
            [
                EdgeType::ParentChild,
                EdgeType::Branch,
                EdgeType::DerivedFrom,
                EdgeType::Validates,
                EdgeType::Checks,
                EdgeType::Stresses,
                EdgeType::MergeParent,
            ]
        );
        assert_eq!(EdgeType::ALL.len(), 7);
    }

    #[test]
    fn ref_kind_set_is_stable_and_complete() {
        assert_eq!(RefKind::ALL, [RefKind::Head, RefKind::Branch, RefKind::Tag]);
        assert_eq!(RefKind::ALL.len(), 3);
    }

    #[test]
    fn edge_type_tags_match_design_names() {
        let pairs = [
            (EdgeType::ParentChild, "PARENT_CHILD"),
            (EdgeType::Branch, "BRANCH"),
            (EdgeType::DerivedFrom, "DERIVED_FROM"),
            (EdgeType::Validates, "VALIDATES"),
            (EdgeType::Checks, "CHECKS"),
            (EdgeType::Stresses, "STRESSES"),
            (EdgeType::MergeParent, "MERGE_PARENT"),
        ];
        for (variant, tag) in pairs {
            assert_eq!(variant.as_tag(), tag);
            assert_eq!(variant.to_string(), tag);
        }
    }

    #[test]
    fn ref_kind_tags_match_design_names() {
        assert_eq!(RefKind::Head.as_tag(), "Head");
        assert_eq!(RefKind::Branch.as_tag(), "Branch");
        assert_eq!(RefKind::Tag.as_tag(), "Tag");
        for k in RefKind::ALL {
            assert_eq!(k.to_string(), k.as_tag());
        }
    }

    #[test]
    fn edge_type_serde_round_trips_via_stable_tags() {
        for et in EdgeType::ALL {
            let json = serde_json::to_string(&et).unwrap();
            // Tag is the quoted SCREAMING_SNAKE_CASE name.
            assert_eq!(json, format!("\"{}\"", et.as_tag()));
            let back: EdgeType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, et);
        }
    }

    #[test]
    fn ref_kind_serde_round_trips_via_stable_tags() {
        for rk in RefKind::ALL {
            let json = serde_json::to_string(&rk).unwrap();
            assert_eq!(json, format!("\"{}\"", rk.as_tag()));
            let back: RefKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, rk);
        }
    }

    #[test]
    fn schema_versions_present() {
        assert_eq!(EDGE_TYPE_SCHEMA_VERSION, 1);
        assert_eq!(REF_KIND_SCHEMA_VERSION, 1);
    }

    // ---- acyclicity: self-edge ---------------------------------------------

    #[test]
    fn self_edge_is_a_cycle() {
        let adj = MemAdj::default();
        let a = n(1);
        assert!(would_create_cycle(&adj, a, a));
    }

    // ---- acyclicity: empty / disconnected ----------------------------------

    #[test]
    fn edge_between_unrelated_nodes_is_not_a_cycle() {
        let adj = MemAdj::default();
        let (a, b) = (n(1), n(2));
        // Nothing recorded; no path exists either way.
        assert!(!would_create_cycle(&adj, a, b));
        assert!(!would_create_cycle(&adj, b, a));
    }

    // ---- acyclicity: direct cycle ------------------------------------------

    #[test]
    fn direct_cycle_is_detected() {
        // Graph: b descends from a  (edge b -> a).
        let mut adj = MemAdj::default();
        let (a, b) = (n(1), n(2));
        adj.link(b, a);

        // Adding a -> b: a would descend from b, but a is already an ancestor
        // of b. Cycle.
        assert!(would_create_cycle(&adj, a, b));
        // The existing-direction edge b -> a closes nothing new: a has no path
        // back to b.
        assert!(!would_create_cycle(&adj, b, a));
    }

    // ---- acyclicity: transitive cycle --------------------------------------

    #[test]
    fn transitive_cycle_is_detected() {
        // Chain: c -> b -> a  (c descends from b, b descends from a).
        let mut adj = MemAdj::default();
        let (a, b, c) = (n(1), n(2), n(3));
        adj.link(b, a);
        adj.link(c, b);

        // a is a transitive ancestor of c. Adding a -> c (a descends from c)
        // closes the loop a -> c -> b -> a.
        assert!(would_create_cycle(&adj, a, c));
        // Likewise a -> b is a (shorter) transitive cycle.
        assert!(would_create_cycle(&adj, a, b));
    }

    #[test]
    fn transitive_non_cycle_is_accepted() {
        // Chain: c -> b -> a. Adding c -> a (a new shortcut to the same
        // ancestor) does NOT create a cycle — a is still an ancestor of c, the
        // edge points "up", and there is no path from c back to itself.
        let mut adj = MemAdj::default();
        let (a, b, c) = (n(1), n(2), n(3));
        adj.link(b, a);
        adj.link(c, b);

        assert!(!would_create_cycle(&adj, c, a));
        // Adding a wholly new descendant d -> a is fine.
        let d = n(4);
        assert!(!would_create_cycle(&adj, d, a));
    }

    // ---- acyclicity: diamond (DAG with multiple parents) -------------------

    #[test]
    fn diamond_dag_has_no_false_cycle() {
        // Diamond:  b -> a, c -> a, d -> b, d -> c   (d descends from both
        // b and c, which both descend from a). Pure DAG.
        let mut adj = MemAdj::default();
        let (a, b, c, d) = (n(1), n(2), n(3), n(4));
        adj.link(b, a);
        adj.link(c, a);
        adj.link(d, b);
        adj.link(d, c);

        // Any "downward" edge from an ancestor to a descendant is a cycle...
        assert!(would_create_cycle(&adj, a, d)); // a -> d closes a->d->b->a
        assert!(would_create_cycle(&adj, a, b));
        assert!(would_create_cycle(&adj, b, d));
        // ...but a fresh independent node creates none.
        let e = n(5);
        assert!(!would_create_cycle(&adj, e, d));
        assert!(!would_create_cycle(&adj, d, e));
    }

    // ---- acyclicity: robustness against a pre-existing cycle ----------------

    #[test]
    fn walk_terminates_even_if_adjacency_already_has_a_cycle() {
        // Construct a degenerate adjacency that itself contains a loop
        // (a -> b -> a). The guard must terminate (not infinite-loop) thanks
        // to its visited set, and still answer correctly.
        let mut adj = MemAdj::default();
        let (a, b, x) = (n(1), n(2), n(3));
        adj.link(a, b);
        adj.link(b, a);

        // x is unrelated to the loop: adding x -> a must terminate and report
        // no cycle (x is not reachable as an ancestor of a).
        assert!(!would_create_cycle(&adj, x, a));
        // Asking about a node inside the loop still terminates and reports the
        // cycle (b is an ancestor of a).
        assert!(would_create_cycle(&adj, b, a));
    }

    // ---- EdgeError surface --------------------------------------------------

    #[test]
    fn edge_error_cycle_displays_with_endpoints() {
        let (a, b) = (n(1), n(2));
        let e = EdgeError::Cycle { from: a, to: b };
        let msg = e.to_string();
        assert!(msg.contains(&a.to_string()));
        assert!(msg.contains(&b.to_string()));
        assert!(msg.contains("cycle"));
    }

    #[test]
    fn edge_error_disallowed_displays_kind_and_edge() {
        let e = EdgeError::DisallowedEdgeType {
            kind: "snapshot".to_string(),
            edge: EdgeType::Validates,
        };
        let msg = e.to_string();
        assert!(msg.contains("snapshot"));
        assert!(msg.contains("VALIDATES"));
    }

    #[test]
    fn edge_error_is_clone_eq() {
        let e = EdgeError::Cycle {
            from: n(7),
            to: n(8),
        };
        assert_eq!(e.clone(), e);
    }
}
