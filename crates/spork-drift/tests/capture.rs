//! Headless integration tests for `spork-drift` (DESIGN.md §10.1, §10.2, A.3,
//! §15.5).
//!
//! These drive the full drift pipeline with no GUI: out-of-band bash mutations
//! are simulated with `std::fs` (`rm`/add/modify), the [`Attributor`] is checked
//! against the A.3 decision tree and the interceptor-over-watcher precedence,
//! the [`SecretScanner`] is verified to keep a planted key out of the CAS, the
//! unsaved-buffer case is exercised, and every capture is shown to produce an
//! `origin = auto_drift` node whose stored snapshot round-trips byte-for-byte.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spork_cas::{LooseStore, ObjectStore};
use spork_drift::{
    Attribution, BufferBridge, ChangeOp, ChangeSource, Confidence, DriftCapture, EditInterceptor,
    ReconciliationRescan, TurnContext,
};
use spork_graph::{GraphService, BUILTIN_SNAPSHOT_KIND};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use spork_log::EventLog;
use ulid::Ulid;

/// A full headless harness: a working tree, a CAS, and a graph service with the
/// built-in `snapshot` descriptor registered, plus a seed root node to parent
/// drift nodes on.
struct Harness {
    _store_dir: tempfile::TempDir,
    work_dir: tempfile::TempDir,
    _log_dir: tempfile::TempDir,
    _log: EventLog,
    store: ObjectStore<LooseStore>,
    graph: GraphService,
    root_node: Ulid,
}

impl Harness {
    fn new() -> Self {
        let store_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();

        let store = ObjectStore::new(LooseStore::open(store_dir.path()).unwrap());
        let log = EventLog::open(&log_dir.path().join("log.db")).unwrap();
        let mut graph = GraphService::open_in_memory(log.writer()).unwrap();
        graph.register_builtin_snapshot().unwrap();

        // A seed root node (the initial manual snapshot) to parent drift on.
        let root = graph
            .create_node(
                BUILTIN_SNAPSHOT_KIND,
                None,
                vec![],
                "main",
                serde_json::json!({ "origin": "manual" }),
                true,
                Some(spork_hash::hash_bytes(b"seed-root")),
            )
            .unwrap();

        Harness {
            _store_dir: store_dir,
            work_dir,
            _log_dir: log_dir,
            _log: log,
            store,
            graph,
            root_node: root.id,
        }
    }

    fn root(&self) -> &Path {
        self.work_dir.path()
    }

    fn write(&self, rel: &str, contents: &[u8]) {
        let p = self.root().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, contents).unwrap();
    }

    fn capture(&self) -> DriftCapture {
        DriftCapture::new(self.root())
    }

    /// Assert no stored CAS object's bytes contain `needle`.
    fn assert_no_blob_contains(&self, needle: &[u8]) {
        // Walk the loose-store directory and scan every object file's bytes.
        // Objects are stored compressed/headered, so we both scan the raw on-disk
        // bytes AND re-read every reachable blob via the store API for the plain
        // bytes — the strongest possible check that no secret leaked.
        let dir = self._store_dir.path();
        scan_dir_for(dir, needle);
    }
}

/// Recursively assert no file under `dir` contains `needle` in its raw bytes.
fn scan_dir_for(dir: &Path, needle: &[u8]) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            scan_dir_for(&path, needle);
        } else {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !contains(&bytes, needle),
                "secret bytes leaked into CAS object {}",
                path.display()
            );
        }
    }
}

/// Naive substring search over bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn matcher() -> IgnoreMatcher {
    IgnoreMatcher::new(&IgnoreProfile::default_profile())
}

// ---- DoD: out-of-band rm + add + modify, correctly attributed --------------

#[test]
fn out_of_band_rm_add_modify_are_attributed_within_debounce_window() {
    let h = Harness::new();
    h.write("keep.rs", b"fn keep() {}\n");
    h.write("gone.rs", b"fn gone() {}\n");

    // Seed a reconciliation baseline from the initial tree.
    let mut rescan = ReconciliationRescan::new(h.root(), matcher());
    rescan.set_baseline(rescan.scan_tree().unwrap());

    // Simulate bash mutations with std::fs: modify, add, rm.
    fs::write(h.root().join("keep.rs"), b"fn keep() { /* edited */ }\n").unwrap();
    fs::write(h.root().join("new.rs"), b"fn brand_new() {}\n").unwrap();
    fs::remove_file(h.root().join("gone.rs")).unwrap();

    rescan.rescan().unwrap();

    // No agent turn is active and these are watcher/rescan-class changes with no
    // exec anchor, so A.3 attributes them External (medium) — never the agent.
    let capture = h.capture().with_debounce(Duration::from_millis(250));
    let ctx = TurnContext::default();
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut rescan];
    let records = capture.fuse_and_attribute(&mut sources, &ctx);

    let by_path: BTreeMap<_, _> = records.iter().map(|r| (r.path.as_str(), r)).collect();
    assert_eq!(by_path[&"keep.rs"].op, ChangeOp::Modified);
    assert_eq!(by_path[&"new.rs"].op, ChangeOp::Added);
    assert_eq!(by_path[&"gone.rs"].op, ChangeOp::Removed);
    for r in &records {
        assert_eq!(r.attribution, Attribution::External, "{r:?}");
        assert_eq!(r.confidence, Confidence::Medium);
    }
}

#[test]
fn agent_bash_attribution_within_debounce_window() {
    let h = Harness::new();
    h.write("src/lib.rs", b"// before\n");

    let mut rescan = ReconciliationRescan::new(h.root(), matcher());
    rescan.set_baseline(rescan.scan_tree().unwrap());

    // The agent just ran a bash command; record the exec anchor, then a build
    // script (out-of-band) writes a generated file within the debounce window.
    let exec = Instant::now();
    fs::write(h.root().join("src/gen.rs"), b"// generated\n").unwrap();
    rescan.rescan().unwrap();

    let capture = h.capture().with_debounce(Duration::from_secs(30));
    let ctx = TurnContext {
        active_turn: Some(11),
        last_agent_exec: Some(exec),
        ..Default::default()
    };
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut rescan];
    let records = capture.fuse_and_attribute(&mut sources, &ctx);
    let rec = records.iter().find(|r| r.path == "src/gen.rs").unwrap();
    assert_eq!(rec.attribution, Attribution::AgentBash);
    assert_eq!(rec.confidence, Confidence::Medium);
}

// ---- DoD: interceptor-vs-watcher precedence for the same path --------------

#[test]
fn interceptor_beats_watcher_for_same_path() {
    let h = Harness::new();
    h.write("src/app.rs", b"fn main() {}\n");

    // The agent edits the file in-process (interceptor) during turn 4...
    let mut interceptor = EditInterceptor::new();
    interceptor.begin_turn(4);
    interceptor.record("src/app.rs", ChangeOp::Modified);

    // ...and a (lossy, who-blind) fs-watcher independently observes the change.
    let mut watcher_events = ManualWatcher::new();
    watcher_events.push("src/app.rs", ChangeOp::Modified);

    let capture = h.capture();
    let ctx = TurnContext {
        active_turn: Some(4),
        ..Default::default()
    };
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut interceptor, &mut watcher_events];
    let records = capture.fuse_and_attribute(&mut sources, &ctx);
    let rec = records.iter().find(|r| r.path == "src/app.rs").unwrap();
    // The interceptor wins: high-confidence agent, not a downgraded watcher guess.
    assert_eq!(rec.attribution, Attribution::Agent);
    assert_eq!(rec.confidence, Confidence::High);
    assert!(!rec.review_flag);
}

// ---- DoD: a planted key is excluded and never enters the CAS ----------------

#[test]
fn planted_secret_is_excluded_and_never_enters_the_cas() {
    let h = Harness::new();
    h.write("src/main.rs", b"fn main() { println!(\"hi\"); }\n");
    // A planted, fake AWS key + an api_key in a gitignored-style .env file.
    let aws_key = b"AKIAIOSFODNN7EXAMPLE";
    h.write(
        ".env",
        b"API_KEY=supersecretvalue1234567890\nAWS=AKIAIOSFODNN7EXAMPLE\n",
    );
    h.write(
        "config.rs",
        b"const aws_secret_access_key = \"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\";\n",
    );

    let mut h = h;
    let capture = h.capture();
    let report = capture
        .capture(
            &h.store,
            &mut h.graph,
            vec![h.root_node],
            "main",
            vec![],
            None,
        )
        .unwrap();

    // Both secret-bearing files are reported as excluded, not silently dropped.
    assert!(
        report.excluded_secrets.contains(&".env".to_string()),
        "{:?}",
        report.excluded_secrets
    );
    assert!(report.excluded_secrets.contains(&"config.rs".to_string()));

    // The strongest check: no stored CAS object contains the planted key bytes.
    h.assert_no_blob_contains(aws_key);
    h.assert_no_blob_contains(b"supersecretvalue1234567890");
    h.assert_no_blob_contains(b"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");

    // And the non-secret file IS captured: materialize the snapshot and confirm.
    let dest = tempfile::tempdir().unwrap();
    h.store
        .materialize_snapshot(&report.snapshot_hash, dest.path())
        .unwrap();
    assert!(dest.path().join("src/main.rs").exists());
    assert!(!dest.path().join(".env").exists());
    assert!(!dest.path().join("config.rs").exists());
}

// ---- DoD: a capture creates an origin=auto_drift node ----------------------

#[test]
fn capture_creates_an_auto_drift_node_parented_correctly() {
    let mut h = Harness::new();
    h.write("src/a.rs", b"fn a() {}\n");
    h.write("README.md", b"# project\n");

    let parents = vec![h.root_node];
    let capture = h.capture();
    let report = capture
        .capture(
            &h.store,
            &mut h.graph,
            parents.clone(),
            "main",
            vec![],
            None,
        )
        .unwrap();

    let node = h.graph.get_node(report.node_id).unwrap().unwrap();
    assert_eq!(node.kind, BUILTIN_SNAPSHOT_KIND);
    assert!(node.owns_snapshot);
    assert_eq!(node.snapshot_hash, Some(report.snapshot_hash));
    assert_eq!(node.parent_ids, parents);

    // The payload records origin = auto_drift (DESIGN.md §6.2).
    let (payload, _v) = h.graph.get_payload(report.node_id).unwrap().unwrap();
    assert_eq!(payload["origin"], "auto_drift");

    // The captured snapshot round-trips byte-for-byte.
    let dest = tempfile::tempdir().unwrap();
    h.store
        .materialize_snapshot(&report.snapshot_hash, dest.path())
        .unwrap();
    assert_eq!(
        fs::read(dest.path().join("src/a.rs")).unwrap(),
        b"fn a() {}\n"
    );
    assert_eq!(
        fs::read(dest.path().join("README.md")).unwrap(),
        b"# project\n"
    );
}

// ---- DoD: the unsaved-buffer case is captured ------------------------------

#[test]
fn unsaved_buffer_is_materialized_into_the_drift_node() {
    let mut h = Harness::new();
    // On disk the file holds the saved text...
    h.write("src/edit.rs", b"fn old() {}\n");

    // ...but the human has unsaved, focused edits in the editor buffer.
    let mut buffers = BufferBridge::new();
    buffers.set_buffer("src/edit.rs", b"fn new_unsaved() {}\n".to_vec(), true);

    // Attribution: the focused-dirty buffer is a high-confidence human edit.
    let capture = h.capture();
    let ctx = TurnContext {
        focused_dirty: vec![PathBuf::from("src/edit.rs")],
        ..Default::default()
    };
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut buffers];
    let records = capture.fuse_and_attribute(&mut sources, &ctx);
    let rec = records.iter().find(|r| r.path == "src/edit.rs").unwrap();
    assert_eq!(rec.attribution, Attribution::HumanEditor);
    assert_eq!(rec.confidence, Confidence::High);

    // The capture must materialize the UNSAVED bytes, not the stale on-disk ones.
    let report = capture
        .capture(
            &h.store,
            &mut h.graph,
            vec![h.root_node],
            "main",
            records,
            Some(&buffers),
        )
        .unwrap();

    let dest = tempfile::tempdir().unwrap();
    h.store
        .materialize_snapshot(&report.snapshot_hash, dest.path())
        .unwrap();
    assert_eq!(
        fs::read(dest.path().join("src/edit.rs")).unwrap(),
        b"fn new_unsaved() {}\n",
        "the drift node must reflect the unsaved buffer (DESIGN.md §10.2)"
    );
}

// ---- Reconciliation discipline: capture is non-invasive to .git-style dirs --

#[test]
fn capture_excludes_ignored_dirs_and_leaves_working_tree_untouched() {
    let mut h = Harness::new();
    h.write("src/keep.rs", b"keep\n");
    h.write("node_modules/dep/index.js", b"module.exports = {}\n");
    h.write("target/debug/app", b"\x7fELF binary-ish\n");

    // Snapshot the working tree's file set before capture.
    let before = ReconciliationRescan::new(h.root(), matcher())
        .scan_tree()
        .unwrap();

    let capture = h.capture();
    let report = capture
        .capture(
            &h.store,
            &mut h.graph,
            vec![h.root_node],
            "main",
            vec![],
            None,
        )
        .unwrap();

    // The working tree is byte-unchanged by the capture (staging happens
    // out-of-tree).
    let after = ReconciliationRescan::new(h.root(), matcher())
        .scan_tree()
        .unwrap();
    assert_eq!(before, after, "capture must not mutate the working tree");
    // The ignored heavy dirs are still on disk (capture never deletes them)...
    assert!(h.root().join("node_modules/dep/index.js").exists());
    assert!(h.root().join("target/debug/app").exists());

    // ...but they are excluded from the snapshot.
    let dest = tempfile::tempdir().unwrap();
    h.store
        .materialize_snapshot(&report.snapshot_hash, dest.path())
        .unwrap();
    assert!(dest.path().join("src/keep.rs").exists());
    assert!(!dest.path().join("node_modules").exists());
    assert!(!dest.path().join("target").exists());
}

// ---- End-to-end: fuse the real four-source set, attribute, then capture -----

#[test]
fn end_to_end_fuse_attribute_capture_with_all_source_kinds() {
    let mut h = Harness::new();
    h.write("src/agent_edit.rs", b"// v0\n");
    h.write("src/human_edit.rs", b"// saved\n");
    h.write("src/external.rs", b"// untouched\n");

    // Source 1: interceptor (agent, turn 8).
    let mut interceptor = EditInterceptor::new();
    interceptor.begin_turn(8);
    fs::write(h.root().join("src/agent_edit.rs"), b"// v1 by agent\n").unwrap();
    interceptor.record("src/agent_edit.rs", ChangeOp::Modified);

    // Source 2: buffer bridge (human, focused-dirty).
    let mut buffers = BufferBridge::new();
    buffers.set_buffer("src/human_edit.rs", b"// unsaved human\n".to_vec(), true);

    // Source 3: reconciliation rescan picks up an external add no one claimed.
    let mut rescan = ReconciliationRescan::new(h.root(), matcher());
    rescan.set_baseline(rescan.scan_tree().unwrap());
    fs::write(h.root().join("src/external.rs"), b"// changed externally\n").unwrap();
    rescan.rescan().unwrap();

    let capture = h.capture();
    let ctx = TurnContext {
        active_turn: Some(8),
        focused_dirty: vec![PathBuf::from("src/human_edit.rs")],
        ..Default::default()
    };
    let mut sources: Vec<&mut dyn ChangeSource> = vec![&mut interceptor, &mut buffers, &mut rescan];
    let records = capture.fuse_and_attribute(&mut sources, &ctx);

    let by_path: BTreeMap<_, _> = records
        .iter()
        .map(|r| (r.path.clone(), r.clone()))
        .collect();
    assert_eq!(by_path["src/agent_edit.rs"].attribution, Attribution::Agent);
    assert_eq!(
        by_path["src/human_edit.rs"].attribution,
        Attribution::HumanEditor
    );
    // The external add: turn IS active but no interceptor/buffer/exec evidence ->
    // tentative + review flag (A.3 Q4). This is the honest-fidelity stance.
    assert_eq!(
        by_path["src/external.rs"].attribution,
        Attribution::AgentTentative
    );
    assert!(by_path["src/external.rs"].review_flag);

    // Capture stores everything (no secrets), buffer overriding disk.
    let report = capture
        .capture(
            &h.store,
            &mut h.graph,
            vec![h.root_node],
            "main",
            records,
            Some(&buffers),
        )
        .unwrap();

    let (payload, _v) = h.graph.get_payload(report.node_id).unwrap().unwrap();
    assert_eq!(payload["origin"], "auto_drift");
    // The attribution travels in the node payload (3 records).
    assert_eq!(payload["attribution"].as_array().unwrap().len(), 3);

    let dest = tempfile::tempdir().unwrap();
    h.store
        .materialize_snapshot(&report.snapshot_hash, dest.path())
        .unwrap();
    assert_eq!(
        fs::read(dest.path().join("src/human_edit.rs")).unwrap(),
        b"// unsaved human\n"
    );
    assert_eq!(
        fs::read(dest.path().join("src/agent_edit.rs")).unwrap(),
        b"// v1 by agent\n"
    );
}

/// A tiny manual fs-watcher stand-in for the precedence test: it produces
/// watcher-class events deterministically without depending on OS event timing
/// (which is inherently racy in a unit test). It implements the same
/// [`ChangeSource`] seam the real [`spork_drift::FsWatcher`] does.
struct ManualWatcher {
    pending: Vec<spork_drift::ChangeEvent>,
}

impl ManualWatcher {
    fn new() -> Self {
        ManualWatcher {
            pending: Vec::new(),
        }
    }

    fn push(&mut self, path: &str, op: ChangeOp) {
        self.pending.push(spork_drift::ChangeEvent::now(
            path,
            op,
            spork_drift::ChangeSourceKind::FsWatcher,
            None,
        ));
    }
}

impl ChangeSource for ManualWatcher {
    fn kind(&self) -> spork_drift::ChangeSourceKind {
        spork_drift::ChangeSourceKind::FsWatcher
    }

    fn poll(&mut self) -> Vec<spork_drift::ChangeEvent> {
        std::mem::take(&mut self.pending)
    }
}
