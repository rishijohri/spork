//! P6 Definition-of-Done integration tests: `node.agentRun` end to end through
//! the daemon's frozen [`CommandHandler`](spork_ipc::CommandHandler) seam
//! (DESIGN.md §6.6, §12.1, §12.3, §12.5).
//!
//! These exercise the P6 daemon wiring fully **offline** — no real model, no
//! network beyond loopback — proving:
//!
//! - a read-only agent run invokes a model over a real transport (a CLI
//!   subprocess and a loopback local HTTP server), and **attaches** the answer as
//!   a [`Family::Context`] node linked to the target by a *dotted*
//!   `DerivedFrom` edge — the target is never mutated and no branch is forked
//!   (DESIGN.md §6.6);
//! - the attached node records the resolved `model` and the priced `cost`, and
//!   `graph_view` surfaces both (per-node attribution; the per-branch ledger is
//!   the sum over the branch);
//! - a `local_only` node is **refused** a cloud provider before any byte leaves
//!   (privacy enforced at resolve, DESIGN.md §12.5);
//! - model invocation stays **denied by default** — without an explicit
//!   `grant_model_access` the run is a capability denial (DESIGN.md §15.1);
//! - the same node hot-swaps between two providers (CLI ↔ local), each priced
//!   independently (the DoD hot-swap).

use std::io::{Read, Write};
use std::net::TcpListener;

use spork_daemon::{
    AgentConfig, AgentRunIntent, Command, CommandHandler, CommandResult, Daemon, EdgeType, Family,
    GraphView,
};
use ulid::Ulid;

/// A daemon with model access granted and a configurable agent transport set.
struct Harness {
    daemon: Daemon,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A daemon whose agent transports are `config`, with model access granted.
    fn with_config(config: AgentConfig) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::builder(dir.path())
            .grant_model_access()
            .with_agent_config(config)
            .build()
            .unwrap();
        Harness { daemon, _dir: dir }
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.daemon.workdir().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    /// Capture the working tree into a snapshot node (the agent-run target).
    fn create_target(&self) -> Ulid {
        let (snapshot_hash, _root) = self.daemon.capture_working_tree().unwrap();
        let result = self
            .daemon
            .dispatch(Command::NodeCreate {
                kind: "snapshot".into(),
                type_version: "1.0.0".into(),
                parent_ids: vec![],
                branch_id: "main".into(),
                payload: serde_json::json!({ "schema_version": 1, "origin": "manual" }),
                owns_snapshot: true,
                snapshot_hash: Some(snapshot_hash),
            })
            .unwrap();
        node_id_of(&result)
    }
}

fn node_id_of(result: &CommandResult) -> Ulid {
    match result {
        CommandResult::Mutation { ids, .. } => {
            Ulid::from_string(ids["nodeId"].as_str().expect("nodeId")).unwrap()
        }
        other => panic!("expected Mutation, got {other:?}"),
    }
}

fn ids_of(result: &CommandResult) -> serde_json::Value {
    match result {
        CommandResult::Mutation { ids, .. } => ids.clone(),
        other => panic!("expected Mutation, got {other:?}"),
    }
}

/// Write an executable CLI-agent shell script that ignores stdin and prints a
/// fixed JSONL response (an assistant line + a usage block).
fn write_cli_agent(dir: &std::path::Path) -> String {
    let path = dir.join("agent.sh");
    let body = "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"lines\":[{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"the answer\"}]}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4}}'\n";
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().to_string()
}

/// Spawn a one-shot loopback HTTP server returning an OpenAI-compatible response
/// (with a usage block), returning its `http://127.0.0.1:<port>/...` endpoint.
fn serve_local_openai() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut socket, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf);
            let body = "{\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"local answer\"}}],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20}}";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(resp.as_bytes());
            let _ = socket.flush();
        }
    });
    format!("http://{addr}/v1/chat/completions")
}

/// Find the context node attached to `target` by a `DerivedFrom` edge.
fn attached_context(view: &GraphView, target: Ulid) -> Option<&spork_daemon::NodeView> {
    let edge = view
        .edges
        .iter()
        .find(|e| e.to == target && e.edge_type == EdgeType::DerivedFrom)?;
    view.node(edge.from)
}

#[test]
fn cli_agent_run_attaches_a_context_node_with_model_and_cost() {
    let dir = tempfile::tempdir().unwrap();
    let program = write_cli_agent(dir.path());
    let h = Harness::with_config(AgentConfig {
        local_endpoint: None,
        cli_command: Some((program, vec![])),
    });
    h.write("src/lib.rs", "fn ok() {}\n");
    let target = h.create_target();

    let result = h
        .daemon
        .dispatch(Command::NodeAgentRun {
            target_node_id: target,
            prompt: "what does this do?".into(),
            model_key: "cli/copilot-cli".into(),
            privacy: "any".into(),
            intent: AgentRunIntent::Ask,
        })
        .unwrap();

    let ids = ids_of(&result);
    assert_eq!(ids["provider"], "cli");
    assert_eq!(ids["model"], "cli/copilot-cli");
    assert_eq!(ids["costMicroUsd"], 0); // a CLI agent is free at the seam
    assert_eq!(ids["inputTokens"], 12); // tokens still attributed from usage
    assert_eq!(ids["outputTokens"], 4);
    let ctx_id = node_id_of(&result);

    // The attached node is a CONTEXT node, dotted (DerivedFrom) to the target,
    // owning no snapshot — a read-only attach, no branch forked (DESIGN §6.6).
    let view = h.daemon.graph_view();
    let ctx = view.node(ctx_id).expect("context node in view");
    assert_eq!(ctx.family, Family::Context);
    assert!(!ctx.owns_snapshot);
    assert_eq!(ctx.model.as_deref(), Some("cli/copilot-cli"));
    let cost = ctx.cost.expect("cost recorded on the node");
    assert_eq!(cost.input_tokens, 12);
    assert_eq!(cost.output_tokens, 4);
    assert_eq!(cost.micro_usd, 0);

    // The edge to the target is the dotted DerivedFrom attachment, and there is
    // NO solid PARENT_CHILD lineage edge between them (it is an attach, not a fork).
    assert_eq!(attached_context(&view, target).map(|n| n.id), Some(ctx_id));
    assert!(
        !view
            .edges
            .iter()
            .any(|e| e.from == ctx_id && e.to == target && e.edge_type == EdgeType::ParentChild),
        "a read-only attach must not create a solid lineage edge"
    );

    // The target itself is untouched (still a snapshot-owning mutating node).
    let target_view = view.node(target).unwrap();
    assert!(target_view.owns_snapshot);
    assert_eq!(target_view.family, Family::Mutating);
}

#[test]
fn local_http_agent_run_prices_through_the_loopback_server() {
    let endpoint = serve_local_openai();
    let h = Harness::with_config(AgentConfig {
        local_endpoint: Some(endpoint),
        cli_command: None,
    });
    h.write("src/lib.rs", "fn ok() {}\n");
    let target = h.create_target();

    let result = h
        .daemon
        .dispatch(Command::NodeAgentRun {
            target_node_id: target,
            prompt: "summarize".into(),
            model_key: "local/llama3.1".into(),
            privacy: "local_only".into(), // a local model satisfies local_only
            intent: AgentRunIntent::Analysis,
        })
        .unwrap();
    let ids = ids_of(&result);
    assert_eq!(ids["provider"], "local");
    assert_eq!(ids["inputTokens"], 100);
    assert_eq!(ids["outputTokens"], 20);
    assert_eq!(ids["costMicroUsd"], 0); // local is free

    let view = h.daemon.graph_view();
    let ctx = attached_context(&view, target).expect("attached context");
    assert_eq!(ctx.family, Family::Context);
    assert_eq!(ctx.cost.map(|c| c.input_tokens), Some(100));
}

#[test]
fn local_only_node_is_refused_a_cloud_provider() {
    let dir = tempfile::tempdir().unwrap();
    let program = write_cli_agent(dir.path());
    let h = Harness::with_config(AgentConfig {
        local_endpoint: None,
        cli_command: Some((program, vec![])),
    });
    h.write("src/lib.rs", "fn ok() {}\n");
    let target = h.create_target();
    let before = h.daemon.graph_view().nodes.len();

    // local_only + a first-party cloud model → refused at resolve, before any
    // transport is consulted (privacy enforced first, DESIGN §12.5).
    let err = h
        .daemon
        .dispatch(Command::NodeAgentRun {
            target_node_id: target,
            prompt: "leak this".into(),
            model_key: "openai/gpt-4o".into(),
            privacy: "local_only".into(),
            intent: AgentRunIntent::Ask,
        })
        .unwrap_err();
    // It is refused *for privacy* specifically (not some unrelated failure): the
    // error names the privacy class / forbidden provider, proving enforcement at
    // resolve before any transport is consulted (DESIGN §12.5).
    let msg = err.to_string();
    assert!(
        msg.contains("local_only") || msg.contains("privacy"),
        "expected a privacy refusal, got: {msg}"
    );
    assert_eq!(
        h.daemon.graph_view().nodes.len(),
        before,
        "a refused run attaches no node"
    );
}

#[test]
fn agent_run_is_denied_without_model_access_grant() {
    // A daemon WITHOUT grant_model_access (the default deny-by-default posture).
    let dir = tempfile::tempdir().unwrap();
    let program = write_cli_agent(dir.path());
    let daemon = Daemon::builder(dir.path())
        .with_agent_config(AgentConfig {
            local_endpoint: None,
            cli_command: Some((program, vec![])),
        })
        .build()
        .unwrap();
    std::fs::write(daemon.workdir().join("a.txt"), "x").unwrap();
    let (snap, _r) = daemon.capture_working_tree().unwrap();
    let target = node_id_of(
        &daemon
            .dispatch(Command::NodeCreate {
                kind: "snapshot".into(),
                type_version: "1.0.0".into(),
                parent_ids: vec![],
                branch_id: "main".into(),
                payload: serde_json::json!({ "schema_version": 1, "origin": "manual" }),
                owns_snapshot: true,
                snapshot_hash: Some(snap),
            })
            .unwrap(),
    );

    let err = daemon
        .dispatch(Command::NodeAgentRun {
            target_node_id: target,
            prompt: "hi".into(),
            model_key: "cli/copilot-cli".into(),
            privacy: "any".into(),
            intent: AgentRunIntent::Ask,
        })
        .unwrap_err();
    assert!(
        matches!(err, spork_daemon::IpcError::Capability(_)),
        "model.invoke must be denied by default, got {err:?}"
    );
}

#[test]
fn same_node_hot_swaps_between_two_providers() {
    // Both a CLI agent and a local HTTP server are configured; the same target is
    // asked once via each provider, attaching two independently-priced context
    // nodes (the DoD hot-swap, end to end).
    let dir = tempfile::tempdir().unwrap();
    let program = write_cli_agent(dir.path());
    let endpoint = serve_local_openai();
    let h = Harness::with_config(AgentConfig {
        local_endpoint: Some(endpoint),
        cli_command: Some((program, vec![])),
    });
    h.write("src/lib.rs", "fn ok() {}\n");
    let target = h.create_target();

    let cli = ids_of(
        &h.daemon
            .dispatch(Command::NodeAgentRun {
                target_node_id: target,
                prompt: "via cli".into(),
                model_key: "cli/copilot-cli".into(),
                privacy: "any".into(),
                intent: AgentRunIntent::Ask,
            })
            .unwrap(),
    );
    let local = ids_of(
        &h.daemon
            .dispatch(Command::NodeAgentRun {
                target_node_id: target,
                prompt: "via local".into(),
                model_key: "local/llama3.1".into(),
                privacy: "any".into(),
                intent: AgentRunIntent::Ask,
            })
            .unwrap(),
    );
    assert_eq!(cli["provider"], "cli");
    assert_eq!(local["provider"], "local");

    // Two context nodes now hang off the same target via dotted DerivedFrom edges.
    let view = h.daemon.graph_view();
    let attached = view
        .edges
        .iter()
        .filter(|e| e.to == target && e.edge_type == EdgeType::DerivedFrom)
        .count();
    assert_eq!(attached, 2, "both runs attached to the same node");
}
