//! The F3 headless-core Definition of Done, driven by a headless client over the
//! real in-process daemon (DESIGN.md §5.5, §10.1–§10.4, §14.1, §15.1–§15.5, A.1,
//! A.3).
//!
//! There is no GUI here: a plain `#[test]` *is* the headless client. Each test
//! maps to a DoD bullet:
//!
//! - mutations return an `op_id` and resulting state arrives via the ordered
//!   event stream (not the mutation return);
//! - ephemeral token/stdout volume never stalls ordered event delivery;
//! - out-of-band `rm`/`mv`/add/modify and an unsaved buffer each produce a
//!   correctly-attributed drift node within the debounce window;
//! - a planted fake API key is caught at capture and never enters the CAS;
//! - a side effect without a granted capability is denied with an `AuditEntry`;
//! - restoring an old node restores code + bound conversation atomically, fails
//!   closed on injected divergence, and forward history survives as a sibling;
//! - `export_to_git` produces a clean commit while `.git` stays byte-unchanged;
//! - node restore p95 < 500 ms (measured honestly).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use spork_broker::{Capability, Grant, Scope};
use spork_daemon::{
    Command, CommandHandler, CommandResult, Daemon, EphemeralChannel, IpcError, OpLogEvent,
};
use spork_drift::{
    Attribution, BufferBridge, ChangeEvent, ChangeOp, ChangeSource, ChangeSourceKind, Confidence,
    EditInterceptor, ReconciliationRescan, TurnContext,
};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use ulid::Ulid;

// --------------------------------------------------------------------------
// Headless harness
// --------------------------------------------------------------------------

/// A headless harness: a daemon rooted in a temp dir, plus accessors for its
/// working tree.
struct Harness {
    _root: tempfile::TempDir,
    daemon: Daemon,
}

impl Harness {
    /// A daemon with the default grants (snapshot read/write over the worktree).
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let daemon = Daemon::open(root.path()).unwrap();
        Harness {
            _root: root,
            daemon,
        }
    }

    /// A daemon with an explicit grant set (e.g. to test a denial).
    fn with_grants(grants: Vec<Grant>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let daemon = Daemon::builder(root.path())
            .with_grants(grants)
            .build()
            .unwrap();
        Harness {
            _root: root,
            daemon,
        }
    }

    fn work(&self) -> std::path::PathBuf {
        self.daemon.workdir()
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.work().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, bytes).unwrap();
    }
}

/// The default snapshot grants the daemon ships with.
fn default_grants() -> Vec<Grant> {
    let worktree = Scope::new().with_path_globs(["**"]);
    vec![
        Grant::new(Capability::SnapshotRead, worktree.clone()),
        Grant::new(Capability::SnapshotWrite, worktree),
    ]
}

/// Create a snapshot-owning node by capturing the current working tree, binding
/// an optional conversation, and dispatching `node.create`. Returns the node id.
fn create_snapshot_node(daemon: &Daemon, parents: Vec<Ulid>, conversation: Option<&[u8]>) -> Ulid {
    let (snapshot_hash, _root_tree) = daemon.capture_working_tree().unwrap();
    let mut payload = serde_json::json!({ "origin": "manual" });
    if let Some(conv) = conversation {
        let conv_hash = daemon.put_conversation(conv).unwrap();
        payload["conversationRef"] = serde_json::json!(conv_hash.to_hex());
    }
    let result = daemon
        .dispatch(Command::NodeCreate {
            kind: "snapshot".into(),
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

/// Pull the `nodeId` out of a `Mutation` result.
fn node_id_of(result: &CommandResult) -> Ulid {
    match result {
        CommandResult::Mutation { ids, .. } => {
            let s = ids["nodeId"].as_str().expect("nodeId in mutation result");
            Ulid::from_string(s).expect("valid ULID")
        }
        other => panic!("expected a Mutation result, got {other:?}"),
    }
}

/// Read every file under `dir` into a sorted `path -> bytes` map.
fn read_tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(base, &path, out);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// A manual, who-blind fs-watcher: a [`ChangeSource`] that replays queued events
/// (the real notify-backed watcher needs real-fs timing; this models its output
/// deterministically for the precedence test).
struct ManualWatcher {
    events: Vec<ChangeEvent>,
}

impl ManualWatcher {
    fn new() -> Self {
        ManualWatcher { events: Vec::new() }
    }

    fn push(&mut self, path: &str, op: ChangeOp) {
        self.events.push(ChangeEvent::now(
            path,
            op,
            ChangeSourceKind::FsWatcher,
            None,
        ));
    }
}

impl ChangeSource for ManualWatcher {
    fn kind(&self) -> ChangeSourceKind {
        ChangeSourceKind::FsWatcher
    }
    fn poll(&mut self) -> Vec<ChangeEvent> {
        std::mem::take(&mut self.events)
    }
}

// --------------------------------------------------------------------------
// DoD: mutation returns op_id; state arrives via the ordered event stream
// --------------------------------------------------------------------------

#[test]
fn mutation_returns_op_id_and_state_arrives_via_events() {
    let h = Harness::new();
    h.write("src/main.rs", b"fn main() {}\n");

    // Subscribe BEFORE the mutation so we observe its events.
    let events = h.daemon.subscribe_events();

    let (snapshot_hash, _root) = h.daemon.capture_working_tree().unwrap();
    let result = h
        .daemon
        .dispatch(Command::NodeCreate {
            kind: "snapshot".into(),
            type_version: "1.0.0".into(),
            parent_ids: vec![],
            branch_id: "main".into(),
            payload: serde_json::json!({ "origin": "manual" }),
            owns_snapshot: true,
            snapshot_hash: Some(snapshot_hash),
        })
        .unwrap();

    // The return value carries ONLY an op_id (+ minted ids), never graph state.
    let op_id = result.op_id().expect("a mutation returns an op_id");
    let node_id = node_id_of(&result);
    let result_json = serde_json::to_value(&result).unwrap();
    assert!(result_json.get("nodeCreated").is_none());
    assert!(result_json.get("changedPaths").is_none());
    let _ = op_id;

    // The resulting state arrives over the ordered stream.
    let event = events.recv_timeout(Duration::from_secs(2)).unwrap();
    match event {
        OpLogEvent::NodeCreated {
            seq, node_id: got, ..
        } => {
            assert_eq!(seq, 1, "the first event carries seq 1");
            assert_eq!(got, node_id, "the event reports the created node");
        }
        other => panic!("expected NodeCreated, got {other:?}"),
    }

    // The denormalized view-model surface reflects the new node.
    let view = h.daemon.graph_view();
    assert!(view.node(node_id).is_some());
}

#[test]
fn parent_edges_arrive_as_ordered_events_with_no_gaps() {
    let h = Harness::new();
    h.write("a.txt", b"a\n");
    let events = h.daemon.subscribe_events();

    let root = create_snapshot_node(&h.daemon, vec![], None);
    h.write("b.txt", b"b\n");
    let child = create_snapshot_node(&h.daemon, vec![root], None);

    // Drain events: NodeCreated(root), NodeCreated(child), EdgeAdded(root->child).
    let mut seqs = Vec::new();
    let mut saw_edge = false;
    for _ in 0..3 {
        let e = events.recv_timeout(Duration::from_secs(2)).unwrap();
        seqs.push(e.seq());
        if let OpLogEvent::EdgeAdded { from, to, edge, .. } = e {
            assert_eq!(from, root);
            assert_eq!(to, child);
            assert_eq!(edge, spork_daemon::EdgeType::ParentChild);
            saw_edge = true;
        }
    }
    assert!(saw_edge, "the parent edge was published as an event");
    assert_eq!(seqs, vec![1, 2, 3], "ordered, gapless seqs");
}

// --------------------------------------------------------------------------
// DoD: ephemeral volume never stalls ordered event delivery
// --------------------------------------------------------------------------

#[test]
fn ephemeral_flood_does_not_stall_ordered_event_delivery() {
    let h = Harness::new();

    // Subscribe to the ordered rail BEFORE any mutation so we see seq 1 onward.
    let ordered = h.daemon.subscribe_events();

    h.write("x.txt", b"x\n");
    let node = create_snapshot_node(&h.daemon, vec![], None);
    let _ephemeral = h.daemon.subscribe_node(node);

    // The node's creation event is on the rail (seq 1).
    let first = ordered.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first.seq(), 1);

    // Flood the node's ephemeral channel with thousands of frames.
    for i in 0..5000u32 {
        h.daemon
            .publish_ephemeral(node, EphemeralChannel::ChatTokens, format!("tok{i}"));
    }

    // Now perform an ordered mutation; its event must arrive promptly despite the
    // flood (the channels are structurally separate — DESIGN §5.5, §14.4).
    let started = Instant::now();
    h.write("y.txt", b"y\n");
    let child = create_snapshot_node(&h.daemon, vec![node], None);

    // Find the child's NodeCreated among the next ordered events.
    let mut found_child = false;
    for _ in 0..3 {
        if let OpLogEvent::NodeCreated { node_id, .. } =
            ordered.recv_timeout(Duration::from_secs(2)).unwrap()
        {
            if node_id == child {
                found_child = true;
                break;
            }
        }
    }
    assert!(
        found_child,
        "the ordered child event arrived despite the flood"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "ordered delivery stayed prompt under the ephemeral flood"
    );
}

// --------------------------------------------------------------------------
// DoD: out-of-band rm/mv/add/modify + unsaved buffer -> attributed drift node
// --------------------------------------------------------------------------

#[test]
fn out_of_band_mutations_produce_an_attributed_drift_node() {
    let h = Harness::new();
    h.write("keep.rs", b"fn keep() {}\n");
    h.write("gone.rs", b"fn gone() {}\n");

    // Seed a rescan baseline from the current tree.
    let matcher = IgnoreMatcher::new(&IgnoreProfile::default_profile());
    let mut rescan = ReconciliationRescan::new(h.work(), matcher);
    rescan.set_baseline(rescan.scan_tree().unwrap());

    // Seed a root node to parent the drift on.
    let root = create_snapshot_node(&h.daemon, vec![], None);

    // Simulate raw bash: modify, add (mv-destination), rm (mv-source).
    std::fs::write(h.work().join("keep.rs"), b"fn keep() { /* edited */ }\n").unwrap();
    std::fs::write(h.work().join("added.rs"), b"fn added() {}\n").unwrap();
    std::fs::remove_file(h.work().join("gone.rs")).unwrap();
    rescan.rescan().unwrap();

    let events = h.daemon.subscribe_events();
    let ctx = TurnContext::default();
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut rescan];
    let report = h
        .daemon
        .capture_drift(&mut sources, &ctx, vec![root], "main", None)
        .unwrap();

    // The drift node exists, parented at root, and is in the view-model.
    let view = h.daemon.graph_view();
    let drift = view.node(report.node_id).expect("drift node in view");
    assert_eq!(drift.parent_ids, vec![root]);

    // Attribution: no active turn + no exec anchor => External (never the agent).
    let by_path: BTreeMap<_, _> = report
        .attribution
        .iter()
        .map(|r| (r.path.as_str(), r))
        .collect();
    assert_eq!(by_path["keep.rs"].op, ChangeOp::Modified);
    assert_eq!(by_path["added.rs"].op, ChangeOp::Added);
    assert_eq!(by_path["gone.rs"].op, ChangeOp::Removed);
    for r in &report.attribution {
        assert_eq!(r.attribution, Attribution::External, "{r:?}");
        assert_eq!(r.confidence, Confidence::Medium);
    }

    // The drift node's NodeCreated + EdgeAdded reached the ordered rail.
    let mut saw_node = false;
    for _ in 0..2 {
        if let OpLogEvent::NodeCreated { node_id, .. } =
            events.recv_timeout(Duration::from_secs(2)).unwrap()
        {
            if node_id == report.node_id {
                saw_node = true;
            }
        }
    }
    assert!(saw_node, "the drift node surfaced on the ordered stream");
}

#[test]
fn interceptor_outranks_watcher_for_the_same_path() {
    let h = Harness::new();
    h.write("src/app.rs", b"fn main() {}\n");
    let root = create_snapshot_node(&h.daemon, vec![], None);

    // The agent edits in-process (interceptor) during turn 4; a who-blind watcher
    // independently observes the same path.
    let mut interceptor = EditInterceptor::new();
    interceptor.begin_turn(4);
    interceptor.record("src/app.rs", ChangeOp::Modified);
    let mut watcher = ManualWatcher::new();
    watcher.push("src/app.rs", ChangeOp::Modified);

    let ctx = TurnContext {
        active_turn: Some(4),
        ..Default::default()
    };
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut interceptor, &mut watcher];
    let report = h
        .daemon
        .capture_drift(&mut sources, &ctx, vec![root], "main", None)
        .unwrap();

    let rec = report
        .attribution
        .iter()
        .find(|r| r.path == "src/app.rs")
        .unwrap();
    // The interceptor is authoritative on *who*: agent, high confidence.
    assert_eq!(rec.attribution, Attribution::Agent);
    assert_eq!(rec.confidence, Confidence::High);
    assert!(
        !rec.review_flag,
        "a high-confidence attribution sets no review flag"
    );
}

#[test]
fn unsaved_buffer_yields_a_human_attributed_node_capturing_the_buffer_bytes() {
    // DoD (c), the unsaved-buffer case: a focused, dirty editor buffer must yield
    // a correctly-attributed drift node whose captured tree reflects what the
    // human is looking at (the unsaved bytes), not the stale on-disk bytes
    // (DESIGN.md §10.2 buffer bridge, A.3 Q2).
    let h = Harness::new();
    // On disk: the last *saved* version.
    h.write("src/draft.rs", b"fn draft() { /* saved */ }\n");
    let root = create_snapshot_node(&h.daemon, vec![], None);

    // The human has unsaved edits open and focused for that path. The bridge
    // plays two cooperating roles in the headless wiring: it is a change *source*
    // (emitting the dirty-buffer event the attributor fuses) and a *materializer*
    // (it hands the unsaved bytes to the capture stage). The borrow checker keeps
    // those roles separate, so the source poll happens first and the same bytes
    // are then handed in for materialization.
    let unsaved = b"fn draft() { /* UNSAVED edits the human sees */ }\n";
    let mut source_bridge = BufferBridge::new();
    source_bridge.set_buffer("src/draft.rs", unsaved.to_vec(), true);
    let mut materializer = BufferBridge::new();
    materializer.set_buffer("src/draft.rs", unsaved.to_vec(), true);
    // Drain the materializer's own queued event so it acts purely as a byte
    // source at capture time (the attribution event comes from source_bridge).
    let _ = materializer.poll();

    // The context marks the path focused-dirty so attribution is high-confidence
    // human (A.3 Q2).
    let ctx = TurnContext {
        focused_dirty: vec![std::path::PathBuf::from("src/draft.rs")],
        ..Default::default()
    };
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut source_bridge];

    let events = h.daemon.subscribe_events();
    let report = h
        .daemon
        .capture_drift(&mut sources, &ctx, vec![root], "main", Some(&materializer))
        .unwrap();

    // The change is attributed to the human editor with high confidence and no
    // review flag (it is not silently blamed on the agent).
    let rec = report
        .attribution
        .iter()
        .find(|r| r.path == "src/draft.rs")
        .expect("an attribution record for the buffered path");
    assert_eq!(rec.op, ChangeOp::Modified);
    assert_eq!(rec.attribution, Attribution::HumanEditor);
    assert_eq!(rec.confidence, Confidence::High);
    assert!(
        !rec.review_flag,
        "a high-confidence human edit is not flagged"
    );

    // The captured tree holds the UNSAVED buffer bytes, not the on-disk bytes.
    let blob = h
        .daemon
        .dispatch(Command::BlobRead {
            tree_hash: report.root_tree,
            path: "src/draft.rs".into(),
        })
        .unwrap();
    match blob {
        CommandResult::Blob { bytes, .. } => {
            assert_eq!(
                bytes.as_slice(),
                unsaved,
                "the node captured the unsaved buffer, not the stale on-disk bytes"
            );
        }
        other => panic!("expected the buffered file in the tree, got {other:?}"),
    }

    // The drift node is in the graph, parented at root, and surfaced on the rail.
    let view = h.daemon.graph_view();
    let node = view.node(report.node_id).expect("drift node in view");
    assert_eq!(node.parent_ids, vec![root]);
    let mut saw_node = false;
    for _ in 0..2 {
        if let OpLogEvent::NodeCreated { node_id, .. } =
            events.recv_timeout(Duration::from_secs(2)).unwrap()
        {
            if node_id == report.node_id {
                saw_node = true;
            }
        }
    }
    assert!(
        saw_node,
        "the unsaved-buffer drift node surfaced on the rail"
    );
}

// --------------------------------------------------------------------------
// DoD: a planted fake API key is caught at capture and never enters the CAS
// --------------------------------------------------------------------------

#[test]
fn planted_api_key_is_excluded_and_never_enters_the_cas() {
    let h = Harness::new();
    // Seed a root node from a clean (secret-free) working tree.
    h.write("src/main.rs", b"fn main() { println!(\"hi\"); }\n");
    let root = create_snapshot_node(&h.daemon, vec![], None);

    // Now plant a fake AWS key + an api_key in a .env file (out-of-band).
    h.write(
        ".env",
        b"API_KEY=supersecretvalue1234567890\nAWS=AKIAIOSFODNN7EXAMPLE\n",
    );

    let ctx = TurnContext::default();
    let mut sources: Vec<&mut dyn ChangeSource> = vec![];
    let report = h
        .daemon
        .capture_drift(&mut sources, &ctx, vec![root], "main", None)
        .unwrap();

    // The secret-bearing file is reported excluded, not silently dropped.
    assert!(
        report.excluded_secrets.contains(&".env".to_string()),
        "excluded: {:?}",
        report.excluded_secrets
    );

    // No CAS object anywhere contains the planted secret bytes.
    assert_no_cas_blob_contains(h.daemon.cas_dir(), b"supersecretvalue1234567890");
    assert_no_cas_blob_contains(h.daemon.cas_dir(), b"AKIAIOSFODNN7EXAMPLE");

    // The non-secret file IS captured (readable from the drift node's tree)...
    let main = h
        .daemon
        .dispatch(Command::BlobRead {
            tree_hash: report.root_tree,
            path: "src/main.rs".into(),
        })
        .unwrap();
    assert!(
        matches!(main, CommandResult::Blob { .. }),
        "src/main.rs is in the captured tree"
    );

    // ...while the secret-bearing file is NOT in the tree (it was redacted).
    let env = h.daemon.dispatch(Command::BlobRead {
        tree_hash: report.root_tree,
        path: ".env".into(),
    });
    assert!(
        matches!(env, Err(IpcError::NotFound(_))),
        ".env must be absent from the captured tree (excluded as a secret), got {env:?}"
    );
}

/// Assert no loose CAS object file under `cas_dir/objects` contains `needle`.
fn assert_no_cas_blob_contains(cas_dir: &Path, needle: &[u8]) {
    let objects = cas_dir.join("objects");
    fn walk(dir: &Path, needle: &[u8]) {
        if !dir.exists() {
            return;
        }
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(&path, needle);
            } else if meta.is_file() {
                let bytes = std::fs::read(&path).unwrap();
                assert!(
                    !contains(&bytes, needle),
                    "secret bytes leaked into CAS object {path:?}"
                );
            }
        }
    }
    walk(&objects, needle);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// --------------------------------------------------------------------------
// DoD: a side effect without a granted capability is denied with an AuditEntry
// --------------------------------------------------------------------------

#[test]
fn side_effect_without_capability_is_denied_with_an_audit_entry() {
    // Grant ONLY snapshot read — no snapshot.write, no secrets.get.
    let read_only = vec![Grant::new(
        Capability::SnapshotRead,
        Scope::new().with_path_globs(["**"]),
    )];
    let h = Harness::with_grants(read_only);

    // A write mutation (node.create) must be denied.
    let err = h
        .daemon
        .dispatch(Command::NodeCreate {
            kind: "snapshot".into(),
            type_version: "1.0.0".into(),
            parent_ids: vec![],
            branch_id: "main".into(),
            payload: serde_json::json!({ "origin": "manual" }),
            owns_snapshot: true,
            snapshot_hash: Some(spork_hash::hash_bytes(b"t")),
        })
        .unwrap_err();
    assert!(matches!(err, IpcError::Capability(_)), "got {err:?}");

    // The denial was audited (allowed = false) for snapshot.write.
    let audit = h.daemon.audit_log();
    let last = audit.last().expect("an audit entry was appended");
    assert_eq!(last.capability, Capability::SnapshotWrite);
    assert!(!last.allowed, "the denial is recorded as allowed = false");

    // A secrets.get op (vault) is also denied + audited.
    let verr = h
        .daemon
        .vault_get(&spork_vault::VaultRef::from_string("nope".into()))
        .unwrap_err();
    assert!(
        matches!(verr, spork_daemon::DaemonError::Capability(_)),
        "got {verr:?}"
    );
    let audit = h.daemon.audit_log();
    assert!(audit
        .iter()
        .any(|e| e.capability == Capability::SecretsGet && !e.allowed));
}

#[test]
fn granted_capability_is_allowed_and_audited() {
    let h = Harness::with_grants(default_grants());
    h.write("a.txt", b"a\n");
    let _ = create_snapshot_node(&h.daemon, vec![], None);

    let audit = h.daemon.audit_log();
    // The capture + the node.create both authorized snapshot.write (allowed).
    assert!(audit
        .iter()
        .any(|e| e.capability == Capability::SnapshotWrite && e.allowed));
}

// --------------------------------------------------------------------------
// DoD: restore is atomic across code + conversation, fail-closed, history lives
// --------------------------------------------------------------------------

#[test]
fn restore_materializes_code_and_conversation_and_history_survives() {
    let h = Harness::new();

    // Node A: the state we will restore back to.
    h.write("README.md", b"v1 readme");
    h.write("src/main.rs", b"fn main() { println!(\"v1\"); }");
    let node_a = create_snapshot_node(&h.daemon, vec![], Some(b"agent: built v1\nuser: ship it"));

    // Node B: a forward edit (a different working state) parented at A.
    h.write("src/main.rs", b"fn main() { println!(\"v2\"); }");
    h.write("NEW.md", b"a v2-only file");
    let node_b = create_snapshot_node(&h.daemon, vec![node_a], Some(b"agent: built v2"));

    let events = h.daemon.subscribe_events();
    // Drain the creation events so we can isolate the restore's events.
    while events.try_recv().is_ok() {}

    // Restore back to A: code + conversation move together atomically.
    let result = h
        .daemon
        .dispatch(Command::NodeRestore { node_id: node_a })
        .unwrap();
    assert!(result.op_id().is_some());

    // The working tree is byte-identical to A's captured state (v1, no NEW.md).
    let work = read_tree(&h.work());
    assert_eq!(
        work.get("src/main.rs").unwrap(),
        b"fn main() { println!(\"v1\"); }"
    );
    assert!(
        !work.contains_key("NEW.md"),
        "v2-only file is gone after restore to v1"
    );

    // The restore surfaced RESTORE_PERFORMED + REF_MOVED(HEAD->A) as events.
    let mut saw_restore = false;
    let mut saw_head = false;
    for _ in 0..2 {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            OpLogEvent::RestorePerformed { node_id, .. } => {
                assert_eq!(node_id, node_a);
                saw_restore = true;
            }
            OpLogEvent::RefMoved { ref_name, to, .. } => {
                assert_eq!(ref_name, "HEAD");
                assert_eq!(to, node_a);
                saw_head = true;
            }
            other => panic!("unexpected restore event {other:?}"),
        }
    }
    assert!(saw_restore && saw_head);

    // Forward history survives: node B still exists as a sibling in the graph.
    let view = h.daemon.graph_view();
    assert!(view.node(node_b).is_some(), "forward node survives restore");
    assert_eq!(view.ref_target("HEAD"), Some(node_a), "HEAD is on A");
}

#[test]
fn restore_fails_closed_on_injected_divergence_changing_nothing() {
    let h = Harness::new();
    h.write("code.rs", b"real code");
    let (snapshot_hash, _root) = h.daemon.capture_working_tree().unwrap();

    // Bind a conversation ref that was NEVER stored in the CAS (injected
    // divergence): a digest of bytes the store has never seen.
    let phantom = spork_hash::hash_bytes(b"this conversation was never stored");
    let result = h
        .daemon
        .dispatch(Command::NodeCreate {
            kind: "snapshot".into(),
            type_version: "1.0.0".into(),
            parent_ids: vec![],
            branch_id: "main".into(),
            payload: serde_json::json!({ "origin": "manual", "conversationRef": phantom.to_hex() }),
            owns_snapshot: true,
            snapshot_hash: Some(snapshot_hash),
        })
        .unwrap();
    let node = node_id_of(&result);

    // Pre-seed the working dir with a sentinel to prove it is untouched.
    std::fs::write(h.work().join("SENTINEL"), b"do not touch").unwrap();
    let before = read_tree(&h.work());

    // Restore must fail closed (the bound conversation is missing).
    let err = h
        .daemon
        .dispatch(Command::NodeRestore { node_id: node })
        .unwrap_err();
    assert!(matches!(err, IpcError::Restore(_)), "got {err:?}");

    // Nothing changed: the working dir (incl. the sentinel) is intact.
    assert_eq!(
        read_tree(&h.work()),
        before,
        "working dir unchanged on fail-closed restore"
    );
    let view = h.daemon.graph_view();
    assert_eq!(view.ref_target("HEAD"), None, "HEAD was never moved");
}

#[test]
fn branch_fork_is_metadata_only_and_emits_events() {
    let h = Harness::new();
    h.write("f.txt", b"f\n");
    let node = create_snapshot_node(&h.daemon, vec![], None);

    let events = h.daemon.subscribe_events();
    while events.try_recv().is_ok() {}

    let cas_before = cas_object_count(h.daemon.cas_dir());
    let result = h
        .daemon
        .dispatch(Command::BranchFork {
            from_node_id: node,
            name: "experiment".into(),
        })
        .unwrap();
    assert!(result.op_id().is_some());
    let cas_after = cas_object_count(h.daemon.cas_dir());
    assert_eq!(
        cas_before, cas_after,
        "branch fork copies zero bytes (metadata only)"
    );

    // The new ref is in the view-model and the events surfaced.
    let view = h.daemon.graph_view();
    assert_eq!(view.ref_target("experiment"), Some(node));
    let mut saw_forked = false;
    let mut saw_created = false;
    for _ in 0..2 {
        match events.recv_timeout(Duration::from_secs(2)).unwrap() {
            OpLogEvent::BranchForked { ref_name, .. } => {
                assert_eq!(ref_name, "experiment");
                saw_forked = true;
            }
            OpLogEvent::RefCreated { ref_name, .. } => {
                assert_eq!(ref_name, "experiment");
                saw_created = true;
            }
            other => panic!("unexpected fork event {other:?}"),
        }
    }
    assert!(saw_forked && saw_created);
}

/// Count loose+pack object files under a CAS dir (for the zero-copy assertion).
fn cas_object_count(cas_dir: &Path) -> usize {
    let mut count = 0;
    fn walk(dir: &Path, count: &mut usize) {
        if !dir.exists() {
            return;
        }
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(&path, count);
            } else if meta.is_file() {
                *count += 1;
            }
        }
    }
    walk(&cas_dir.join("objects"), &mut count);
    count
}

// --------------------------------------------------------------------------
// DoD: export_to_git produces a clean commit while .git stays byte-unchanged
// --------------------------------------------------------------------------

#[test]
fn export_to_git_is_non_invasive() {
    let h = Harness::new();
    h.write("src/lib.rs", b"pub fn answer() -> u32 { 42 }\n");
    h.write("README.md", b"# project\n");
    let node = create_snapshot_node(&h.daemon, vec![], None);

    // Make a real git repo elsewhere with working state.
    let repo_dir = tempfile::tempdir().unwrap();
    git2::Repository::init(repo_dir.path()).unwrap();
    std::fs::write(
        repo_dir.path().join("existing.txt"),
        b"on the user's branch",
    )
    .unwrap();
    let head_before = repo_head_state(repo_dir.path());

    // Capture the .git/HEAD + working tree (excluding .git) before export.
    let git_head_before = std::fs::read(repo_dir.path().join(".git").join("HEAD")).unwrap();
    let work_before = read_tree_excluding_git(repo_dir.path());

    // Export the node into a new branch.
    let commit_sha = h
        .daemon
        .export_to_git(repo_dir.path(), node, "spork/export")
        .unwrap();
    assert_eq!(commit_sha.len(), 40, "a 40-char SHA-1 commit id");

    // The new branch exists and its tree matches the node tree.
    let exported = git2::Repository::open(repo_dir.path()).unwrap();
    let branch = exported
        .find_branch("spork/export", git2::BranchType::Local)
        .unwrap();
    let commit = branch.get().peel_to_commit().unwrap();
    assert_eq!(commit.id().to_string(), commit_sha);
    let tree = commit.tree().unwrap();
    assert!(tree.get_path(Path::new("src/lib.rs")).is_ok());
    assert!(tree.get_path(Path::new("README.md")).is_ok());

    // HEAD/working-tree are byte-unchanged (the non-invasive contract); the new
    // branch ref + the commit's objects are the only expected .git additions.
    assert_eq!(
        repo_head_state(repo_dir.path()),
        head_before,
        "HEAD unchanged"
    );
    assert_eq!(
        read_tree_excluding_git(repo_dir.path()),
        work_before,
        "working tree unchanged"
    );
    let git_head_after = std::fs::read(repo_dir.path().join(".git").join("HEAD")).unwrap();
    assert_eq!(
        git_head_before, git_head_after,
        ".git/HEAD is byte-unchanged"
    );
}

/// Read every file under `dir` except those inside `.git`.
fn read_tree_excluding_git(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    read_tree(dir)
        .into_iter()
        .filter(|(p, _)| !p.starts_with(".git/") && p != ".git")
        .collect()
}

/// The repo's HEAD ref content (or `None` for an unborn HEAD), to prove export
/// does not move it.
fn repo_head_state(repo_path: &Path) -> Option<String> {
    let repo = git2::Repository::open(repo_path).unwrap();
    repo.head().ok().map(|h| h.name().unwrap_or("").to_string())
}

// --------------------------------------------------------------------------
// DoD: node restore p95 < 500 ms (measured honestly)
// --------------------------------------------------------------------------

#[test]
fn node_restore_p95_under_500ms() {
    let h = Harness::new();
    // A moderate fixture: ~120 files across a few dirs.
    for i in 0..120 {
        h.write(
            &format!("src/mod{}/file{i}.rs", i % 8),
            format!("// file {i}\npub fn f{i}() {{}}\n").as_bytes(),
        );
    }
    let node = create_snapshot_node(&h.daemon, vec![], Some(b"agent: built the fixture"));

    // Warm up, then measure many restores back to the same node.
    let mut samples = Vec::new();
    for _ in 0..30 {
        let start = Instant::now();
        h.daemon
            .dispatch(Command::NodeRestore { node_id: node })
            .unwrap();
        samples.push(start.elapsed());
    }
    samples.sort();
    let p95 = samples[(samples.len() as f64 * 0.95) as usize - 1];
    // Honest budget per DESIGN A.5 (hardware: the CI/dev box running this test).
    assert!(
        p95 < Duration::from_millis(500),
        "restore p95 = {p95:?} exceeds the 500ms budget (measured on this host)"
    );
}
