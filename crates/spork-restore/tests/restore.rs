//! Integration tests for `spork-restore`: the atomic dual-restore guard and the
//! metadata-only branch fork, exercised against a *real* content store, graph
//! service, and working directory (DESIGN §6.3, §6.4, §10.1, §10.3, §11.4).
//!
//! These tests are the crate's slice of the F3 Definition of Done:
//! - restore materializes code byte-identically **and** resolves the bound
//!   conversation atomically;
//! - an injected missing/mismatched conversation (or snapshot) fails closed —
//!   the working directory and refs are left intact;
//! - after a restore the previously-forward node still exists (forward history
//!   survives as a sibling, restore being an event not an overwrite);
//! - `branch_fork` copies zero bytes;
//! - restore p95 < 500 ms on a moderate fixture (measured honestly).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use spork_cas::{LooseStore, ObjectStore};
use spork_graph::{GraphService, RefKind, BUILTIN_SNAPSHOT_KIND};
use spork_hash::Hash;
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use spork_log::EventLog;
use spork_restore::{RestoreError, RestoreGuard};
use tempfile::TempDir;
use ulid::Ulid;

/// A full restore harness: a temp root holding the CAS, the event log, the
/// working directory the restore materializes into, and a fixture-source dir.
struct Harness {
    _root: TempDir,
    cas_dir: std::path::PathBuf,
    log: EventLog,
    workdir: std::path::PathBuf,
    src_dir: std::path::PathBuf,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let cas_dir = root.path().join("cas");
        std::fs::create_dir_all(&cas_dir).unwrap();
        let log = EventLog::open(&root.path().join("log.db")).unwrap();
        let workdir = root.path().join("work");
        let src_dir = root.path().join("src-fixture");
        std::fs::create_dir_all(&src_dir).unwrap();
        Harness {
            _root: root,
            cas_dir,
            log,
            workdir,
            src_dir,
        }
    }

    /// Open a fresh object store over this harness's CAS directory.
    fn store(&self) -> ObjectStore<LooseStore> {
        ObjectStore::new(LooseStore::open(&self.cas_dir).unwrap())
    }

    /// Build a graph service over the harness's log, with the built-in snapshot
    /// type registered (so snapshot nodes can be created).
    fn graph(&self) -> GraphService {
        let mut svc = GraphService::open_in_memory(self.log.writer()).unwrap();
        svc.register_builtin_snapshot().unwrap();
        svc
    }
}

/// Write a tree of files under `dir`, given `(relative path, contents)` pairs.
fn write_fixture(dir: &Path, files: &[(&str, &[u8])]) {
    for (rel, bytes) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }
}

/// Read every file under `dir` into a sorted `path -> bytes` map (for byte-exact
/// comparison of a materialized tree against its source).
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
                    .into_owned();
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

/// Capture `src` into the CAS through the default ignore profile and return the
/// snapshot hash.
fn capture(store: &ObjectStore<LooseStore>, src: &Path) -> Hash {
    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);
    let (snap, _root, _stats) = store
        .capture_snapshot(src, &matcher, profile.hash(), None)
        .unwrap();
    snap
}

/// Store conversation bytes in the CAS and return the blob hash used as the
/// node's bound conversation ref.
fn put_conversation(store: &ObjectStore<LooseStore>, bytes: &[u8]) -> Hash {
    let (hash, _stats) = store.put_blob_bytes(bytes).unwrap();
    hash
}

/// Create a built-in snapshot node bound to a code snapshot and (optionally) a
/// conversation ref in its payload, returning the node id.
fn make_node(
    graph: &mut GraphService,
    parents: Vec<Ulid>,
    snapshot: Hash,
    conversation: Option<Hash>,
) -> Ulid {
    let payload = match conversation {
        Some(c) => json!({ "origin": "manual", "conversationRef": c.to_hex() }),
        None => json!({ "origin": "manual" }),
    };
    graph
        .create_node(
            BUILTIN_SNAPSHOT_KIND,
            None,
            parents,
            "main",
            payload,
            true,
            Some(snapshot),
        )
        .unwrap()
        .id
}

// ---- DoD: restore materializes code + resolves conversation atomically -------

#[test]
fn restore_materializes_code_and_resolves_conversation() {
    let h = Harness::new();
    let store = h.store();

    write_fixture(
        &h.src_dir,
        &[
            ("README.md", b"v1 readme"),
            ("src/main.rs", b"fn main() { println!(\"v1\"); }"),
            (
                "src/util/mod.rs",
                b"pub fn add(a: u32, b: u32) -> u32 { a + b }",
            ),
        ],
    );
    let snapshot = capture(&store, &h.src_dir);
    let conversation = put_conversation(&store, b"agent: built v1\nuser: ship it");

    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], snapshot, Some(conversation));

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let outcome = guard.restore(node).unwrap();

    // The outcome reports both restored refs and an empty effects log (§11.4).
    assert_eq!(outcome.node_id, node);
    assert_eq!(outcome.restored_snapshot, snapshot);
    assert_eq!(outcome.restored_conversation, Some(conversation));
    assert!(outcome.external_effects.is_empty());

    // The working tree is byte-identical to the captured source (the §10.1
    // "click a node, see the exact state" invariant).
    assert_eq!(read_tree(&h.workdir), read_tree(&h.src_dir));

    // HEAD now points at the restored node.
    guard.with_graph(|g| {
        assert_eq!(g.projection().ref_target("HEAD").unwrap(), Some(node));
    });
}

#[test]
fn restore_works_for_a_node_with_no_bound_conversation() {
    let h = Harness::new();
    let store = h.store();
    write_fixture(&h.src_dir, &[("a.txt", b"only code, no chat")]);
    let snapshot = capture(&store, &h.src_dir);

    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], snapshot, None);

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let outcome = guard.restore(node).unwrap();
    assert_eq!(outcome.restored_conversation, None);
    assert_eq!(read_tree(&h.workdir), read_tree(&h.src_dir));
}

// ---- DoD: fail closed on injected divergence — nothing changes ---------------

#[test]
fn restore_fails_closed_on_missing_conversation() {
    let h = Harness::new();
    let store = h.store();
    write_fixture(&h.src_dir, &[("code.rs", b"real code")]);
    let snapshot = capture(&store, &h.src_dir);

    // Bind a conversation ref that was NEVER stored in the CAS (injected
    // divergence): a digest of bytes the store has never seen.
    let phantom_conversation = spork_hash::hash_bytes(b"this conversation was never stored");

    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], snapshot, Some(phantom_conversation));

    // Pre-seed the working dir with a sentinel so we can prove it is untouched.
    std::fs::create_dir_all(&h.workdir).unwrap();
    std::fs::write(h.workdir.join("SENTINEL"), b"do not touch").unwrap();
    let before = read_tree(&h.workdir);

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let err = guard.restore(node).unwrap_err();

    // It is precisely the missing-conversation divergence...
    match err {
        RestoreError::MissingConversation(got) => assert_eq!(got, phantom_conversation),
        other => panic!("expected MissingConversation, got {other:?}"),
    }
    // ...and NOTHING changed: the working dir is byte-for-byte as before, and
    // HEAD was never moved.
    assert_eq!(read_tree(&h.workdir), before);
    guard.with_graph(|g| {
        assert_eq!(g.projection().ref_target("HEAD").unwrap(), None);
    });
}

#[test]
fn restore_fails_closed_on_malformed_conversation_ref() {
    let h = Harness::new();
    let store = h.store();
    write_fixture(&h.src_dir, &[("code.rs", b"real code")]);
    let snapshot = capture(&store, &h.src_dir);

    let mut graph = h.graph();
    // A node whose conversationRef is present but not a valid hex digest.
    let node = graph
        .create_node(
            BUILTIN_SNAPSHOT_KIND,
            None,
            vec![],
            "main",
            json!({ "origin": "manual", "conversationRef": "not-a-hash" }),
            true,
            Some(snapshot),
        )
        .unwrap()
        .id;

    std::fs::create_dir_all(&h.workdir).unwrap();
    let before = read_tree(&h.workdir);

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let err = guard.restore(node).unwrap_err();
    assert!(matches!(err, RestoreError::Divergence { .. }));
    assert_eq!(read_tree(&h.workdir), before);
}

#[test]
fn restore_fails_closed_on_missing_snapshot() {
    let h = Harness::new();
    let store = h.store();

    // A node that claims a snapshot hash the CAS does not hold.
    let phantom_snapshot = spork_hash::hash_bytes(b"a snapshot that was never captured");
    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], phantom_snapshot, None);

    std::fs::create_dir_all(&h.workdir).unwrap();
    std::fs::write(h.workdir.join("SENTINEL"), b"keep me").unwrap();
    let before = read_tree(&h.workdir);

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let err = guard.restore(node).unwrap_err();
    match err {
        RestoreError::MissingSnapshot(got) => assert_eq!(got, phantom_snapshot),
        other => panic!("expected MissingSnapshot, got {other:?}"),
    }
    assert_eq!(read_tree(&h.workdir), before);
}

#[test]
fn restore_of_unknown_node_is_divergence() {
    let h = Harness::new();
    let store = h.store();
    let graph = h.graph();
    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let err = guard.restore(Ulid::new()).unwrap_err();
    assert!(matches!(err, RestoreError::Divergence { .. }));
    // No working dir was created.
    assert!(!h.workdir.exists());
}

// ---- DoD: forward history survives a restore as a sibling --------------------

#[test]
fn forward_history_survives_restore() {
    let h = Harness::new();
    let store = h.store();

    // v1 -> v2 (forward). Restore v1; v2 must still exist afterwards.
    write_fixture(&h.src_dir, &[("f.txt", b"v1")]);
    let snap_v1 = capture(&store, &h.src_dir);
    let conv_v1 = put_conversation(&store, b"chat v1");

    let mut graph = h.graph();
    let v1 = make_node(&mut graph, vec![], snap_v1, Some(conv_v1));

    // v2 captures a different content (forward of v1).
    write_fixture(&h.src_dir, &[("f.txt", b"v2 - newer")]);
    let snap_v2 = capture(&store, &h.src_dir);
    let conv_v2 = put_conversation(&store, b"chat v2");
    let v2 = make_node(&mut graph, vec![v1], snap_v2, Some(conv_v2));

    // HEAD starts at v2.
    graph.create_ref("HEAD", RefKind::Head, v2).unwrap();

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let outcome = guard.restore(v1).unwrap();
    assert_eq!(outcome.restored_snapshot, snap_v1);

    // The working tree is v1's content.
    assert_eq!(
        read_tree(&h.workdir).get("f.txt").unwrap().as_slice(),
        b"v1"
    );

    guard.with_graph(|g| {
        // HEAD moved back to v1...
        assert_eq!(g.projection().ref_target("HEAD").unwrap(), Some(v1));
        // ...but v2 (the previously-forward node) still exists as a sibling: the
        // restore was an event, not an overwrite (DESIGN §6.4).
        assert!(g.get_node(v2).unwrap().is_some());
        // And v2 still has v1 as its parent — lineage is intact.
        assert_eq!(g.get_node(v2).unwrap().unwrap().parent_ids, vec![v1]);
    });
}

#[test]
fn restore_is_recorded_as_an_event() {
    let h = Harness::new();
    let store = h.store();
    write_fixture(&h.src_dir, &[("x", b"data")]);
    let snapshot = capture(&store, &h.src_dir);
    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], snapshot, None);

    let log_len_before = h.log.writer().len().unwrap();
    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    guard.restore(node).unwrap();

    // The restore appended events: a ref move/create for HEAD plus the
    // restore.performed op. The log grew, and the chain is intact on replay.
    let log_len_after = h.log.writer().len().unwrap();
    assert!(log_len_after > log_len_before);

    let reader = h.log.reader().unwrap();
    let saw_restore = reader
        .iter_from(0)
        .unwrap()
        .map(|e| e.unwrap())
        .any(|e| e.event_type == spork_restore::EVENT_RESTORE_PERFORMED);
    assert!(saw_restore, "a restore.performed event must be in the log");
}

// ---- DoD: branch_fork is metadata-only (zero bytes copied) -------------------

#[test]
fn branch_fork_copies_zero_bytes() {
    let h = Harness::new();
    let store = h.store();
    write_fixture(&h.src_dir, &[("a", b"x"), ("b", b"y")]);
    let snapshot = capture(&store, &h.src_dir);
    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], snapshot, None);

    // Object-store size on disk before the fork.
    let bytes_before = dir_size(&h.cas_dir);

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let ref_id = guard.branch_fork(node, "branch/feature-x").unwrap();
    assert_eq!(ref_id.as_str(), "branch/feature-x");

    // Not a single byte was added to the content store: a fork is a pure
    // metadata move (DESIGN §6.3).
    assert_eq!(dir_size(&h.cas_dir), bytes_before);
    // And the working directory was never materialized.
    assert!(!h.workdir.exists());

    // The new ref points at the source node.
    guard.with_graph(|g| {
        assert_eq!(
            g.projection().ref_target("branch/feature-x").unwrap(),
            Some(node)
        );
    });
}

#[test]
fn branch_fork_of_unknown_node_is_divergence() {
    let h = Harness::new();
    let store = h.store();
    let graph = h.graph();
    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());
    let err = guard.branch_fork(Ulid::new(), "branch/nope").unwrap_err();
    assert!(matches!(err, RestoreError::Divergence { .. }));
}

/// Recursively sum the size of every regular file under `dir`.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if !dir.exists() {
        return 0;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let meta = std::fs::symlink_metadata(entry.path()).unwrap();
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

// ---- DoD: restore p95 < 500 ms on a moderate fixture (measured honestly) -----

#[test]
fn restore_p95_under_500ms_on_moderate_fixture() {
    let h = Harness::new();
    let store = h.store();

    // A moderate fixture: 400 small files across a few directories.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..400u32 {
        let dir = i % 8;
        files.push((
            format!("pkg{dir}/file_{i:04}.rs"),
            format!("// file {i}\npub fn f{i}() -> u32 {{ {i} }}\n").into_bytes(),
        ));
    }
    let file_refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    write_fixture(&h.src_dir, &file_refs);

    let snapshot = capture(&store, &h.src_dir);
    let conversation = put_conversation(&store, b"a moderate conversation transcript");
    let mut graph = h.graph();
    let node = make_node(&mut graph, vec![], snapshot, Some(conversation));

    let guard = RestoreGuard::new(graph, store, h.workdir.clone(), h.log.writer());

    // Warm-up (filesystem caches), then measure repeated restores.
    guard.restore(node).unwrap();
    let mut samples = Vec::new();
    for _ in 0..20 {
        let start = std::time::Instant::now();
        guard.restore(node).unwrap();
        samples.push(start.elapsed());
    }
    samples.sort();
    // p95 of 20 samples is the 19th (index 18, 0-based) — the slowest below max.
    let p95 = samples[(samples.len() as f64 * 0.95) as usize - 1];
    assert!(
        p95 < std::time::Duration::from_millis(500),
        "restore p95 was {p95:?} (budget 500 ms; measured on a 400-file moderate fixture, \
         CoW-absent dev sandbox so this is a conservative copy-path number)"
    );

    // Correctness still holds after the measured loop.
    assert_eq!(read_tree(&h.workdir), read_tree(&h.src_dir));
}
