//! End-to-end integration tests driving the built `spork-cas` binary.
//!
//! These exercise the CLI exactly as the perf / definition-of-done harness does:
//! by invoking the compiled binary as a subprocess (via Cargo's
//! `CARGO_BIN_EXE_spork-cas` env var) and asserting on its stdout, exit code,
//! and side effects on a real on-disk store. They cover the DoD demos this crate
//! owns: a generated corpus round-trips, a warm re-capture writes zero new
//! objects, and editing one file re-stores only that file's path (DESIGN.md §10,
//! §14.5).

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Absolute path to the freshly built `spork-cas` binary under test.
const BIN: &str = env!("CARGO_BIN_EXE_spork-cas");

/// Run the binary with `args`, returning its captured output.
fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("failed to spawn spork-cas")
}

/// Run the binary, assert it exited `0`, and return stdout as a UTF-8 string.
fn run_ok(args: &[&str]) -> String {
    let out = run(args);
    assert!(
        out.status.success(),
        "expected success for {args:?}, got status {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("stdout was not UTF-8")
}

/// Parse a binary invocation's `--json` stdout into a JSON value.
fn run_json(args: &[&str]) -> Value {
    let stdout = run_ok(args);
    serde_json::from_str(stdout.trim()).expect("stdout was not valid JSON")
}

/// Helper: stringify a path for passing as an argument.
fn p(path: &Path) -> &str {
    path.to_str().expect("path is valid UTF-8")
}

#[test]
fn gen_fixture_is_deterministic_across_runs() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let ja = run_json(&[
        "gen-fixture",
        p(&a.path().join("c")),
        "--files",
        "200",
        "--seed",
        "9",
        "--json",
    ]);
    let jb = run_json(&[
        "gen-fixture",
        p(&b.path().join("c")),
        "--files",
        "200",
        "--seed",
        "9",
        "--json",
    ]);
    assert_eq!(ja["files"], jb["files"]);
    assert_eq!(ja["bytes"], jb["bytes"]);

    // The two corpora must be byte-identical (same relative paths and bytes).
    let mut sa = collect(&a.path().join("c"));
    let mut sb = collect(&b.path().join("c"));
    sa.sort();
    sb.sort();
    assert_eq!(sa, sb, "same seed must reproduce identical corpus");
}

#[test]
fn put_tree_then_verify_roundtrip_passes() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("fixture");
    let store = work.path().join("store");

    run_ok(&["gen-fixture", p(&fixture), "--files", "300", "--seed", "1"]);
    let put = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    assert!(put["snapshot"].as_str().unwrap().len() == 64);
    assert!(put["new_objects"].as_u64().unwrap() > 0);
    assert!(put["new_chunks"].as_u64().unwrap() > 0);

    // verify-roundtrip must exit 0 and report ok.
    let out = run(&[
        "verify-roundtrip",
        p(&fixture),
        "--store",
        p(&store),
        "--json",
    ]);
    assert!(out.status.success(), "verify-roundtrip should exit 0");
    let report: Value = serde_json::from_slice(out.stdout.trim_ascii_end()).unwrap();
    assert_eq!(report["ok"], Value::Bool(true));
    assert!(report["files_checked"].as_u64().unwrap() > 0);
    assert_eq!(report["mismatches"].as_array().unwrap().len(), 0);
}

#[test]
fn warm_recapture_writes_zero_new_objects() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("fixture");
    let store = work.path().join("store");

    run_ok(&["gen-fixture", p(&fixture), "--files", "250", "--seed", "5"]);
    // Cold capture into the fresh store.
    let cold = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    assert!(cold["new_objects"].as_u64().unwrap() > 0);
    assert!(cold["new_chunks"].as_u64().unwrap() > 0);

    // Warm re-capture into the same store must write nothing new.
    let warm = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    assert_eq!(
        cold["snapshot"], warm["snapshot"],
        "identical tree => identical snapshot"
    );
    assert_eq!(warm["new_objects"].as_u64().unwrap(), 0);
    assert_eq!(warm["new_chunks"].as_u64().unwrap(), 0);
    assert_eq!(warm["bytes_written"].as_u64().unwrap(), 0);
    assert!(warm["reused_objects"].as_u64().unwrap() > 0);
}

#[test]
fn chunk_stats_reports_dedup_on_warm_pass() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("fixture");
    let store = work.path().join("store");

    run_ok(&["gen-fixture", p(&fixture), "--files", "200", "--seed", "2"]);
    let out = run(&["chunk-stats", p(&fixture), "--store", p(&store), "--json"]);
    assert!(
        out.status.success(),
        "chunk-stats should exit 0 when dedup holds"
    );
    let report: Value = serde_json::from_slice(out.stdout.trim_ascii_end()).unwrap();
    assert_eq!(report["deduped"], Value::Bool(true));
    // The cold pass (fresh store) writes objects; the warm pass writes none.
    assert!(report["cold"]["new_objects"].as_u64().unwrap() > 0);
    assert_eq!(report["warm"]["new_objects"].as_u64().unwrap(), 0);
    assert_eq!(report["warm"]["new_chunks"].as_u64().unwrap(), 0);
}

#[test]
fn editing_one_file_restores_only_that_files_path() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("fixture");
    let store = work.path().join("store");

    // A corpus with at least one large file so the edit hits a multi-chunk blob.
    run_ok(&["gen-fixture", p(&fixture), "--files", "120", "--seed", "8"]);
    let cold = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    assert!(cold["new_chunks"].as_u64().unwrap() > 1);

    // Flip one region of a large binary file (a single content-defined chunk).
    let target = fixture.join("large").join("blob_000.bin");
    let mut bytes = std::fs::read(&target).unwrap();
    let mid = bytes.len() / 2;
    for b in &mut bytes[mid..mid + 16] {
        *b ^= 0xFF;
    }
    std::fs::write(&target, &bytes).unwrap();

    // Re-capture: exactly one new chunk (the edited region), and only the edited
    // blob plus its ancestor trees (large/ and root) are new objects.
    let warm = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    assert_eq!(
        warm["new_chunks"].as_u64().unwrap(),
        1,
        "a single-region edit must re-store exactly one chunk: {warm}"
    );
    // New objects on the path-to-root: the edited blob, its two ancestor trees
    // (large/ and the root), and the snapshot (whose root_tree changed) — at most
    // four. Every other blob and the untouched sibling subtree are reused.
    assert!(
        warm["new_objects"].as_u64().unwrap() <= 4,
        "only the edited blob, its ancestor trees, and the snapshot should change: {warm}"
    );
    assert!(warm["reused_chunks"].as_u64().unwrap() > 0);
    assert!(warm["reused_objects"].as_u64().unwrap() > 0);

    // And the new snapshot differs from the old (content changed).
    assert_ne!(cold["snapshot"], warm["snapshot"]);
}

#[test]
fn cat_round_trips_blob_tree_and_snapshot() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("fixture");
    let store = work.path().join("store");

    // A small, fully reproducible corpus.
    run_ok(&["gen-fixture", p(&fixture), "--files", "40", "--seed", "4"]);
    let put = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    let snapshot = put["snapshot"].as_str().unwrap();
    let root_tree = put["root_tree"].as_str().unwrap();

    // cat <snapshot> -> JSON whose root_tree matches.
    let snap_json: Value =
        serde_json::from_str(&run_ok(&["cat", snapshot, "--store", p(&store)])).unwrap();
    assert_eq!(snap_json["root_tree"].as_str().unwrap(), root_tree);
    assert_eq!(snap_json["v"], 1);

    // cat <tree> -> JSON with entries.
    let tree_json: Value =
        serde_json::from_str(&run_ok(&["cat", root_tree, "--store", p(&store)])).unwrap();
    assert!(tree_json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["name"] == "large"));

    // Find a file blob inside the `large` subtree and cat it back to bytes.
    let large_target = tree_json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "large")
        .unwrap()["target"]
        .as_str()
        .unwrap()
        .to_string();
    let large_tree: Value =
        serde_json::from_str(&run_ok(&["cat", &large_target, "--store", p(&store)])).unwrap();
    let first_entry = &large_tree["entries"].as_array().unwrap()[0];
    let blob_hash = first_entry["target"].as_str().unwrap();
    let file_name = first_entry["name"].as_str().unwrap();

    // cat <blob> reassembles the original file bytes.
    let out = run(&["cat", blob_hash, "--store", p(&store)]);
    assert!(out.status.success());
    let original = std::fs::read(fixture.join("large").join(file_name)).unwrap();
    assert_eq!(
        out.stdout, original,
        "cat of a blob must reproduce the file bytes"
    );
}

#[test]
fn cat_forced_kind_overrides_detection() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("fixture");
    let store = work.path().join("store");
    run_ok(&["gen-fixture", p(&fixture), "--files", "10", "--seed", "0"]);
    let put = run_json(&["put-tree", p(&fixture), "--store", p(&store), "--json"]);
    let root_tree = put["root_tree"].as_str().unwrap();

    // Forcing --kind tree on a tree works; forcing --kind blob fails to decode.
    run_ok(&["cat", root_tree, "--store", p(&store), "--kind", "tree"]);
    let bad = run(&["cat", root_tree, "--store", p(&store), "--kind", "blob"]);
    assert!(!bad.status.success(), "a tree read as a blob must error");
}

#[test]
fn cat_unknown_hash_is_an_error() {
    let work = TempDir::new().unwrap();
    let store = work.path().join("store");
    // Open the store first so the directory exists.
    run_ok(&[
        "gen-fixture",
        p(&work.path().join("f")),
        "--files",
        "1",
        "--seed",
        "0",
    ]);
    run_ok(&[
        "put-tree",
        p(&work.path().join("f")),
        "--store",
        p(&store),
        "--json",
    ]);

    let missing = "0".repeat(64);
    let out = run(&["cat", &missing, "--store", p(&store)]);
    assert!(
        !out.status.success(),
        "cat of a missing object must exit non-zero"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("not found"));
}

#[test]
fn put_tree_on_nonexistent_dir_errors() {
    let work = TempDir::new().unwrap();
    let out = run(&["put-tree", p(&work.path().join("does-not-exist"))]);
    assert!(!out.status.success());
}

#[test]
fn put_tree_rejects_malformed_git_parent() {
    let work = TempDir::new().unwrap();
    let fixture = work.path().join("f");
    let store = work.path().join("store");
    run_ok(&["gen-fixture", p(&fixture), "--files", "5", "--seed", "0"]);
    let out = run(&[
        "put-tree",
        p(&fixture),
        "--store",
        p(&store),
        "--git-parent",
        "not-a-sha1",
    ]);
    assert!(
        !out.status.success(),
        "a malformed git parent must be rejected"
    );
}

/// Recursively collect `(relative_path, bytes)` for every file under `root`.
fn collect(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in walkdir(root) {
        let rel = entry
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        out.push((rel, std::fs::read(&entry).unwrap()));
    }
    out
}

/// A tiny recursive file lister (the integration test has no walkdir dep of its
/// own, so this keeps the helper self-contained).
fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}
