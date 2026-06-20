//! P5 Definition-of-Done integration tests (DESIGN.md §6.5, §7.1, §8.1, §8.2,
//! A.4, A.7 C-2).
//!
//! These exercise the P5 wiring end-to-end through the daemon's frozen
//! [`CommandHandler`](spork_ipc::CommandHandler) seam, proving the DoD:
//!
//! - all six built-ins register through the public registry (no built-in-only
//!   path);
//! - creating an Edit node auto-triggers a Sanity check that cache-hits on an
//!   unchanged subtree;
//! - Validation/Stress runs attach append-only results to the parent **without**
//!   mutating it (parent `snapshot_hash` unchanged; results are observing
//!   children);
//! - a 3-way merge of an alternate Edit yields a clean, materializable Merge
//!   node, while a conflicting merge returns a conflict set and builds no node;
//! - import ingests external state as an `origin = import` Snapshot node, not a
//!   new kind.

use spork_daemon::{Command, CommandHandler, CommandResult, Daemon};
use spork_hash::Hash;
use ulid::Ulid;

/// A daemon rooted at a temp dir, with a working directory we can seed with files
/// so captured snapshots have real content.
struct Harness {
    daemon: Daemon,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(dir.path()).unwrap();
        Harness { daemon, _dir: dir }
    }

    /// Write a file into the daemon's working directory (so a capture sees it).
    fn write(&self, rel: &str, contents: &str) {
        let path = self.daemon.workdir().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    /// Capture the working tree and create a snapshot-owning node of `kind` with
    /// `payload`, returning the node id.
    fn create_node(&self, kind: &str, parents: Vec<Ulid>, payload: serde_json::Value) -> Ulid {
        let (snapshot_hash, _root) = self.daemon.capture_working_tree().unwrap();
        let result = self
            .daemon
            .dispatch(Command::NodeCreate {
                kind: kind.into(),
                type_version: "1.0.0".into(),
                parent_ids: parents,
                branch_id: "main".into(),
                payload,
                owns_snapshot: true,
                snapshot_hash: Some(snapshot_hash),
            })
            .unwrap();
        node_id_of(&result)
    }
}

/// Extract the `nodeId` of a mutation reply.
fn node_id_of(result: &CommandResult) -> Ulid {
    match result {
        CommandResult::Mutation { ids, .. } => {
            Ulid::from_string(ids["nodeId"].as_str().expect("nodeId")).unwrap()
        }
        other => panic!("expected Mutation, got {other:?}"),
    }
}

/// The mutation reply's `ids` bag.
fn ids_of(result: &CommandResult) -> serde_json::Value {
    match result {
        CommandResult::Mutation { ids, .. } => ids.clone(),
        other => panic!("expected Mutation, got {other:?}"),
    }
}

#[test]
fn all_six_builtins_are_registered_out_of_the_box() {
    // The daemon registers the six P5 built-ins at construction through the public
    // registry (DESIGN §7.1). Creating a node of each mutating kind succeeds, and
    // an observing kind is registered too (proved by a runCheck below).
    let h = Harness::new();
    h.write("src/lib.rs", "fn ok() {}\n");

    for kind in ["snapshot", "codebase-edit", "merge"] {
        // Each mutating built-in resolves and accepts a node.create. (Merge
        // normally has >=2 parents, but the registry/edge contract permits a
        // single-parent create here; we only assert the kind is registered.)
        let payload = match kind {
            "snapshot" => serde_json::json!({ "schema_version": 1, "origin": "manual" }),
            "codebase-edit" => {
                serde_json::json!({ "schema_version": 1, "diff_summary": "x", "files_changed": [], "tool_calls": [], "context_sources": [] })
            }
            _ => {
                serde_json::json!({ "schema_version": 1, "base_ref": Hash::from_bytes([0;32]).to_hex(), "conflict_resolution": { "schema_version": 1, "resolutions": [] } })
            }
        };
        let (snapshot_hash, _root) = h.daemon.capture_working_tree().unwrap();
        let result = h.daemon.dispatch(Command::NodeCreate {
            kind: kind.into(),
            type_version: "1.0.0".into(),
            parent_ids: vec![],
            branch_id: "main".into(),
            payload,
            owns_snapshot: true,
            snapshot_hash: Some(snapshot_hash),
        });
        assert!(
            result.is_ok(),
            "kind {kind} should be registered: {result:?}"
        );
    }
}

#[test]
fn creating_an_edit_auto_triggers_a_sanity_check_that_cache_hits() {
    let h = Harness::new();
    h.write("src/lib.rs", "fn clean() {}\n");
    let events = h.daemon.subscribe_events();

    // Create an Edit node — the auto-run hook schedules a change-scoped Sanity
    // check against it (DESIGN §8.2).
    let edit = h.create_node(
        "codebase-edit",
        vec![],
        serde_json::json!({
            "schema_version": 1,
            "diff_summary": "add clean fn",
            "files_changed": ["src/lib.rs"],
            "tool_calls": [],
            "context_sources": []
        }),
    );

    // The auto-run emits CHECK_SCHEDULED then RESULT_RECORDED (DESIGN §8.2).
    let mut scheduled = false;
    let mut recorded = false;
    while let Ok(ev) = events.try_recv() {
        match ev {
            spork_daemon::OpLogEvent::CheckScheduled { .. } => scheduled = true,
            spork_daemon::OpLogEvent::ResultRecorded { node_id, .. } => {
                recorded = true;
                // The result is an OBSERVING child of the Edit, not the Edit.
                assert_ne!(node_id, edit);
            }
            _ => {}
        }
    }
    assert!(scheduled, "auto-run must emit CHECK_SCHEDULED");
    assert!(recorded, "auto-run must emit RESULT_RECORDED");

    // The observing Sanity child is attached and owns no snapshot.
    let view = h.daemon.graph_view();
    let sanity_child = view
        .nodes
        .iter()
        .find(|n| n.kind == "sanity" && n.parent_ids.contains(&edit))
        .expect("a sanity observing child attached to the edit");
    assert!(
        !sanity_child.owns_snapshot,
        "observing node owns no snapshot"
    );

    // Re-running the SAME sanity check against the SAME (unchanged) snapshot is a
    // cache hit — the F4 input-digest cache (DESIGN §8.1, §8.2).
    let rerun = h
        .daemon
        .dispatch(Command::NodeRunCheck {
            target_node_id: edit,
            spec: serde_json::json!({
                "check": "sanity",
                "config": { "forbid": ["TODO", "FIXME", "XXX"] },
                "changed_paths": ["src/lib.rs"]
            }),
        })
        .unwrap();
    assert_eq!(
        ids_of(&rerun)["cacheHit"],
        serde_json::json!(true),
        "an identical re-run on an unchanged subtree must cache-hit"
    );
}

#[test]
fn validation_attaches_results_without_mutating_the_parent() {
    let h = Harness::new();
    h.write("src/lib.rs", "fn f() {}\n");
    let edit = h.create_node(
        "codebase-edit",
        vec![],
        serde_json::json!({
            "schema_version": 1, "diff_summary": "x", "files_changed": [],
            "tool_calls": [], "context_sources": []
        }),
    );

    // The parent's snapshot before the check.
    let before = parent_snapshot(&h, edit);

    // Run a Validation check whose junit-xml reports one pass + one fail.
    let junit = r#"<testsuite>
        <testcase name="passes"/>
        <testcase name="fails"><failure message="boom"/></testcase>
    </testsuite>"#;
    let result = h
        .daemon
        .dispatch(Command::NodeRunCheck {
            target_node_id: edit,
            spec: serde_json::json!({
                "check": "validation",
                "config": { "command": "cargo test", "junit_xml": junit }
            }),
        })
        .unwrap();
    let ids = ids_of(&result);
    assert_eq!(ids["outcome"], "failed", "one failing unit fails the suite");

    // The parent's snapshot is UNCHANGED — the result is observing (DESIGN §8.1).
    let after = parent_snapshot(&h, edit);
    assert_eq!(
        before, after,
        "validation must not mutate the parent snapshot"
    );

    // The result attached as an observing child via a VALIDATES edge.
    let view = h.daemon.graph_view();
    let validation = view
        .nodes
        .iter()
        .find(|n| n.kind == "validation" && n.parent_ids.contains(&edit))
        .expect("a validation observing child");
    assert!(!validation.owns_snapshot);
}

#[test]
fn stress_attaches_perf_metrics_without_mutating_the_parent() {
    let h = Harness::new();
    h.write("src/lib.rs", "fn f() {}\n");
    let edit = h.create_node(
        "codebase-edit",
        vec![],
        serde_json::json!({
            "schema_version": 1, "diff_summary": "x", "files_changed": [],
            "tool_calls": [], "context_sources": []
        }),
    );
    let before = parent_snapshot(&h, edit);

    let result = h
        .daemon
        .dispatch(Command::NodeRunCheck {
            target_node_id: edit,
            spec: serde_json::json!({
                "check": "stress",
                "config": {
                    "command": "k6 run load.js",
                    "metrics": { "p99_latency_us": 120000, "throughput_rps": 9000 }
                }
            }),
        })
        .unwrap();
    assert_eq!(ids_of(&result)["outcome"], "passed");

    let after = parent_snapshot(&h, edit);
    assert_eq!(before, after, "stress must not mutate the parent snapshot");

    let view = h.daemon.graph_view();
    assert!(
        view.nodes
            .iter()
            .any(|n| n.kind == "stress" && n.parent_ids.contains(&edit)),
        "a stress observing child attaches to the edit"
    );
}

#[test]
fn clean_three_way_merge_yields_a_materializable_merge_node() {
    let h = Harness::new();
    // Base: two files.
    h.write("a.txt", "1\n");
    h.write("b.txt", "1\n");
    let base = h.create_node("snapshot", vec![], snapshot_payload());

    // "ours": change a.txt only.
    h.write("a.txt", "ours\n");
    let ours = h.create_node("codebase-edit", vec![base], edit_payload(&["a.txt"]));

    // "theirs": from base, change b.txt only (disjoint edit -> clean merge).
    h.write("a.txt", "1\n"); // restore a.txt
    h.write("b.txt", "theirs\n");
    let theirs = h.create_node("codebase-edit", vec![base], edit_payload(&["b.txt"]));

    let result = h
        .daemon
        .dispatch(Command::BranchMerge {
            into_ref: ours.to_string(),
            from_node_id: theirs,
            resolution: None,
        })
        .unwrap();
    let ids = ids_of(&result);
    assert_eq!(
        ids["merged"],
        serde_json::json!(true),
        "disjoint edits merge cleanly"
    );
    let merge_node = Ulid::from_string(ids["mergeNodeId"].as_str().unwrap()).unwrap();

    // The Merge node is materializable: it owns a snapshot with >=2 parents.
    let view = h.daemon.graph_view();
    let merge = view.nodes.iter().find(|n| n.id == merge_node).unwrap();
    assert_eq!(merge.kind, "merge");
    assert!(merge.owns_snapshot, "a Merge node owns its merged snapshot");
    assert!(
        merge.parent_ids.contains(&ours) && merge.parent_ids.contains(&theirs),
        "the Merge node carries both parents"
    );

    // The merged snapshot is genuinely materializable — a node.diff against it
    // resolves (it reads the merged tree without error).
    let diff = h
        .daemon
        .dispatch(Command::NodeDiff {
            node_id: merge_node,
            against: Some(base),
        })
        .unwrap();
    assert!(matches!(diff, CommandResult::Diff { .. }));
}

#[test]
fn observing_results_re_run_against_the_merged_node_post_merge() {
    // DESIGN A.4: observing results do not merge; the merge re-runs them against
    // the merged tree so the new state is certified by POST-merge results, never
    // the stale pre-merge ones. This proves the "observing results re-run
    // post-merge" clause of the P5 DoD (item d).
    let h = Harness::new();
    h.write("a.txt", "1\n");
    h.write("b.txt", "1\n");
    let base = h.create_node("snapshot", vec![], snapshot_payload());

    // "ours": change a.txt only. Creating the Edit auto-runs a change-scoped
    // Sanity check (DESIGN §8.2), so `ours` already carries an observing Sanity
    // child — exactly the pre-merge observer the merge must re-run.
    h.write("a.txt", "ours\n");
    let ours = h.create_node("codebase-edit", vec![base], edit_payload(&["a.txt"]));

    let pre_merge_sanity_on_ours = h
        .daemon
        .graph_view()
        .nodes
        .iter()
        .filter(|n| n.kind == "sanity" && n.parent_ids.contains(&ours))
        .count();
    assert!(
        pre_merge_sanity_on_ours >= 1,
        "the Edit auto-run leaves a pre-merge observing sanity on ours"
    );

    // "theirs": from base, change b.txt only (disjoint -> clean merge).
    h.write("a.txt", "1\n"); // restore a.txt
    h.write("b.txt", "theirs\n");
    let theirs = h.create_node("codebase-edit", vec![base], edit_payload(&["b.txt"]));

    let result = h
        .daemon
        .dispatch(Command::BranchMerge {
            into_ref: ours.to_string(),
            from_node_id: theirs,
            resolution: None,
        })
        .unwrap();
    let ids = ids_of(&result);
    assert_eq!(ids["merged"], serde_json::json!(true));
    let merge_node = Ulid::from_string(ids["mergeNodeId"].as_str().unwrap()).unwrap();

    // The observing Sanity that was attached to a merge parent re-runs against the
    // MERGE node post-merge: a fresh sanity observing child now hangs off the merge
    // node, owning no snapshot (DESIGN A.4). The pre-merge result on `ours` is left
    // intact (append-only) — the merge does not mutate it.
    let view = h.daemon.graph_view();
    let post_merge_sanity_on_merge: Vec<_> = view
        .nodes
        .iter()
        .filter(|n| n.kind == "sanity" && n.parent_ids.contains(&merge_node))
        .collect();
    assert_eq!(
        post_merge_sanity_on_merge.len(),
        1,
        "the observing sanity re-runs against the merged node post-merge"
    );
    assert!(
        !post_merge_sanity_on_merge[0].owns_snapshot,
        "the re-run result is observing (owns no snapshot)"
    );
    // The pre-merge observer on `ours` is untouched (append-only history): the
    // merge attached a NEW observing result to the merge node rather than moving
    // or mutating the old one.
    let still_on_ours = view
        .nodes
        .iter()
        .filter(|n| n.kind == "sanity" && n.parent_ids.contains(&ours))
        .count();
    assert_eq!(
        still_on_ours, pre_merge_sanity_on_ours,
        "the pre-merge observing result is preserved (append-only)"
    );
}

#[test]
fn conflicting_merge_returns_a_conflict_set_and_builds_no_node() {
    let h = Harness::new();
    h.write("a.txt", "base\n");
    let base = h.create_node("snapshot", vec![], snapshot_payload());

    // Both sides change a.txt to DIFFERENT content -> conflict.
    h.write("a.txt", "ours\n");
    let ours = h.create_node("codebase-edit", vec![base], edit_payload(&["a.txt"]));
    h.write("a.txt", "theirs\n");
    let theirs = h.create_node("codebase-edit", vec![base], edit_payload(&["a.txt"]));

    let nodes_before = h.daemon.graph_view().nodes.len();

    let result = h
        .daemon
        .dispatch(Command::BranchMerge {
            into_ref: ours.to_string(),
            from_node_id: theirs,
            resolution: None,
        })
        .unwrap();
    let ids = ids_of(&result);
    assert_eq!(
        ids["merged"],
        serde_json::json!(false),
        "a conflict does not merge"
    );
    let conflicts = ids["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["path"], "a.txt");

    // No half-node was built (DESIGN A.4): the node count is unchanged.
    let nodes_after = h.daemon.graph_view().nodes.len();
    assert_eq!(
        nodes_before, nodes_after,
        "a conflicting merge builds no node"
    );
}

#[test]
fn conflicting_merge_with_a_resolution_produces_a_clean_node() {
    let h = Harness::new();
    h.write("a.txt", "base\n");
    let base = h.create_node("snapshot", vec![], snapshot_payload());
    h.write("a.txt", "ours\n");
    let ours = h.create_node("codebase-edit", vec![base], edit_payload(&["a.txt"]));
    h.write("a.txt", "theirs\n");
    let theirs = h.create_node("codebase-edit", vec![base], edit_payload(&["a.txt"]));

    // Supply a resolution that takes "theirs" for the conflicting file.
    let result = h
        .daemon
        .dispatch(Command::BranchMerge {
            into_ref: ours.to_string(),
            from_node_id: theirs,
            resolution: Some(serde_json::json!({
                "schema_version": 1,
                "resolutions": [ { "path": "a.txt", "chosen": { "choice": "theirs" } } ]
            })),
        })
        .unwrap();
    assert_eq!(
        ids_of(&result)["merged"],
        serde_json::json!(true),
        "a supplied resolution settles the conflict into a clean Merge node"
    );
}

#[test]
fn import_ingests_external_state_as_an_origin_import_snapshot() {
    // DESIGN A.7 C-2: import is NOT a new kind — it is a Snapshot with
    // origin=import.
    let h = Harness::new();
    h.write("imported.txt", "from elsewhere\n");
    let import = h.create_node(
        "snapshot",
        vec![],
        serde_json::json!({
            "schema_version": 1,
            "origin": "import",
            "import_source": { "type": "git", "uri": "https://example/x.git", "git_ref": "v1" }
        }),
    );

    let view = h.daemon.graph_view();
    let node = view.nodes.iter().find(|n| n.id == import).unwrap();
    // The kind is the ordinary snapshot kind — not a separate "import" kind.
    assert_eq!(node.kind, "snapshot");
    assert!(node.owns_snapshot);
}

// ---- helpers ----------------------------------------------------------------

/// The parent node's current snapshot hash (as a hex string), via the view.
fn parent_snapshot(h: &Harness, node: Ulid) -> Option<String> {
    h.daemon
        .graph_view()
        .nodes
        .into_iter()
        .find(|n| n.id == node)
        .and_then(|n| n.snapshot_hash)
}

/// A manual-snapshot payload.
fn snapshot_payload() -> serde_json::Value {
    serde_json::json!({ "schema_version": 1, "origin": "manual" })
}

/// An Edit payload changing `files`.
fn edit_payload(files: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "diff_summary": "edit",
        "files_changed": files,
        "tool_calls": [],
        "context_sources": []
    })
}
