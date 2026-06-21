//! P7 Definition-of-Done integration tests: gated merges, historical checkout,
//! lineage context + handoff, and the read-only Lineage/History MCP — all through
//! the daemon's frozen [`CommandHandler`](spork_ipc::CommandHandler) seam
//! (DESIGN.md §6.6, §8.3, §13.2, §13.5, §13.7, A.4).
//!
//! These exercise the P7 daemon wiring fully offline, proving:
//!
//! - a gated merge re-runs observers against the merged snapshot, evaluates a
//!   [`GatePolicy`] against those **post-merge** results, attaches an immutable
//!   gate-verdict node, and **withholds promotion** of the branch ref when the
//!   verdict blocks; an override promotes it and records a visible audit
//!   (DESIGN.md §8.3, A.4);
//! - a passing gate promotes the merge normally;
//! - `node.context` compiles a lineage-aware context with a stable `prefix_hash`
//!   and a selection trace (DESIGN.md §13.2, §13.6);
//! - `node.handoff` distills a regenerable handoff document (DESIGN.md §13.5);
//! - the read-only Lineage/History MCP answers `tools/list` /
//!   `walk_ancestors` / `search_history`, auto lineage-scoped (DESIGN.md §13.7);
//! - a historical checkout of a non-tip node auto-forks (fork-on-divergence),
//!   while a tip checkout does not (DESIGN.md §6.6);
//! - the History MCP stays denied without `nodes.readOutputs`.

use spork_baseline::Baseline;
use spork_daemon::{Command, CommandHandler, CommandResult, Daemon, GraphView};
use spork_gates::{GatePolicy, Predicate, Transition};
use ulid::Ulid;

struct Harness {
    daemon: Daemon,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::builder(dir.path())
            .grant_model_access() // also grants nodes.readOutputs for history.query
            .build()
            .unwrap();
        Harness { daemon, _dir: dir }
    }

    /// A daemon with the default grants only (no model/history access).
    fn ungranted() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::builder(dir.path()).build().unwrap();
        Harness { daemon, _dir: dir }
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.daemon.workdir().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn capture(&self) -> spork_hash::Hash {
        self.daemon.capture_working_tree().unwrap().0
    }

    fn snapshot_node(&self, branch: &str, parents: Vec<Ulid>, snap: spork_hash::Hash) -> Ulid {
        let r = self
            .daemon
            .dispatch(Command::NodeCreate {
                kind: "snapshot".into(),
                type_version: "1.0.0".into(),
                parent_ids: parents,
                branch_id: branch.into(),
                payload: serde_json::json!({ "schema_version": 1, "origin": "manual" }),
                owns_snapshot: true,
                snapshot_hash: Some(snap),
            })
            .unwrap();
        node_id_of(&r)
    }

    fn edit_node(
        &self,
        branch: &str,
        parents: Vec<Ulid>,
        snap: spork_hash::Hash,
        files: &[&str],
    ) -> Ulid {
        let r = self
            .daemon
            .dispatch(Command::NodeCreate {
                kind: "codebase-edit".into(),
                type_version: "1.0.0".into(),
                parent_ids: parents,
                branch_id: branch.into(),
                payload: serde_json::json!({
                    "schema_version": 1,
                    "diff_summary": "edit",
                    "files_changed": files,
                    "tool_calls": [],
                    "context_sources": [],
                }),
                owns_snapshot: true,
                snapshot_hash: Some(snap),
            })
            .unwrap();
        node_id_of(&r)
    }

    fn create_ref(&self, name: &str, to: Ulid) {
        self.daemon
            .dispatch(Command::RefCreate {
                name: name.into(),
                kind: spork_daemon::RefKind::Branch,
                to,
            })
            .unwrap();
    }

    fn gated_merge(
        &self,
        into_ref: &str,
        theirs: Ulid,
        gate: &GatePolicy,
        baseline: Option<&Baseline>,
        override_reason: Option<&str>,
    ) -> serde_json::Value {
        let r = self
            .daemon
            .dispatch(Command::BranchMergeGated {
                into_ref: into_ref.into(),
                from_node_id: theirs,
                resolution: None,
                gate: serde_json::to_value(gate).unwrap(),
                baseline: baseline.map(|b| serde_json::to_value(b).unwrap()),
                override_reason: override_reason.map(str::to_string),
            })
            .unwrap();
        ids_of(&r)
    }

    fn view(&self) -> GraphView {
        self.daemon.graph_view()
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

fn read_data(result: &CommandResult) -> serde_json::Value {
    match result {
        CommandResult::Read { data } => data.clone(),
        other => panic!("expected Read, got {other:?}"),
    }
}

/// Build R (on main) and an edit `theirs` (on feature, parent R) that adds a
/// file containing a forbidden marker (so the auto-run sanity attaches a failing
/// observer). Returns (R, theirs).
fn two_branches(h: &Harness) -> (Ulid, Ulid) {
    h.write("a.txt", "ok\n");
    let r_snap = h.capture();
    let r = h.snapshot_node("main", vec![], r_snap);
    h.create_ref("main", r);

    h.write("c.txt", "// FIXME finish this\n");
    let theirs_snap = h.capture();
    let theirs = h.edit_node("feature", vec![r], theirs_snap, &["c.txt"]);
    (r, theirs)
}

#[test]
fn gated_merge_blocks_and_withholds_promotion() {
    let h = Harness::new();
    let (r, theirs) = two_branches(&h);

    // A gate that always blocks (the regression-vs-baseline specifics are unit-
    // tested in spork-gates); here we prove the *daemon transition* is gated.
    let gate = GatePolicy::new("merge-block", Transition::Merge, Predicate::Never);
    let ids = h.gated_merge("main", theirs, &gate, None, None);

    assert_eq!(ids["merged"], true);
    assert_eq!(ids["decision"], "blocked");
    assert_eq!(ids["promoted"], false);
    let merge_node = Ulid::from_string(ids["mergeNodeId"].as_str().unwrap()).unwrap();
    let gate_node = Ulid::from_string(ids["gateNodeId"].as_str().unwrap()).unwrap();

    let view = h.view();
    // The branch ref was NOT promoted to the merge node — it still points at R.
    assert_eq!(
        view.ref_target("main"),
        Some(r),
        "blocked merge must not promote"
    );
    // The gate-verdict node exists and reads "blocked".
    let gv = view.node(gate_node).unwrap();
    assert_eq!(gv.kind, spork_daemon::GATE_KIND);
    assert_eq!(gv.gate.as_ref().unwrap().decision, "blocked");
    // The merge re-ran observers against the merged snapshot: a sanity observer
    // is attached to the merge node (post-merge re-run happened — DESIGN A.4).
    let post_merge_observer = view
        .nodes
        .iter()
        .any(|n| n.parent_ids.contains(&merge_node) && n.kind == "sanity");
    assert!(
        post_merge_observer,
        "observers must re-run against the merge node"
    );
}

#[test]
fn override_promotes_blocked_merge_and_records_audit() {
    let h = Harness::new();
    let (_r, theirs) = two_branches(&h);
    let gate = GatePolicy::new("merge-block", Transition::Merge, Predicate::Never);

    let ids = h.gated_merge("main", theirs, &gate, None, Some("urgent hotfix"));
    assert_eq!(ids["decision"], "overridden");
    assert_eq!(ids["promoted"], true);
    let merge_node = Ulid::from_string(ids["mergeNodeId"].as_str().unwrap()).unwrap();
    let gate_node = Ulid::from_string(ids["gateNodeId"].as_str().unwrap()).unwrap();

    let view = h.view();
    // Promoted: main now points at the merge node.
    assert_eq!(view.ref_target("main"), Some(merge_node));
    // The override is visible: the gate node decision is "overridden".
    let gv = view.node(gate_node).unwrap().gate.clone().unwrap();
    assert_eq!(gv.decision, "overridden");
    assert!(gv.overridden);
}

#[test]
fn real_post_merge_sanity_failure_blocks_via_predicate() {
    // The substantive A.4 guarantee: the gate evaluates the ACTUAL post-merge
    // re-run result. `theirs` adds a file containing a forbidden marker, so its
    // auto-run sanity (forbid TODO/FIXME/XXX) fails; the gated merge re-runs
    // sanity against the merged tree WITH the preserved forbid config → it fails
    // → an `all_passed(kind=sanity)` gate blocks (not via Never — via a real
    // post-merge result). Proves the config-preservation fix.
    let h = Harness::new();
    let (r, theirs) = two_branches(&h);
    let gate = GatePolicy::new(
        "merge-sanity",
        Transition::Merge,
        Predicate::AllPassed {
            kind: Some("sanity".into()),
        },
    );
    let ids = h.gated_merge("main", theirs, &gate, None, None);
    assert_eq!(
        ids["decision"], "blocked",
        "a real post-merge sanity failure must block"
    );
    assert_eq!(ids["promoted"], false);
    assert_eq!(
        h.view().ref_target("main"),
        Some(r),
        "blocked merge stays unpromoted"
    );
}

#[test]
fn passing_gate_promotes_the_merge() {
    let h = Harness::new();
    let (_r, theirs) = two_branches(&h);
    let gate = GatePolicy::new("merge-allow", Transition::Merge, Predicate::Always);

    let ids = h.gated_merge("main", theirs, &gate, None, None);
    assert_eq!(ids["decision"], "pass");
    assert_eq!(ids["promoted"], true);
    let merge_node = Ulid::from_string(ids["mergeNodeId"].as_str().unwrap()).unwrap();
    assert_eq!(h.view().ref_target("main"), Some(merge_node));
}

#[test]
fn node_context_compiles_with_stable_prefix_hash_and_trace() {
    let h = Harness::new();
    let (_r, theirs) = two_branches(&h);

    let data = read_data(
        &h.daemon
            .dispatch(Command::NodeContext { node_id: theirs })
            .unwrap(),
    );
    let prefix = data["prefixHash"].as_str().unwrap().to_string();
    assert!(!prefix.is_empty());
    assert!(
        data["layers"].as_array().unwrap().len() >= 2,
        "system + diff at least"
    );
    assert!(!data["selectionTrace"].as_array().unwrap().is_empty());

    // Deterministic: recompiling the same node yields the same prefix hash (the
    // warm-cache key is stable — DESIGN §13.2).
    let again = read_data(
        &h.daemon
            .dispatch(Command::NodeContext { node_id: theirs })
            .unwrap(),
    );
    assert_eq!(again["prefixHash"].as_str().unwrap(), prefix);
}

#[test]
fn node_handoff_distills_a_regenerable_document() {
    let h = Harness::new();
    let (_r, theirs) = two_branches(&h);
    let data = read_data(
        &h.daemon
            .dispatch(Command::NodeHandoff { node_id: theirs })
            .unwrap(),
    );
    // The handoff carries the node's lineage hash and is regenerable.
    assert!(data["lineage_hash"].as_str().is_some());
    assert_eq!(data["regenerable"], true);
    assert!(data["files_touched"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "c.txt"));
}

#[test]
fn history_mcp_answers_lineage_queries() {
    let h = Harness::new();
    let (r, theirs) = two_branches(&h);

    // tools/list advertises the six read-only tools.
    let list = read_data(
        &h.daemon
            .dispatch(Command::HistoryQuery {
                request: serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            })
            .unwrap(),
    );
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 6);

    // walk_ancestors(theirs) returns the lineage chain including R.
    let walk = read_data(
        &h.daemon
            .dispatch(Command::HistoryQuery {
                request: serde_json::json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": { "name": "walk_ancestors", "arguments": { "nodeId": theirs.to_string() } }
                }),
            })
            .unwrap(),
    );
    let text = walk["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(&r.to_string()),
        "lineage chain must include R"
    );
    assert!(text.contains(&theirs.to_string()));
}

#[test]
fn checkout_forks_on_a_non_tip_and_not_on_a_tip() {
    let h = Harness::new();
    let (r, theirs) = two_branches(&h);

    // R is a non-tip (theirs has it as a parent) → checkout auto-forks.
    let non_tip = ids_of(
        &h.daemon
            .dispatch(Command::NodeCheckout { node_id: r })
            .unwrap(),
    );
    assert_eq!(non_tip["isTip"], false);
    assert!(non_tip["forkedRef"].is_string(), "non-tip checkout forks");

    // theirs is a tip (no children) → checkout does not fork.
    let tip = ids_of(
        &h.daemon
            .dispatch(Command::NodeCheckout { node_id: theirs })
            .unwrap(),
    );
    assert_eq!(tip["isTip"], true);
    assert!(tip["forkedRef"].is_null(), "tip checkout does not fork");
}

#[test]
fn history_query_is_denied_without_capability() {
    let h = Harness::ungranted();
    let err = h
        .daemon
        .dispatch(Command::HistoryQuery {
            request: serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
        })
        .unwrap_err();
    assert!(
        matches!(err, spork_daemon::IpcError::Capability(_)),
        "history.query must be gated on nodes.readOutputs, got {err:?}"
    );
}
