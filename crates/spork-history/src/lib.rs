//! Spork P7 read-only Lineage/History exploration (DESIGN.md §13.7, §4.7).
//!
//! An agent (or a human) often needs to reach back across the whole work-DAG —
//! "where was this decided", "what touched this file on this lineage", "show me
//! that node's transcript". Spork exposes this as a first-class, **read-only**,
//! capability-gated (`nodes.readOutputs`, lineage-only) MCP server backed by the
//! content-addressed transcripts and the DAG projection, **auto lineage-scoped**
//! so a query never silently leaks sibling-branch context. It is the external
//! "drive Spork from Claude Code / Cursor" surface and introduces **no** schema
//! change — it reads existing data (CLAUDE.md C2/C3).
//!
//! - [`HistoryIndex`] — the v1 index with the six query methods, over a
//!   [`HistorySource`] seam (graph-backed in the daemon, in-memory in tests).
//! - [`HistoryMcpServer`] — the JSON-RPC 2.0 server wrapping the index, with a
//!   pure [`HistoryMcpServer::handle_request`] for offline testing and a thin
//!   [`HistoryMcpServer::serve`] stdio loop.
//! - [`regex_is_match`] — a dependency-free regex subset powering `search_history`'s
//!   `regex` mode.
//!
//! # Example
//!
//! ```
//! use spork_history::{HistoryIndex, HistoryMcpServer};
//! # use spork_history::{HistoryNode, HistorySource};
//! # use ulid::Ulid;
//! # use std::collections::BTreeMap;
//! # #[derive(Default)]
//! # struct Src { nodes: BTreeMap<Ulid, HistoryNode>, t: BTreeMap<Ulid, String> }
//! # impl HistorySource for Src {
//! #   fn node(&self, id: Ulid) -> Option<HistoryNode> { self.nodes.get(&id).cloned() }
//! #   fn ancestors(&self, _id: Ulid) -> Vec<Ulid> { vec![] }
//! #   fn all_nodes(&self) -> Vec<Ulid> { self.nodes.keys().copied().collect() }
//! #   fn transcript(&self, id: Ulid) -> Option<String> { self.t.get(&id).cloned() }
//! #   fn handoff(&self, _id: Ulid) -> Option<String> { None }
//! # }
//! # let id = Ulid::new();
//! # let mut src = Src::default();
//! # src.nodes.insert(id, HistoryNode { id, kind: "codebase-edit".into(), branch_id: "main".into(), summary: None, files_touched: vec![], decisions: vec![] });
//! # src.t.insert(id, "we discussed retry and backoff".into());
//! let server = HistoryMcpServer::new(HistoryIndex::new(src), Some(id), /* nodes.readOutputs */ true);
//! let req = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
//! let resp = server.handle_request(&req).unwrap();
//! assert!(resp["result"]["tools"].as_array().unwrap().len() == 6);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod index;
mod mcp;
mod regex;

pub use index::{
    DecisionHit, FileTouchHit, HistoryIndex, HistoryNode, HistorySource, Scope, SearchHit,
    SearchKind,
};
pub use mcp::HistoryMcpServer;
pub use regex::regex_is_match;
