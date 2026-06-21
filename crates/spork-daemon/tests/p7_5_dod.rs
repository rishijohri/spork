//! P7.5 (Local Trusted-Agent MVP) Slice A Definition-of-Done integration tests —
//! the wiring that makes Spork usable as a minimum viable product (docs/MVP_PLAN.md
//! §5 W1/W2/W3, §8; DESIGN.md §10.1, §6.2, §12.1, §13.7).
//!
//! These exercise the four Slice-A workstreams end to end, offline:
//!
//! - **W1 onboarding/import + W2 root-at-real-dir** — a daemon rooted at a real
//!   project dir (`for_project`) captures the user's *actual* files into a root
//!   snapshot node via `project.import`, points the branch ref + `HEAD` at it,
//!   excludes Spork's own `.spork/` state, refuses a double-import, and rebuilds
//!   from the log on reopen.
//! - **W3 provider config** — `set_agent_config` swaps the configured provider at
//!   runtime so an ask that had no transport now succeeds; the config serde
//!   round-trips (what the desktop app persists).
//! - **W5 transcript-read fix** — a read-only ask now stores its canonical
//!   transcript as a `conversation_ref`, so the History MCP's
//!   `get_node_transcript` returns it (it returned `None` before).

use spork_daemon::{
    AgentConfig, AgentRunIntent, Command, CommandHandler, CommandResult, Daemon, DaemonBuilder,
    Family,
};
use ulid::Ulid;

/// Extract the `nodeId` of a mutation result.
fn node_id_of(result: &CommandResult) -> Ulid {
    match result {
        CommandResult::Mutation { ids, .. } => {
            Ulid::from_string(ids["nodeId"].as_str().expect("nodeId")).unwrap()
        }
        other => panic!("expected Mutation, got {other:?}"),
    }
}

/// Extract the inline data of a read result.
fn read_data(result: &CommandResult) -> serde_json::Value {
    match result {
        CommandResult::Read { data } => data.clone(),
        other => panic!("expected Read, got {other:?}"),
    }
}

/// Build a fixture project directory with a couple of real files.
fn fixture_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "# fixture\n").unwrap();
    dir
}

/// Write an executable CLI-agent shell script that ignores stdin and prints a
/// fixed JSONL response (an assistant line + a usage block). Mirrors the P6 test
/// harness so the offline agent loop runs without a real model.
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

/// Write an executable CLI-agent script that drives one **edit** turn: on the
/// first call (no tool result yet) it emits a `write_file` tool call; on the
/// second call (the daemon has executed the tool and appended its result to the
/// transcript, which the subprocess sees on stdin) it emits a final text answer.
fn write_edit_agent(dir: &std::path::Path) -> String {
    let path = dir.join("edit_agent.sh");
    let body = "#!/bin/sh\nbody=$(cat)\ncase \"$body\" in\n  *tool_result*)\n    printf '%s\\n' '{\"lines\":[{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"added the file\"}]}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":5}}'\n    ;;\n  *)\n    printf '%s\\n' '{\"lines\":[{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_call\",\"id\":\"c1\",\"name\":\"write_file\",\"input\":{\"path\":\"NEW.txt\",\"content\":\"hello from agent\\n\"}}]}],\"usage\":{\"prompt_tokens\":15,\"completion_tokens\":8}}'\n    ;;\nesac\n";
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().to_string()
}

/// Open a project-rooted daemon with the scripted edit agent configured, import
/// it, and return `(daemon, project_dir, root_node)`.
fn edit_harness() -> (Daemon, tempfile::TempDir, Ulid) {
    let project = fixture_project();
    let scripts = tempfile::tempdir().unwrap();
    let program = write_edit_agent(scripts.path());
    // Keep the scripts dir alive for the daemon's lifetime by leaking it (the test
    // process is short-lived; the OS reclaims it). Simpler than threading it out.
    std::mem::forget(scripts);
    let daemon = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        .with_agent_config(AgentConfig::with_cli_agent(program, vec![]))
        .build()
        .unwrap();
    let root = node_id_of(
        &daemon
            .dispatch(Command::ProjectImport {
                branch_id: "main".into(),
                origin: "import".into(),
            })
            .unwrap(),
    );
    (daemon, project, root)
}

fn ids_of(result: &CommandResult) -> serde_json::Value {
    match result {
        CommandResult::Mutation { ids, .. } => ids.clone(),
        other => panic!("expected Mutation, got {other:?}"),
    }
}

#[test]
fn node_agent_edit_creates_an_edit_node_with_a_real_change() {
    // W4: ask the agent to change the code → an Edit node owning a new snapshot
    // with a real diff, on the same branch (the target is a tip).
    let (daemon, project, root) = edit_harness();

    let result = daemon
        .dispatch(Command::NodeAgentEdit {
            target_node_id: root,
            prompt: "create NEW.txt".into(),
            model_key: "cli/copilot-cli".into(),
            privacy: "any".into(),
        })
        .unwrap();
    let ids = ids_of(&result);
    let edit_node = Ulid::from_string(ids["editNodeId"].as_str().unwrap()).unwrap();
    assert_eq!(
        ids["forked"], false,
        "an edit from a tip continues its branch"
    );
    assert_eq!(ids["model"], "cli/copilot-cli");
    assert_eq!(ids["branchId"], "main");

    // The Edit node owns a NEW snapshot, parented on the target.
    let view = daemon.graph_view();
    let node = view.node(edit_node).expect("edit node renders");
    assert_eq!(node.kind, "codebase-edit");
    assert_eq!(node.family, Family::Mutating);
    assert!(node.owns_snapshot);
    assert_eq!(node.parent_ids, vec![root]);

    // Its diff vs the parent shows the agent's change.
    let changed = match daemon
        .dispatch(Command::NodeDiff {
            node_id: edit_node,
            against: Some(root),
        })
        .unwrap()
    {
        CommandResult::Diff { changed_paths } => changed_paths,
        other => panic!("expected Diff, got {other:?}"),
    };
    assert!(
        changed.iter().any(|p| p == "NEW.txt"),
        "the edit must add NEW.txt, got {changed:?}"
    );

    // The user's REAL working dir is untouched — the edit ran on a CoW copy.
    assert!(
        !project.path().join("NEW.txt").exists(),
        "the agent must never touch the real checkout (DESIGN §9.2/§15.3)"
    );

    // An auto-Sanity check attached (observing); a clean file passes.
    assert!(ids["sanity"].is_string(), "auto-Sanity result is attached");
}

#[test]
fn node_agent_edit_from_a_non_tip_auto_forks() {
    // W4 + §6.6: a second edit from the (now non-tip) root auto-forks a new branch
    // so the first line is never silently overwritten.
    let (daemon, _project, root) = edit_harness();

    // First edit makes `root` a non-tip (it now has a Mutating child).
    daemon
        .dispatch(Command::NodeAgentEdit {
            target_node_id: root,
            prompt: "edit one".into(),
            model_key: "cli/copilot-cli".into(),
            privacy: "any".into(),
        })
        .unwrap();

    // Second edit from the non-tip root → auto-fork.
    let ids = ids_of(
        &daemon
            .dispatch(Command::NodeAgentEdit {
                target_node_id: root,
                prompt: "edit two".into(),
                model_key: "cli/copilot-cli".into(),
                privacy: "any".into(),
            })
            .unwrap(),
    );
    assert_eq!(ids["forked"], true, "an edit from a non-tip auto-forks");
    assert!(
        ids["branchId"].as_str().unwrap().starts_with("agent/"),
        "the fork lands on a fresh agent/ branch, got {:?}",
        ids["branchId"]
    );
}

#[test]
fn node_agent_edit_binds_a_retrievable_transcript() {
    // W4 + W5: the Edit node binds its loop transcript as conversation_ref, so the
    // History MCP returns it (code+conversation, DESIGN §13.4).
    let (daemon, _project, root) = edit_harness();
    let edit_node = Ulid::from_string(
        ids_of(
            &daemon
                .dispatch(Command::NodeAgentEdit {
                    target_node_id: root,
                    prompt: "create NEW.txt".into(),
                    model_key: "cli/copilot-cli".into(),
                    privacy: "any".into(),
                })
                .unwrap(),
        )["editNodeId"]
            .as_str()
            .unwrap(),
    )
    .unwrap();

    let resp = read_data(
        &daemon
            .dispatch(Command::HistoryQuery {
                request: serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {
                        "name": "get_node_transcript",
                        "arguments": { "nodeId": edit_node.to_string() }
                    }
                }),
            })
            .unwrap(),
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("transcript content");
    assert!(
        text.contains("write_file") || text.contains("added the file"),
        "the edit transcript must be retrievable, got {text}"
    );
}

#[test]
fn project_import_captures_real_repo_into_a_root_node() {
    // W1 + W2: a project-rooted daemon captures the user's real files.
    let project = fixture_project();
    let daemon = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        .build()
        .unwrap();

    // W2: the working tree IS the project dir (not an empty `…/work` scratch),
    // and Spork's own state lives under a hidden `.spork/`.
    assert_eq!(daemon.workdir(), project.path());
    assert!(
        project.path().join(".spork").join("cas").exists(),
        "daemon state must live under <project>/.spork/"
    );

    // W1: import mints a root snapshot node.
    let result = daemon
        .dispatch(Command::ProjectImport {
            branch_id: "main".into(),
            origin: "import".into(),
        })
        .unwrap();
    let node_id = node_id_of(&result);

    let view = daemon.graph_view();
    let node = view.node(node_id).expect("root node renders in the view");
    assert_eq!(node.kind, "snapshot");
    assert_eq!(node.family, Family::Mutating);
    assert!(node.owns_snapshot, "the root owns a restorable snapshot");
    assert!(
        node.parent_ids.is_empty(),
        "a root node has no lineage parent"
    );

    // The branch ref + HEAD point at the root, so it is a branch tip.
    assert_eq!(view.ref_target("main"), Some(node_id));
    assert_eq!(view.ref_target("HEAD"), Some(node_id));

    // The captured tree lists the repo's real files (a root vs the empty base).
    let diff = daemon
        .dispatch(Command::NodeDiff {
            node_id,
            against: None,
        })
        .unwrap();
    let changed = match diff {
        CommandResult::Diff { changed_paths } => changed_paths,
        other => panic!("expected Diff, got {other:?}"),
    };
    assert!(
        changed.iter().any(|p| p == "src/main.rs"),
        "captured tree must include src/main.rs, got {changed:?}"
    );
    assert!(
        changed.iter().any(|p| p == "README.md"),
        "captured tree must include README.md, got {changed:?}"
    );
}

#[test]
fn project_import_excludes_the_spork_state_dir() {
    // W2: building the daemon creates `<project>/.spork/…`; capture must NOT
    // ingest it (else the CAS would recursively snapshot itself).
    let project = fixture_project();
    let daemon = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        .build()
        .unwrap();
    let node_id = node_id_of(
        &daemon
            .dispatch(Command::ProjectImport {
                branch_id: "main".into(),
                origin: "import".into(),
            })
            .unwrap(),
    );

    let changed = match daemon
        .dispatch(Command::NodeDiff {
            node_id,
            against: None,
        })
        .unwrap()
    {
        CommandResult::Diff { changed_paths } => changed_paths,
        other => panic!("expected Diff, got {other:?}"),
    };
    assert!(
        !changed.iter().any(|p| p.starts_with(".spork/")),
        "the Spork state dir must be excluded from capture, got {changed:?}"
    );
}

#[test]
fn project_import_is_idempotent_on_reopen() {
    // The renderer onboards by calling import every time it opens a project, so a
    // second import on an already-imported branch must be a NO-OP that returns the
    // existing tip (not an error, not a duplicate root) — otherwise a reopened
    // project's canvas stays stuck on the empty state.
    let project = fixture_project();
    let daemon = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        .build()
        .unwrap();
    let first = node_id_of(
        &daemon
            .dispatch(Command::ProjectImport {
                branch_id: "main".into(),
                origin: "import".into(),
            })
            .unwrap(),
    );
    let nodes_before = daemon.graph_view().nodes.len();

    // A second import succeeds, returns the SAME root, and mints no new node.
    let result = daemon
        .dispatch(Command::ProjectImport {
            branch_id: "main".into(),
            origin: "import".into(),
        })
        .unwrap();
    let again = node_id_of(&result);
    assert_eq!(again, first, "re-import returns the existing root");
    assert_eq!(
        daemon.graph_view().nodes.len(),
        nodes_before,
        "re-import mints no new node"
    );
    match &result {
        CommandResult::Mutation { ids, .. } => assert_eq!(ids["reopened"], true),
        other => panic!("expected Mutation, got {other:?}"),
    }
}

#[test]
fn imported_project_rebuilds_from_the_log_on_reopen() {
    // W1: the graph is a pure projection of the durable log — reopening the same
    // project dir rebuilds the imported root with no re-import.
    let project = fixture_project();
    let node_id = {
        let daemon = DaemonBuilder::for_project(project.path())
            .grant_model_access()
            .build()
            .unwrap();
        node_id_of(
            &daemon
                .dispatch(Command::ProjectImport {
                    branch_id: "main".into(),
                    origin: "import".into(),
                })
                .unwrap(),
        )
    };

    // Reopen over the same dir: the daemon rebuilds its projection from `.spork/`.
    let reopened = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        .build()
        .unwrap();
    let view = reopened.graph_view();
    assert!(
        view.node(node_id).is_some(),
        "the imported root survives a reopen (rebuilt from the log)"
    );
    assert_eq!(view.ref_target("main"), Some(node_id));
}

#[test]
fn set_agent_config_swaps_the_provider_at_runtime() {
    // W3: a daemon opened with NO usable provider cannot answer an ask; after
    // `set_agent_config` points it at a CLI agent, the same ask succeeds — the
    // provider swapped at runtime, no reopen.
    let project = fixture_project();
    let daemon = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        // Default agent config: no endpoint, no CLI → no transport.
        .build()
        .unwrap();
    let target = node_id_of(
        &daemon
            .dispatch(Command::ProjectImport {
                branch_id: "main".into(),
                origin: "import".into(),
            })
            .unwrap(),
    );

    // Before configuration: the CLI provider has no transport, so the ask fails.
    let before = daemon.dispatch(Command::NodeAgentRun {
        target_node_id: target,
        prompt: "what does this do?".into(),
        model_key: "cli/copilot-cli".into(),
        privacy: "any".into(),
        intent: AgentRunIntent::Ask,
    });
    assert!(before.is_err(), "an unconfigured provider must not answer");

    // Configure a CLI agent at runtime.
    let scripts = tempfile::tempdir().unwrap();
    let program = write_cli_agent(scripts.path());
    daemon.set_agent_config(AgentConfig::with_cli_agent(program, vec![]));

    // Now the same ask succeeds through the freshly configured transport.
    let after = daemon
        .dispatch(Command::NodeAgentRun {
            target_node_id: target,
            prompt: "what does this do?".into(),
            model_key: "cli/copilot-cli".into(),
            privacy: "any".into(),
            intent: AgentRunIntent::Ask,
        })
        .unwrap();
    let ids = match &after {
        CommandResult::Mutation { ids, .. } => ids.clone(),
        other => panic!("expected Mutation, got {other:?}"),
    };
    assert_eq!(ids["provider"], "cli");
    assert_eq!(ids["model"], "cli/copilot-cli");
}

#[test]
fn agent_run_stores_a_retrievable_transcript() {
    // W5: a read-only ask binds its canonical transcript as a `conversation_ref`,
    // so the History MCP's `get_node_transcript` returns it (it was `None`).
    let project = fixture_project();
    let scripts = tempfile::tempdir().unwrap();
    let program = write_cli_agent(scripts.path());
    let daemon = DaemonBuilder::for_project(project.path())
        .grant_model_access()
        .with_agent_config(AgentConfig::with_cli_agent(program, vec![]))
        .build()
        .unwrap();
    let target = node_id_of(
        &daemon
            .dispatch(Command::ProjectImport {
                branch_id: "main".into(),
                origin: "import".into(),
            })
            .unwrap(),
    );

    let ctx_id = node_id_of(
        &daemon
            .dispatch(Command::NodeAgentRun {
                target_node_id: target,
                prompt: "what does this do?".into(),
                model_key: "cli/copilot-cli".into(),
                privacy: "any".into(),
                intent: AgentRunIntent::Ask,
            })
            .unwrap(),
    );

    // The History MCP returns the stored transcript for the agent-run node.
    let resp = read_data(
        &daemon
            .dispatch(Command::HistoryQuery {
                request: serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {
                        "name": "get_node_transcript",
                        "arguments": { "nodeId": ctx_id.to_string() }
                    }
                }),
            })
            .unwrap(),
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("transcript content");
    assert!(
        text.contains("the answer"),
        "the stored transcript must be retrievable, got {text}"
    );
}

#[test]
fn agent_config_serde_round_trips() {
    // W3: the persisted shape the desktop app writes to `.spork/agent_config.json`.
    for cfg in [
        AgentConfig::with_default_local(),
        AgentConfig::with_endpoint("http://127.0.0.1:1234/v1/chat/completions"),
        AgentConfig::with_cli_agent("copilot-cli", vec!["--json".into()]),
        AgentConfig::default(),
    ] {
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.local_endpoint, back.local_endpoint);
        assert_eq!(cfg.cli_command, back.cli_command);
    }
}

/// A plain (non-project) daemon still defaults to the `<root>/work` layout — the
/// `for_project` change is additive and does not disturb existing callers.
#[test]
fn plain_builder_keeps_the_work_subdir_layout() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    assert_eq!(daemon.workdir(), dir.path().join("work"));
}
