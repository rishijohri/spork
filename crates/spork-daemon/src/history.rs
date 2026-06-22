//! P7 read-only Lineage/History MCP query surface (DESIGN.md §13.7, §4.7).
//!
//! `history.query` answers a JSON-RPC request against the read-only
//! [`HistoryMcpServer`] backed by a snapshot of the DAG projection and the
//! content-addressed transcripts. It is **read-only** and capability-gated on
//! [`Capability::NodesReadOutputs`](spork_broker::Capability::NodesReadOutputs)
//! (granted opt-in via
//! [`DaemonBuilder::grant_model_access`](crate::DaemonBuilder::grant_model_access)),
//! introduces **no** schema change, and is auto lineage-scoped so a query never
//! leaks sibling-branch context (DESIGN.md §13.7). The graph is snapshotted into
//! an in-memory [`HistorySource`] per query so the index runs without holding the
//! core lock.

use std::collections::BTreeMap;

use spork_broker::{Capability, RequestedScope};
use spork_history::{HistoryIndex, HistoryMcpServer, HistoryNode, HistorySource};
use spork_ipc::CommandResult;
use ulid::Ulid;

use crate::core::Daemon;
use crate::error::DaemonError;

impl Daemon {
    /// `history.query` (read): answer a JSON-RPC request against the read-only
    /// Lineage/History MCP server (DESIGN.md §13.7).
    pub(crate) fn cmd_history_query(
        &self,
        request: serde_json::Value,
    ) -> Result<CommandResult, DaemonError> {
        // The History MCP is gated on nodes.readOutputs (lineage-only, read-only).
        self.authorize(Capability::NodesReadOutputs, &RequestedScope::none())?;

        let source = self.snapshot_history_source()?;
        // Already authorized above, so the server runs with the capability granted;
        // it remains read-only and auto lineage-scoped by construction.
        let server = HistoryMcpServer::new(HistoryIndex::new(source), None, true);
        let response = server
            .handle_request(&request)
            .unwrap_or(serde_json::Value::Null);
        Ok(CommandResult::Read { data: response })
    }

    /// Snapshot the DAG projection + transcripts into an in-memory
    /// [`HistorySource`] (so the index runs lock-free).
    fn snapshot_history_source(&self) -> Result<MemHistorySource, DaemonError> {
        let core = self.core.lock().expect("daemon core mutex poisoned");
        let state = core
            .graph
            .projection()
            .state()
            .map_err(|e| DaemonError::Graph(e.to_string()))?;

        let mut nodes: BTreeMap<Ulid, HistoryNode> = BTreeMap::new();
        let mut parents: BTreeMap<Ulid, Vec<Ulid>> = BTreeMap::new();
        let mut transcripts: BTreeMap<Ulid, String> = BTreeMap::new();
        let mut handoffs: BTreeMap<Ulid, String> = BTreeMap::new();

        for env in state.nodes.values() {
            let (summary, files) = payload_summary_files(&core, env.id);
            nodes.insert(
                env.id,
                HistoryNode {
                    id: env.id,
                    kind: env.kind.clone(),
                    branch_id: env.branch_id.clone(),
                    summary: (!summary.is_empty()).then(|| summary.clone()),
                    files_touched: files,
                    // v1 decisions are not separately stored; the summary stands
                    // in for "what this node did" until a richer source lands.
                    decisions: Vec::new(),
                },
            );
            parents.insert(env.id, env.parent_ids.clone());
            // The canonical transcript is the conversation blob, if the node bound
            // one (read from the single content-addressed store, never duplicated).
            if let Some(t) = read_conversation(&core, env.id) {
                transcripts.insert(env.id, t);
            }
            // The handoff stand-in is the distilled summary (a real generator is
            // an additive richer source; this keeps get_handoff answering).
            if !summary.is_empty() {
                handoffs.insert(env.id, summary);
            }
        }

        Ok(MemHistorySource {
            nodes,
            parents,
            transcripts,
            handoffs,
        })
    }
}

/// Read a node's distilled summary + changed files from its payload.
fn payload_summary_files(core: &crate::core::DaemonCore, id: Ulid) -> (String, Vec<String>) {
    let Ok(Some((payload, _))) = core.graph.get_payload(id) else {
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

/// Read a node's bound conversation transcript from the CAS, if present.
fn read_conversation(core: &crate::core::DaemonCore, id: Ulid) -> Option<String> {
    let (payload, _) = core.graph.get_payload(id).ok()??;
    let conv_hex = payload.get("conversation_ref")?.as_str()?;
    let hash = spork_hash::Hash::from_hex(conv_hex).ok()?;
    let bytes = core.store.read_blob(&hash).ok()?;
    String::from_utf8(bytes).ok()
}

/// An in-memory [`HistorySource`] snapshot of the DAG + transcripts.
struct MemHistorySource {
    nodes: BTreeMap<Ulid, HistoryNode>,
    parents: BTreeMap<Ulid, Vec<Ulid>>,
    transcripts: BTreeMap<Ulid, String>,
    handoffs: BTreeMap<Ulid, String>,
}

impl HistorySource for MemHistorySource {
    fn node(&self, id: Ulid) -> Option<HistoryNode> {
        self.nodes.get(&id).cloned()
    }
    fn ancestors(&self, id: Ulid) -> Vec<Ulid> {
        // Transitive, nearest-first, over the snapshotted parent map.
        let mut out = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(id);
        let mut frontier: std::collections::VecDeque<Ulid> =
            self.parents.get(&id).cloned().unwrap_or_default().into();
        while let Some(cur) = frontier.pop_front() {
            if !seen.insert(cur) {
                continue;
            }
            out.push(cur);
            if let Some(ps) = self.parents.get(&cur) {
                frontier.extend(ps.iter().copied());
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
