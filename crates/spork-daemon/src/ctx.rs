//! P7 lineage-aware context compilation + handoff (DESIGN.md §13.2, §13.3, §13.5).
//!
//! `node.context` compiles a node's cache-aligned context from its DAG lineage
//! via the additive `spork-context` [`LineageCompiler`]: a [`LineageWalker`]
//! follows the node's ancestry (parents only — siblings are never reached,
//! Open Question 4), a budget-bounded selector folds ancestor summaries into the
//! stable prefix, and the result carries a `prefix_hash` for warm-cache reuse and
//! a `SelectionDecision` trace for the auditable "expand context" panel. The
//! target's `repo_map` layer is built from its changed files via `spork-repomap`.
//! `node.handoff` distills a node into a regenerable [`HandoffDocument`] so a
//! fresh agent can start cold (DESIGN.md §13.5). Both are **reads** (no events).

use std::collections::BTreeMap;

use spork_broker::{Capability, RequestedScope};
use spork_context::ContextCompiler;
use spork_context::ContextError;
use spork_context::{
    policy_for, ContextSource, FileTouched, HandoffGenerator, HandoffMaterials, HandoffSource,
    LineageCompiler, LineageWalker, NodeMaterials, SingleNodeHandoffGenerator,
};
use spork_ipc::CommandResult;
use spork_repomap::{KeywordSymbolExtractor, RepoMap, DEFAULT_REPO_MAP_TOKENS};
use ulid::Ulid;

use crate::core::{Daemon, DaemonCore, WORKTREE_GLOB};
use crate::error::DaemonError;
use crate::read::blob_read;

/// The system prompt the daemon seeds a compiled context with (rank 0, stable).
const SYSTEM_PROMPT: &str =
    "You are a Spork coding agent operating on a branching work-DAG of typed nodes.";

/// The maximum ancestors the daemon walks when compiling a node's context.
const MAX_LINEAGE_DEPTH: usize = 64;

impl Daemon {
    /// `node.context` (read): compile a node's lineage-aware context, returning the
    /// ordered layers, the stable `prefix_hash`, and the selection trace inline
    /// (DESIGN.md §13.2, §13.6).
    pub(crate) fn cmd_node_context(&self, node_id: Ulid) -> Result<CommandResult, DaemonError> {
        // Compiling reads the node's (and ancestors') snapshot/payload state.
        self.authorize(
            Capability::SnapshotRead,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        // Gather materials for the target + its ancestors under one lock, then
        // compile offline (the compiler's traits are pure, no locking).
        let (kind, materials, ancestors) = {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            let env = core
                .graph
                .get_node(node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
            let kind = env.kind.clone();
            let ancestors = ancestors_of(&core, node_id);
            let mut materials: BTreeMap<Ulid, NodeMaterials> = BTreeMap::new();
            materials.insert(node_id, node_materials(&core, node_id, true));
            for anc in &ancestors {
                materials.insert(*anc, node_materials(&core, *anc, false));
            }
            (kind, materials, ancestors)
        };

        let compiler = LineageCompiler::new(
            MemSource { materials },
            MemWalker {
                target: node_id,
                ancestors,
            },
        );
        let cc = compiler
            .compile(node_id, &policy_for(&kind))
            .map_err(map_ctx_err)?;

        Ok(CommandResult::Read {
            data: serde_json::json!({
                "nodeId": node_id.to_string(),
                "prefixHash": cc.prefix_hash.to_hex(),
                "layerCount": cc.layers.len(),
                "totalTokens": cc.total_token_estimate(),
                "layers": cc.layers,
                "selectionTrace": cc.selection_trace,
            }),
        })
    }

    /// `node.handoff` (read): distill a node into a regenerable handoff document so
    /// a fresh agent can continue the branch cold (DESIGN.md §13.5).
    pub(crate) fn cmd_node_handoff(&self, node_id: Ulid) -> Result<CommandResult, DaemonError> {
        self.authorize(
            Capability::SnapshotRead,
            &RequestedScope::path(WORKTREE_GLOB),
        )?;

        let materials = {
            let core = self.core.lock().expect("daemon core mutex poisoned");
            let env = core
                .graph
                .get_node(node_id)
                .map_err(|e| DaemonError::Graph(e.to_string()))?
                .ok_or_else(|| DaemonError::NotFound(format!("node {node_id}")))?;
            let (summary, files) = summary_and_files(&core, node_id);
            HandoffMaterials {
                summary,
                key_decisions: Vec::new(),
                files_touched: files
                    .into_iter()
                    .map(|p| FileTouched::new(p, "changed in this node"))
                    .collect(),
                open_threads: Vec::new(),
                constraints: Vec::new(),
                test_state: "see attached checks".to_string(),
                lineage_hash: env.lineage_hash,
            }
        };

        let generator = SingleNodeHandoffGenerator::new(FixedHandoff {
            node: node_id,
            materials,
        });
        let doc = generator.generate(node_id).map_err(map_ctx_err)?;

        Ok(CommandResult::Read {
            data: serde_json::to_value(&doc)
                .map_err(|e| DaemonError::Graph(format!("encode handoff: {e}")))?,
        })
    }
}

/// The ancestors of a node, nearest-first, bounded by [`MAX_LINEAGE_DEPTH`].
fn ancestors_of(core: &DaemonCore, node_id: Ulid) -> Vec<Ulid> {
    let Ok(state) = core.graph.projection().state() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    seen.insert(node_id);
    let mut frontier: std::collections::VecDeque<Ulid> = state
        .nodes
        .get(&node_id.to_string())
        .map(|e| e.parent_ids.iter().copied().collect())
        .unwrap_or_default();
    while out.len() < MAX_LINEAGE_DEPTH {
        let Some(cur) = frontier.pop_front() else {
            break;
        };
        if !seen.insert(cur) {
            continue;
        }
        out.push(cur);
        if let Some(env) = state.nodes.get(&cur.to_string()) {
            frontier.extend(env.parent_ids.iter().copied());
        }
    }
    out
}

/// Build a node's raw context materials from its envelope + payload.
fn node_materials(core: &DaemonCore, node_id: Ulid, is_target: bool) -> NodeMaterials {
    let (summary, files) = summary_and_files(core, node_id);
    let repo_map = if is_target {
        node_repo_map(core, node_id, &files)
    } else {
        None
    };
    NodeMaterials {
        system_prompt: is_target.then(|| SYSTEM_PROMPT.to_string()),
        repo_map,
        current_diff: (!summary.is_empty()).then_some(summary),
        ..Default::default()
    }
}

/// Extract a node's distilled summary and changed-file list from its payload.
fn summary_and_files(core: &DaemonCore, node_id: Ulid) -> (String, Vec<String>) {
    let Ok(Some((payload, _))) = core.graph.get_payload(node_id) else {
        return (String::new(), Vec::new());
    };
    let summary = payload
        .get("diff_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let files = payload
        .get("files_changed")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (summary, files)
}

/// Build a node's `repo_map` layer from its changed files via `spork-repomap`.
///
/// Reads each changed file's content from the node's snapshot tree and runs the
/// PageRank repo map over them, bounded to the default token budget. Returns
/// `None` when the node owns no snapshot or has no readable changed files.
fn node_repo_map(core: &DaemonCore, node_id: Ulid, files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let env = core.graph.get_node(node_id).ok()??;
    let snapshot_hash = env.snapshot_hash?;
    let snapshot = core.store.read_snapshot(&snapshot_hash).ok()?;
    let mut sources: Vec<(String, String)> = Vec::new();
    for path in files {
        if let Ok(bytes) = blob_read(&core.store, snapshot.root_tree, path) {
            if let Ok(text) = String::from_utf8(bytes) {
                sources.push((path.clone(), text));
            }
        }
    }
    if sources.is_empty() {
        return None;
    }
    let map = RepoMap::build(&sources, &KeywordSymbolExtractor::new());
    let rendered = map.render(DEFAULT_REPO_MAP_TOKENS);
    (!rendered.is_empty()).then_some(rendered)
}

/// Map a [`ContextError`] to a [`DaemonError`].
fn map_ctx_err(e: ContextError) -> DaemonError {
    match e {
        ContextError::NodeNotFound(s) => DaemonError::NotFound(s),
        other => DaemonError::Graph(other.to_string()),
    }
}

/// An in-memory [`ContextSource`] over a pre-gathered materials map.
struct MemSource {
    materials: BTreeMap<Ulid, NodeMaterials>,
}
impl ContextSource for MemSource {
    fn node_materials(&self, node: Ulid) -> Result<Option<NodeMaterials>, ContextError> {
        Ok(self.materials.get(&node).cloned())
    }
}

/// An in-memory [`LineageWalker`] returning the pre-walked ancestry of one target.
struct MemWalker {
    target: Ulid,
    ancestors: Vec<Ulid>,
}
impl LineageWalker for MemWalker {
    fn ancestors(&self, node: Ulid) -> Result<Vec<Ulid>, ContextError> {
        Ok(if node == self.target {
            self.ancestors.clone()
        } else {
            Vec::new()
        })
    }
}

/// A one-node [`HandoffSource`] over pre-gathered materials.
struct FixedHandoff {
    node: Ulid,
    materials: HandoffMaterials,
}
impl HandoffSource for FixedHandoff {
    fn handoff_materials(&self, node: Ulid) -> Result<Option<HandoffMaterials>, ContextError> {
        Ok((node == self.node).then(|| self.materials.clone()))
    }
}
