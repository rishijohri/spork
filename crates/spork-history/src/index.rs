//! The v1 [`HistoryIndex`] over the content-addressed transcript store + DAG
//! (DESIGN.md §13.7, §4.7).
//!
//! The index answers lineage/history questions over the work-DAG — "where was
//! this decided", "what touched this file on this lineage", "show me that node's
//! transcript". It reads the **single** content-addressed transcript store (it
//! does **not** duplicate history into per-sandbox files) and the DAG projection,
//! and it is **auto lineage-scoped** so a query never silently leaks
//! sibling-branch context. It is read-only and introduces **no** schema change —
//! it is a pure consumer of already-stored data (CLAUDE.md C2/C3).
//!
//! The store/graph access is behind the [`HistorySource`] seam so the index is
//! offline-testable; the daemon wires a graph-backed source.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::regex::regex_is_match;

/// A node as the history index sees it (a read-only projection of the envelope +
/// its derived facts). Carries no secrets and no mutable handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryNode {
    /// The node id.
    pub id: Ulid,
    /// The node kind (e.g. `"codebase-edit"`).
    pub kind: String,
    /// The branch the node was created on.
    pub branch_id: String,
    /// A short human summary (from the node's handoff / edit summary), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Files this node touched (from its Edit payload), if any.
    #[serde(default)]
    pub files_touched: Vec<String>,
    /// Recorded decisions for this node (from its handoff), if any.
    #[serde(default)]
    pub decisions: Vec<String>,
}

/// The read seam the index queries (DESIGN.md §13.7).
///
/// All methods are read-only. The daemon provides a graph-backed implementation;
/// tests use an in-memory one.
pub trait HistorySource {
    /// Fetch a node's read-only projection, or `None` if unknown.
    fn node(&self, id: Ulid) -> Option<HistoryNode>;
    /// The ancestors of `id`, nearest-first (lineage only, never siblings).
    fn ancestors(&self, id: Ulid) -> Vec<Ulid>;
    /// Every node id in the project.
    fn all_nodes(&self) -> Vec<Ulid>;
    /// The canonical transcript text for a node, or `None`.
    fn transcript(&self, id: Ulid) -> Option<String>;
    /// The handoff document text for a node, or `None`.
    fn handoff(&self, id: Ulid) -> Option<String>;
}

/// The scope a query runs over (DESIGN.md §13.7). Lineage is the default so a
/// query never silently leaks sibling-branch context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The anchor node and its ancestors (the default).
    #[default]
    Lineage,
    /// Every node on the anchor's branch.
    Branch,
    /// The whole project.
    Project,
}

/// How `search_history` interprets the pattern (DESIGN.md §13.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SearchKind {
    /// Case-insensitive substring search (the default).
    #[default]
    Text,
    /// Regular-expression search (supported subset — see [`crate::regex`]).
    Regex,
    /// Structured search over a node's derived fields (kind, files, decisions).
    Structured,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// The matching node id.
    pub node_id: Ulid,
    /// Which field matched (`"transcript"`, `"summary"`, `"file"`, `"decision"`,
    /// `"kind"`).
    pub field: String,
    /// A short snippet of the matched content.
    pub snippet: String,
}

/// A decision surfaced by `find_decisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionHit {
    /// The node the decision was recorded on.
    pub node_id: Ulid,
    /// The decision text.
    pub decision: String,
}

/// A file-touch surfaced by `find_files_touched`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTouchHit {
    /// The repo-relative path.
    pub path: String,
    /// The nodes (in scope) that touched it.
    pub nodes: Vec<Ulid>,
}

/// The maximum snippet length returned in a [`SearchHit`].
const SNIPPET_LEN: usize = 160;

/// The v1 read-only history index (DESIGN.md §13.7).
pub struct HistoryIndex<S: HistorySource> {
    source: S,
}

impl<S: HistorySource> HistoryIndex<S> {
    /// Construct an index over a [`HistorySource`].
    #[must_use]
    pub fn new(source: S) -> Self {
        HistoryIndex { source }
    }

    /// Borrow the underlying source (read-only).
    #[must_use]
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Resolve a scope to the set of node ids it covers, **auto lineage-scoped**
    /// by default so siblings never leak (DESIGN.md §13.7).
    ///
    /// `Lineage` and `Branch` require an `anchor`; with no anchor they fall back
    /// to the anchor-free interpretation that cannot leak: an empty set for
    /// `Lineage`/`Branch` (nothing to scope to) — never the whole project.
    fn scope_nodes(&self, scope: Scope, anchor: Option<Ulid>) -> Vec<Ulid> {
        match scope {
            Scope::Project => self.source.all_nodes(),
            Scope::Lineage => match anchor {
                Some(a) => {
                    let mut ids = vec![a];
                    ids.extend(self.source.ancestors(a));
                    ids
                }
                None => Vec::new(),
            },
            Scope::Branch => match anchor.and_then(|a| self.source.node(a)) {
                Some(node) => self
                    .source
                    .all_nodes()
                    .into_iter()
                    .filter(|id| {
                        self.source
                            .node(*id)
                            .is_some_and(|n| n.branch_id == node.branch_id)
                    })
                    .collect(),
                None => Vec::new(),
            },
        }
    }

    /// Search transcripts / DAG by pattern, scoped (default lineage) (DESIGN.md
    /// §13.7).
    #[must_use]
    pub fn search_history(
        &self,
        pattern: &str,
        kind: SearchKind,
        scope: Scope,
        anchor: Option<Ulid>,
    ) -> Vec<SearchHit> {
        let mut hits = Vec::new();
        for id in self.scope_nodes(scope, anchor) {
            let Some(node) = self.source.node(id) else {
                continue;
            };
            match kind {
                SearchKind::Structured => self.structured_hits(&node, pattern, &mut hits),
                SearchKind::Text | SearchKind::Regex => {
                    let matcher = |hay: &str| matches(kind, pattern, hay);
                    if let Some(s) = &node.summary {
                        if matcher(s) {
                            hits.push(hit(id, "summary", s));
                        }
                    }
                    if let Some(t) = self.source.transcript(id) {
                        if matcher(&t) {
                            hits.push(hit(id, "transcript", &t));
                        }
                    }
                }
            }
        }
        hits
    }

    fn structured_hits(&self, node: &HistoryNode, pattern: &str, out: &mut Vec<SearchHit>) {
        let p = pattern.to_lowercase();
        if node.kind.to_lowercase().contains(&p) {
            out.push(hit(node.id, "kind", &node.kind));
        }
        for f in &node.files_touched {
            if f.to_lowercase().contains(&p) {
                out.push(hit(node.id, "file", f));
            }
        }
        for d in &node.decisions {
            if d.to_lowercase().contains(&p) {
                out.push(hit(node.id, "decision", d));
            }
        }
    }

    /// Return the canonical transcript for a node (DESIGN.md §13.7).
    #[must_use]
    pub fn get_node_transcript(&self, node: Ulid) -> Option<String> {
        self.source.transcript(node)
    }

    /// Walk the lineage chain for context provenance, nearest-first, including
    /// the node itself first (DESIGN.md §13.7).
    #[must_use]
    pub fn walk_ancestors(&self, node: Ulid) -> Vec<HistoryNode> {
        let mut out = Vec::new();
        if let Some(n) = self.source.node(node) {
            out.push(n);
        }
        for id in self.source.ancestors(node) {
            if let Some(n) = self.source.node(id) {
                out.push(n);
            }
        }
        out
    }

    /// Surface recorded decisions across a scope (DESIGN.md §13.7).
    #[must_use]
    pub fn find_decisions(&self, scope: Scope, anchor: Option<Ulid>) -> Vec<DecisionHit> {
        let mut out = Vec::new();
        for id in self.scope_nodes(scope, anchor) {
            if let Some(node) = self.source.node(id) {
                for d in node.decisions {
                    out.push(DecisionHit {
                        node_id: id,
                        decision: d,
                    });
                }
            }
        }
        out
    }

    /// Enumerate files changed across a scope, each with the nodes that touched
    /// it (DESIGN.md §13.7).
    #[must_use]
    pub fn find_files_touched(&self, scope: Scope, anchor: Option<Ulid>) -> Vec<FileTouchHit> {
        use std::collections::BTreeMap;
        let mut by_file: BTreeMap<String, Vec<Ulid>> = BTreeMap::new();
        for id in self.scope_nodes(scope, anchor) {
            if let Some(node) = self.source.node(id) {
                for f in node.files_touched {
                    by_file.entry(f).or_default().push(id);
                }
            }
        }
        by_file
            .into_iter()
            .map(|(path, nodes)| FileTouchHit { path, nodes })
            .collect()
    }

    /// Fetch the durable handoff document for a node (DESIGN.md §13.7).
    #[must_use]
    pub fn get_handoff(&self, node: Ulid) -> Option<String> {
        self.source.handoff(node)
    }
}

/// Whether `pattern` matches `hay` under a (non-structured) search kind.
fn matches(kind: SearchKind, pattern: &str, hay: &str) -> bool {
    match kind {
        SearchKind::Regex => regex_is_match(pattern, hay),
        // Text and Structured (handled elsewhere) use case-insensitive contains.
        _ => hay.to_lowercase().contains(&pattern.to_lowercase()),
    }
}

fn hit(node_id: Ulid, field: &str, content: &str) -> SearchHit {
    SearchHit {
        node_id,
        field: field.to_string(),
        snippet: snippet(content),
    }
}

/// A short, char-boundary-safe snippet of `content`.
fn snippet(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.len() <= SNIPPET_LEN {
        return trimmed.to_string();
    }
    let mut end = SNIPPET_LEN;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemSource {
        nodes: BTreeMap<Ulid, HistoryNode>,
        parents: BTreeMap<Ulid, Vec<Ulid>>,
        transcripts: BTreeMap<Ulid, String>,
        handoffs: BTreeMap<Ulid, String>,
    }
    impl HistorySource for MemSource {
        fn node(&self, id: Ulid) -> Option<HistoryNode> {
            self.nodes.get(&id).cloned()
        }
        fn ancestors(&self, id: Ulid) -> Vec<Ulid> {
            // Transitive nearest-first via the parents map.
            let mut out = Vec::new();
            let mut frontier = self.parents.get(&id).cloned().unwrap_or_default();
            let mut seen = std::collections::BTreeSet::new();
            while let Some(p) = frontier.first().copied() {
                frontier.remove(0);
                if seen.insert(p) {
                    out.push(p);
                    frontier.extend(self.parents.get(&p).cloned().unwrap_or_default());
                }
            }
            out
        }
        fn all_nodes(&self) -> Vec<Ulid> {
            self.nodes.keys().copied().collect()
        }
        fn transcript(&self, id: Ulid) -> Option<String> {
            self.transcripts.get(&id).cloned()
        }
        fn handoff(&self, id: Ulid) -> Option<String> {
            self.handoffs.get(&id).cloned()
        }
    }

    fn node(id: Ulid, kind: &str, branch: &str) -> HistoryNode {
        HistoryNode {
            id,
            kind: kind.into(),
            branch_id: branch.into(),
            summary: None,
            files_touched: vec![],
            decisions: vec![],
        }
    }

    fn fixture() -> (MemSource, Ulid, Ulid, Ulid) {
        // root -> mid -> tip on branch "main"; sib on branch "feature".
        let root = Ulid::new();
        let mid = Ulid::new();
        let tip = Ulid::new();
        let sib = Ulid::new();
        let mut s = MemSource::default();
        let mut n_root = node(root, "codebase-edit", "main");
        n_root.decisions = vec!["use exponential backoff".into()];
        n_root.files_touched = vec!["src/client.rs".into()];
        s.nodes.insert(root, n_root);
        let mut n_mid = node(mid, "codebase-edit", "main");
        n_mid.files_touched = vec!["src/client.rs".into(), "src/retry.rs".into()];
        s.nodes.insert(mid, n_mid);
        s.nodes.insert(tip, node(tip, "codebase-edit", "main"));
        let mut n_sib = node(sib, "codebase-edit", "feature");
        n_sib.decisions = vec!["sibling-only secret decision".into()];
        s.nodes.insert(sib, n_sib);
        s.parents.insert(mid, vec![root]);
        s.parents.insert(tip, vec![mid]);
        s.transcripts
            .insert(root, "discussed retry strategy and backoff".into());
        s.transcripts
            .insert(sib, "this is the sibling branch transcript".into());
        s.handoffs.insert(tip, "handoff: retry implemented".into());
        (s, root, mid, tip)
    }

    #[test]
    fn lineage_scope_excludes_siblings() {
        let (s, _root, _mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        // Search the lineage of tip — must not see the sibling's decision.
        let hits = idx.search_history(
            "secret decision",
            SearchKind::Structured,
            Scope::Lineage,
            Some(tip),
        );
        assert!(
            hits.is_empty(),
            "sibling content must not leak into a lineage query"
        );
    }

    #[test]
    fn project_scope_sees_everything() {
        let (s, _root, _mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        let hits = idx.search_history(
            "secret decision",
            SearchKind::Structured,
            Scope::Project,
            Some(tip),
        );
        assert_eq!(hits.len(), 1, "project scope sees the sibling decision");
    }

    #[test]
    fn text_search_matches_transcript() {
        let (s, root, _mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        let hits = idx.search_history("backoff", SearchKind::Text, Scope::Lineage, Some(tip));
        assert!(hits
            .iter()
            .any(|h| h.node_id == root && h.field == "transcript"));
    }

    #[test]
    fn regex_search_matches() {
        let (s, root, _mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        let hits = idx.search_history(
            "retry.*backoff",
            SearchKind::Regex,
            Scope::Lineage,
            Some(tip),
        );
        assert!(hits.iter().any(|h| h.node_id == root));
    }

    #[test]
    fn walk_ancestors_returns_chain_nearest_first() {
        let (s, root, mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        let chain = idx.walk_ancestors(tip);
        let ids: Vec<Ulid> = chain.iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![tip, mid, root]);
    }

    #[test]
    fn get_transcript_and_handoff() {
        let (s, root, _mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        assert!(idx.get_node_transcript(root).unwrap().contains("backoff"));
        assert!(idx.get_handoff(tip).unwrap().contains("retry implemented"));
    }

    #[test]
    fn find_decisions_is_lineage_scoped() {
        let (s, root, _mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        let decisions = idx.find_decisions(Scope::Lineage, Some(tip));
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].node_id, root);
        assert!(decisions[0].decision.contains("backoff"));
    }

    #[test]
    fn find_files_touched_aggregates_nodes() {
        let (s, root, mid, tip) = fixture();
        let idx = HistoryIndex::new(s);
        let files = idx.find_files_touched(Scope::Lineage, Some(tip));
        let client = files.iter().find(|f| f.path == "src/client.rs").unwrap();
        // Both root and mid touched src/client.rs.
        assert!(client.nodes.contains(&root));
        assert!(client.nodes.contains(&mid));
    }

    #[test]
    fn lineage_scope_without_anchor_is_empty_not_project() {
        let (s, _root, _mid, _tip) = fixture();
        let idx = HistoryIndex::new(s);
        // No anchor + lineage scope -> empty (never silently the whole project).
        let hits = idx.search_history("anything", SearchKind::Text, Scope::Lineage, None);
        assert!(hits.is_empty());
    }
}
