//! The read-only Lineage/History **MCP server** (DESIGN.md §13.7).
//!
//! Spork exposes the [`HistoryIndex`] as a first-class, **read-only**,
//! capability-gated (`nodes.readOutputs`, lineage-only) MCP server — the external
//! "drive Spork from Claude Code / Cursor" surface and the in-process lineage
//! tool. It speaks JSON-RPC 2.0 (`initialize`, `tools/list`, `tools/call`) over
//! stdio. The request handler [`HistoryMcpServer::handle_request`] is pure
//! (request `Value` → optional response `Value`) so it is fully unit-testable
//! without real stdio; [`HistoryMcpServer::serve`] is the thin line loop on top.
//!
//! Read-only by construction (no tool mutates state) and **auto lineage-scoped**
//! (the default scope is the anchor's lineage, so a query never silently leaks
//! sibling-branch context). The `nodes.readOutputs` capability is checked here so
//! a server constructed without the grant refuses every `tools/call`.

use std::io::{BufRead, Write};

use serde_json::{json, Value};
use ulid::Ulid;

use crate::index::{HistoryIndex, HistorySource, Scope, SearchKind};

/// The MCP protocol version this server advertises.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// The six read-only Lineage/History tools (DESIGN.md §13.7).
const TOOL_NAMES: [&str; 6] = [
    "search_history",
    "get_node_transcript",
    "walk_ancestors",
    "find_decisions",
    "find_files_touched",
    "get_handoff",
];

/// The read-only Lineage/History MCP server (DESIGN.md §13.7).
pub struct HistoryMcpServer<S: HistorySource> {
    index: HistoryIndex<S>,
    anchor: Option<Ulid>,
    capability_granted: bool,
}

impl<S: HistorySource> HistoryMcpServer<S> {
    /// Construct a server over an index.
    ///
    /// `anchor` is the node the agent is reasoning from — it provides the default
    /// lineage scope so queries are auto lineage-scoped. `capability_granted`
    /// records whether the daemon authorized `nodes.readOutputs`; when `false`,
    /// every `tools/call` is refused.
    #[must_use]
    pub fn new(index: HistoryIndex<S>, anchor: Option<Ulid>, capability_granted: bool) -> Self {
        HistoryMcpServer {
            index,
            anchor,
            capability_granted,
        }
    }

    /// Handle one JSON-RPC request, returning the response (or `None` for a
    /// notification, which has no `id`).
    #[must_use]
    pub fn handle_request(&self, req: &Value) -> Option<Value> {
        // Notifications (no `id`) get no response.
        let id = req.get("id").cloned()?;
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "initialize" => Ok(self.initialize_result()),
            "tools/list" => Ok(self.tools_list_result()),
            "tools/call" => self.tools_call(req.get("params")),
            other => Err(rpc_error(-32601, &format!("method not found: {other}"))),
        };
        Some(match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        })
    }

    /// Handle one line of JSON-RPC input, returning the serialized response line
    /// (or `None` for a notification / unparseable line).
    #[must_use]
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let req: Value = serde_json::from_str(line).ok()?;
        let resp = self.handle_request(&req)?;
        serde_json::to_string(&resp).ok()
    }

    /// Serve JSON-RPC over a reader/writer line loop (newline-delimited).
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from the reader/writer.
    pub fn serve<R: BufRead, W: Write>(&self, reader: R, mut writer: W) -> std::io::Result<()> {
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(resp) = self.handle_line(&line) {
                writeln!(writer, "{resp}")?;
                writer.flush()?;
            }
        }
        Ok(())
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "spork-history", "version": env!("CARGO_PKG_VERSION") },
        })
    }

    fn tools_list_result(&self) -> Value {
        let scope_enum = json!(["lineage", "branch", "project"]);
        let node_arg = json!({ "type": "string", "description": "Node id (ULID)" });
        let tools: Vec<Value> = TOOL_NAMES
            .iter()
            .map(|name| match *name {
                "search_history" => json!({
                    "name": name,
                    "description": "Search transcripts/DAG by pattern, scoped to lineage (default), branch, or project.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "pattern": { "type": "string" },
                            "kind": { "type": "string", "enum": ["text", "regex", "structured"] },
                            "scope": { "type": "string", "enum": scope_enum },
                            "node": node_arg,
                        },
                        "required": ["pattern"],
                    },
                }),
                "get_node_transcript" | "walk_ancestors" | "get_handoff" => json!({
                    "name": name,
                    "description": "Read a node's transcript / lineage / handoff (read-only).",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "nodeId": node_arg },
                        "required": ["nodeId"],
                    },
                }),
                _ => json!({
                    "name": name,
                    "description": "Surface decisions / files-touched across a scope (default lineage).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "scope": { "type": "string", "enum": scope_enum },
                            "node": node_arg,
                        },
                    },
                }),
            })
            .collect();
        json!({ "tools": tools })
    }

    fn tools_call(&self, params: Option<&Value>) -> Result<Value, Value> {
        if !self.capability_granted {
            return Err(rpc_error(
                -32001,
                "capability nodes.readOutputs not granted (the Lineage/History MCP is read-only and gated)",
            ));
        }
        let params = params.ok_or_else(|| rpc_error(-32602, "missing params"))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_error(-32602, "missing tool name"))?;
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let payload = self.dispatch_tool(name, &args)?;
        // MCP tool results wrap content blocks; we return the data as a JSON text
        // block so any MCP client can consume it.
        Ok(json!({
            "content": [{ "type": "text", "text": payload.to_string() }],
            "isError": false,
        }))
    }

    fn dispatch_tool(&self, name: &str, args: &Value) -> Result<Value, Value> {
        match name {
            "search_history" => {
                let pattern = arg_str(args, "pattern")?;
                let kind = parse_kind(args.get("kind").and_then(Value::as_str));
                let scope = parse_scope(args.get("scope").and_then(Value::as_str));
                let anchor = self.anchor_for(args);
                let hits = self.index.search_history(pattern, kind, scope, anchor);
                Ok(serde_json::to_value(hits).unwrap_or_else(|_| json!([])))
            }
            "get_node_transcript" => {
                let node = arg_node(args)?;
                Ok(json!({ "transcript": self.index.get_node_transcript(node) }))
            }
            "walk_ancestors" => {
                let node = arg_node(args)?;
                Ok(serde_json::to_value(self.index.walk_ancestors(node)).unwrap_or(json!([])))
            }
            "find_decisions" => {
                let scope = parse_scope(args.get("scope").and_then(Value::as_str));
                Ok(
                    serde_json::to_value(self.index.find_decisions(scope, self.anchor_for(args)))
                        .unwrap_or(json!([])),
                )
            }
            "find_files_touched" => {
                let scope = parse_scope(args.get("scope").and_then(Value::as_str));
                Ok(serde_json::to_value(
                    self.index.find_files_touched(scope, self.anchor_for(args)),
                )
                .unwrap_or(json!([])))
            }
            "get_handoff" => {
                let node = arg_node(args)?;
                Ok(json!({ "handoff": self.index.get_handoff(node) }))
            }
            other => Err(rpc_error(-32602, &format!("unknown tool: {other}"))),
        }
    }

    /// Resolve the anchor for a scoped query: an explicit `node` arg overrides the
    /// server's anchor.
    fn anchor_for(&self, args: &Value) -> Option<Ulid> {
        args.get("node")
            .and_then(Value::as_str)
            .and_then(|s| Ulid::from_string(s).ok())
            .or(self.anchor)
    }
}

fn parse_kind(s: Option<&str>) -> SearchKind {
    match s {
        Some("regex") => SearchKind::Regex,
        Some("structured") => SearchKind::Structured,
        _ => SearchKind::Text,
    }
}

fn parse_scope(s: Option<&str>) -> Scope {
    match s {
        Some("branch") => Scope::Branch,
        Some("project") => Scope::Project,
        // Default and "lineage" both scope to lineage (auto lineage-scoped).
        _ => Scope::Lineage,
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, Value> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_error(-32602, &format!("missing string arg: {key}")))
}

fn arg_node(args: &Value) -> Result<Ulid, Value> {
    let s = arg_str(args, "nodeId")?;
    Ulid::from_string(s).map_err(|_| rpc_error(-32602, "nodeId is not a valid ULID"))
}

fn rpc_error(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{HistoryNode, HistorySource};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemSource {
        nodes: BTreeMap<Ulid, HistoryNode>,
        parents: BTreeMap<Ulid, Vec<Ulid>>,
        transcripts: BTreeMap<Ulid, String>,
    }
    impl HistorySource for MemSource {
        fn node(&self, id: Ulid) -> Option<HistoryNode> {
            self.nodes.get(&id).cloned()
        }
        fn ancestors(&self, id: Ulid) -> Vec<Ulid> {
            self.parents.get(&id).cloned().unwrap_or_default()
        }
        fn all_nodes(&self) -> Vec<Ulid> {
            self.nodes.keys().copied().collect()
        }
        fn transcript(&self, id: Ulid) -> Option<String> {
            self.transcripts.get(&id).cloned()
        }
        fn handoff(&self, _id: Ulid) -> Option<String> {
            None
        }
    }

    fn server(granted: bool) -> (HistoryMcpServer<MemSource>, Ulid, Ulid) {
        let root = Ulid::new();
        let tip = Ulid::new();
        let mut s = MemSource::default();
        s.nodes.insert(
            root,
            HistoryNode {
                id: root,
                kind: "codebase-edit".into(),
                branch_id: "main".into(),
                summary: Some("added retry".into()),
                files_touched: vec!["src/client.rs".into()],
                decisions: vec!["use backoff".into()],
            },
        );
        s.nodes.insert(
            tip,
            HistoryNode {
                id: tip,
                kind: "codebase-edit".into(),
                branch_id: "main".into(),
                summary: None,
                files_touched: vec![],
                decisions: vec![],
            },
        );
        s.parents.insert(tip, vec![root]);
        s.transcripts
            .insert(root, "we discussed retry and backoff".into());
        (
            HistoryMcpServer::new(HistoryIndex::new(s), Some(tip), granted),
            root,
            tip,
        )
    }

    #[test]
    fn initialize_returns_server_info() {
        let (srv, _root, _tip) = server(true);
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" });
        let resp = srv.handle_request(&req).unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "spork-history");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_advertises_six_read_only_tools() {
        let (srv, _r, _t) = server(true);
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = srv.handle_request(&req).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"search_history"));
        assert!(names.contains(&"get_node_transcript"));
        assert!(names.contains(&"walk_ancestors"));
    }

    #[test]
    fn search_history_tool_call_returns_hits() {
        let (srv, _root, _tip) = server(true);
        let req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "search_history", "arguments": { "pattern": "backoff" } }
        });
        let resp = srv.handle_request(&req).unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("backoff") || text.contains("transcript") || text.contains("root"));
        assert_eq!(resp["result"]["isError"], false);
    }

    #[test]
    fn capability_gate_refuses_when_not_granted() {
        let (srv, _root, _tip) = server(false);
        let req = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "search_history", "arguments": { "pattern": "x" } }
        });
        let resp = srv.handle_request(&req).unwrap();
        assert_eq!(resp["error"]["code"], -32001);
        assert!(resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nodes.readOutputs"));
    }

    #[test]
    fn walk_ancestors_tool_returns_chain() {
        let (srv, root, tip) = server(true);
        let req = json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "walk_ancestors", "arguments": { "nodeId": tip.to_string() } }
        });
        let resp = srv.handle_request(&req).unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(&root.to_string()));
        assert!(text.contains(&tip.to_string()));
    }

    #[test]
    fn unknown_method_is_error() {
        let (srv, _r, _t) = server(true);
        let req = json!({ "jsonrpc": "2.0", "id": 6, "method": "nope" });
        let resp = srv.handle_request(&req).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn notification_gets_no_response() {
        let (srv, _r, _t) = server(true);
        let req = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(srv.handle_request(&req).is_none());
    }

    #[test]
    fn handle_line_round_trips() {
        let (srv, _r, _t) = server(true);
        let line = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
        let out = srv.handle_line(line).unwrap();
        assert!(out.contains("search_history"));
        // A garbage line yields no response, never a panic.
        assert!(srv.handle_line("not json").is_none());
    }

    #[test]
    fn serve_processes_a_stream() {
        let (srv, _r, _t) = server(true);
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n\n";
        let mut out: Vec<u8> = Vec::new();
        srv.serve(std::io::Cursor::new(input), &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("search_history"));
    }
}
