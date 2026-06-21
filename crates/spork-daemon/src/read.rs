//! The read side of the daemon: the [`GraphView`] view-model snapshot and the
//! lazy CAS reads (`node.diff`, `blob.read`).
//!
//! Reads return their data inline (never an `op_id`, never an event) — the other
//! half of the load-bearing IPC rule (DESIGN.md §5.5, §14.5). Selection is
//! served lazily from the CAS: a diff loads only the changed-path list (parent
//! tree vs. node tree), and a blob read fetches one file's bytes on demand, so
//! neither materializes a whole worktree (DESIGN.md §14.5).
//!
//! Design references: DESIGN.md §14.3 (the view-model boundary), §14.5 (lazy
//! selection/diffs).

use std::collections::BTreeMap;

use spork_cas::{EntryKind, LooseStore, ObjectStore};
use spork_hash::Hash;
use ulid::Ulid;

use crate::core::{Daemon, DaemonCore};
use crate::error::DaemonError;
use crate::view::{EdgeView, GraphView, NodeView, RefView, GRAPH_VIEW_SCHEMA_VERSION};

impl Daemon {
    /// The frozen read / view-model boundary: a denormalized read snapshot of the
    /// whole work-DAG the future renderer consumes (DESIGN.md §14.3).
    ///
    /// This is a *snapshot*, not a live handle — it reflects the graph at call
    /// time. Live deltas arrive over [`Daemon::subscribe_events`](crate::Daemon::subscribe_events).
    /// Nodes/edges/refs come out in a deterministic order (id, `(from,to,type)`,
    /// name) so two views of the same state are byte-identical.
    ///
    /// # Panics
    /// Panics only if the core lock is poisoned by a prior panic while held.
    #[must_use]
    pub fn graph_view(&self) -> GraphView {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        build_graph_view(&core)
    }
}

/// Build the denormalized [`GraphView`] from the live projection state.
///
/// Pulls the F2 [`ProjectionState`](spork_graph::ProjectionState) — itself an
/// ordered, denormalized read of the node/edge/ref tables — and flattens each
/// envelope/row into its renderer-facing view. A projection read failure yields
/// an empty view rather than panicking, since the view-model surface must never
/// take the daemon down; the empty view is a truthful "nothing readable right
/// now" the renderer can show.
pub(crate) fn build_graph_view(core: &DaemonCore) -> GraphView {
    let Ok(state) = core.graph.projection().state() else {
        return GraphView {
            schema_version: GRAPH_VIEW_SCHEMA_VERSION,
            nodes: Vec::new(),
            edges: Vec::new(),
            refs: Vec::new(),
        };
    };

    let nodes = state
        .nodes
        .values()
        .map(|env| NodeView {
            id: env.id,
            kind: env.kind.clone(),
            family: env.family,
            status: env.status,
            is_stale: env.is_stale,
            owns_snapshot: env.owns_snapshot,
            snapshot_hash: env.snapshot_hash.map(|h| h.to_hex()),
            branch_id: env.branch_id.clone(),
            parent_ids: env.parent_ids.clone(),
            model: env.model.clone(),
            cost: env.cost.clone().map(Into::into),
            // P7: a gate-verdict node surfaces its verdict for the canvas badge.
            // Cheap (only gate nodes fetch their payload) and best-effort.
            gate: gate_verdict_view(core, env),
            // R2 (REALIGNMENT_PLAN.md §5a): the per-type state badge (kind-gated
            // payload read, `None` until the R3 producer), a friendly line label,
            // and the line's fork origin — all additive, derived from the same
            // projection state.
            presentation_status: presentation_status_view(core, env),
            line_label: line_label_for(&env.branch_id),
            forked_from: forked_from(&state, env),
        })
        .collect();

    let edges = state
        .edges
        .iter()
        .map(|e| EdgeView {
            from: e.from,
            to: e.to,
            edge_type: e.edge_type,
        })
        .collect();

    let refs = state
        .refs
        .iter()
        .map(|(name, row)| RefView {
            name: name.clone(),
            kind: row.kind,
            target: row.target,
        })
        .collect();

    GraphView {
        schema_version: GRAPH_VIEW_SCHEMA_VERSION,
        nodes,
        edges,
        refs,
    }
}

/// The gate-verdict view for a node, or `None` if it is not a gate node (or its
/// verdict payload is absent/malformed). Only gate-kind nodes pay the payload
/// fetch (DESIGN §8.3).
fn gate_verdict_view(
    core: &DaemonCore,
    env: &spork_graph::NodeEnvelope,
) -> Option<crate::view::GateVerdictView> {
    if env.kind != crate::gate::GATE_KIND {
        return None;
    }
    let (payload, _) = core.graph.get_payload(env.id).ok()??;
    let verdict = crate::gate::verdict_from_payload(&payload)?;
    Some(verdict.into())
}

/// The per-type **presentation status** a node carries in its own payload
/// (REALIGNMENT_PLAN.md §3b), or `None`. Gated to agentic kinds (`agent-*`)
/// exactly like [`gate_verdict_view`] is gated to gate nodes, so only agentic
/// nodes pay the payload fetch. **No producer writes this field yet** — the R3
/// agent-loop emits it — so it is `None` for every node today; wiring the read
/// now keeps R3 a producer-only change (the badge flips 🟡→🟢 then).
fn presentation_status_view(core: &DaemonCore, env: &spork_graph::NodeEnvelope) -> Option<String> {
    if !env.kind.starts_with("agent-") {
        return None;
    }
    let (payload, _) = core.graph.get_payload(env.id).ok()??;
    payload
        .get("presentation_status")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// A friendly, consistent label for the emergent **line** a `branch_id` denotes
/// (REALIGNMENT_PLAN.md §1) — never user-facing git chrome. `main` stays `main`;
/// an auto-forked agent line (`agent/<ulid>`) shows `agent · <short>` so distinct
/// lines stay distinguishable; anything else echoes its id. `None` for an empty
/// id.
fn line_label_for(branch_id: &str) -> Option<String> {
    if branch_id.is_empty() {
        return None;
    }
    let label = match branch_id.strip_prefix("agent/") {
        Some(id) => format!("agent · {}", id.get(..8).unwrap_or(id)),
        None => branch_id.to_string(),
    };
    Some(label)
}

/// The node this node's **line forked from** (REALIGNMENT_PLAN.md §5a):
/// `Some(parent)` iff this node starts a new line — its `branch_id` differs from
/// its first parent's — so the canvas draws the fork connector between lanes.
/// `None` for a node continuing its parent's line, or a root node. The parent is
/// resolved from the same projection state (keyed by the id string).
fn forked_from(
    state: &spork_graph::ProjectionState,
    env: &spork_graph::NodeEnvelope,
) -> Option<Ulid> {
    let parent = env.parent_ids.first()?;
    let parent_env = state.nodes.get(&parent.to_string())?;
    (parent_env.branch_id != env.branch_id).then_some(*parent)
}

/// Compute the changed-path set of a node's snapshot tree against a baseline.
///
/// The baseline is `against`'s snapshot tree when given, else the node's first
/// parent's snapshot tree, else the empty tree (a root node's whole tree is
/// "changed"). The result is the union of paths that exist in only one side or
/// whose file bytes differ — a lazy, O(changed files) computation that reads only
/// the two trees, never materializing a worktree (DESIGN.md §14.5).
///
/// # Errors
/// - [`DaemonError::NotFound`] if the node (or `against`) does not exist or owns
///   no snapshot.
/// - [`DaemonError::Cas`] on a tree-read failure.
pub(crate) fn node_diff(
    core: &DaemonCore,
    node_id: Ulid,
    against: Option<Ulid>,
) -> Result<Vec<String>, DaemonError> {
    let node_tree = node_root_tree(core, node_id)?;

    let baseline_tree = match against {
        Some(other) => Some(node_root_tree(core, other)?),
        None => {
            // Diff against the first parent's tree if there is one.
            let env = core
                .graph
                .get_node(node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
            match env.parent_ids.first() {
                Some(parent) => Some(node_root_tree(core, *parent)?),
                None => None,
            }
        }
    };

    let mut new_files = BTreeMap::new();
    flatten_tree(&core.store, &node_tree, "", &mut new_files)?;
    let mut base_files = BTreeMap::new();
    if let Some(base) = baseline_tree {
        flatten_tree(&core.store, &base, "", &mut base_files)?;
    }

    let mut changed: Vec<String> = Vec::new();
    for (path, hash) in &new_files {
        match base_files.get(path) {
            Some(base_hash) if base_hash == hash => {}
            _ => changed.push(path.clone()),
        }
    }
    // Deletions: present in the baseline, gone in the new tree.
    for path in base_files.keys() {
        if !new_files.contains_key(path) {
            changed.push(path.clone());
        }
    }
    changed.sort();
    changed.dedup();
    Ok(changed)
}

/// Read one blob's bytes from a tree by path, fetched lazily on demand.
///
/// Walks `tree_hash` component by component to the file at `path` and reassembles
/// its blob. This is the `blob.read` lazy fetch (DESIGN.md §14.5): only the one
/// file is materialized, as the renderer opens it.
///
/// # Errors
/// - [`DaemonError::NotFound`] if the path does not resolve to a file in the tree.
/// - [`DaemonError::Cas`] on a tree/blob-read failure.
pub(crate) fn blob_read(
    store: &ObjectStore<LooseStore>,
    tree_hash: Hash,
    path: &str,
) -> Result<Vec<u8>, DaemonError> {
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if components.is_empty() {
        return Err(DaemonError::NotFound(format!(
            "empty blob path in tree {tree_hash}"
        )));
    }
    let mut current = tree_hash;
    for (i, comp) in components.iter().enumerate() {
        let tree = store
            .read_tree(&current)
            .map_err(|e| DaemonError::Cas(e.to_string()))?;
        let entry = tree
            .entries
            .iter()
            .find(|e| e.name == *comp)
            .ok_or_else(|| {
                DaemonError::NotFound(format!("path {path:?} not in tree {tree_hash}"))
            })?;
        let is_last = i == components.len() - 1;
        match entry.kind {
            EntryKind::Dir if !is_last => current = entry.target,
            EntryKind::File | EntryKind::Symlink if is_last => {
                return store
                    .read_blob(&entry.target)
                    .map_err(|e| DaemonError::Cas(e.to_string()));
            }
            _ => {
                return Err(DaemonError::NotFound(format!(
                    "path {path:?} does not resolve to a file in tree {tree_hash}"
                )))
            }
        }
    }
    Err(DaemonError::NotFound(format!(
        "path {path:?} not found in tree {tree_hash}"
    )))
}

/// Resolve a node's snapshot root tree hash (via its snapshot object).
///
/// A node must own a snapshot to be diffed/read; an observing/context node, or a
/// missing node, is a [`DaemonError::NotFound`].
fn node_root_tree(core: &DaemonCore, node_id: Ulid) -> Result<Hash, DaemonError> {
    let env = core
        .graph
        .get_node(node_id)
        .map_err(|e| DaemonError::Graph(e.to_string()))?
        .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
    let snapshot_hash = env
        .snapshot_hash
        .ok_or_else(|| DaemonError::NotFound(format!("node {node_id} owns no snapshot")))?;
    let snapshot = core
        .store
        .read_snapshot(&snapshot_hash)
        .map_err(|e| DaemonError::Cas(e.to_string()))?;
    Ok(snapshot.root_tree)
}

/// Flatten a tree into a `path -> file-blob-hash` map, recursing into subdirs.
///
/// Directories are descended; files and symlinks contribute one entry keyed by
/// their `/`-joined path. The blob hash is the comparison key for a diff (a
/// changed file has a different blob hash; an unchanged one dedups to the same
/// hash — the content-addressing identity invariant, DESIGN.md §6.1).
fn flatten_tree(
    store: &ObjectStore<LooseStore>,
    tree_hash: &Hash,
    prefix: &str,
    out: &mut BTreeMap<String, Hash>,
) -> Result<(), DaemonError> {
    let tree = store
        .read_tree(tree_hash)
        .map_err(|e| DaemonError::Cas(e.to_string()))?;
    for entry in &tree.entries {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        match entry.kind {
            EntryKind::Dir => flatten_tree(store, &entry.target, &path, out)?,
            EntryKind::File | EntryKind::Symlink => {
                out.insert(path, entry.target);
            }
        }
    }
    Ok(())
}
