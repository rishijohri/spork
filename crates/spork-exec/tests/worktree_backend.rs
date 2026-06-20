//! End-to-end tests for the v1 [`WorktreeCowBackend`] over the real
//! `spork-cas` / `spork-asset` stack (DESIGN.md §5.3, §11.1, §11.3, §4.10).
//!
//! These prove the F4 definition-of-done for the execution substrate:
//! `provision` materializes a snapshot **byte-identically** and mounts an asset
//! dependency **read-only**; `exec` runs against the CoW copy (never the user
//! checkout); `capture_path` content-addresses a path back into the store;
//! `teardown` removes the worktree; a lapsed lease refuses mutation; and a
//! path-escaping `cwd`/capture is refused.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use spork_asset::{AssetKey, AssetStore, LocalCasAssetStore};
use spork_cas::{LooseStore, ObjectStore};
use spork_exec::{
    AssetMount, CancelToken, Clock, EnvManifest, ExecError, IsolationBackend, LeaseLedger,
    PreparedRun, SandboxTier, WorktreeCowBackend,
};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use tempfile::TempDir;

/// A fake clock whose "now" is shared via `Arc` so a test keeps a handle to
/// advance it after handing a clone to the backend. Drives lease TTLs
/// deterministically without sleeping.
#[derive(Debug, Clone)]
struct FakeClock {
    now: Arc<AtomicU64>,
}
impl FakeClock {
    fn new(now: u64) -> Self {
        FakeClock {
            now: Arc::new(AtomicU64::new(now)),
        }
    }
    fn set(&self, now: u64) {
        self.now.store(now, Ordering::SeqCst);
    }
}
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

fn build_project(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), b"[package]\nname=\"demo\"\n").unwrap();
    fs::write(
        root.join("src/main.rs"),
        b"fn main() { println!(\"hi\"); }\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), b"# Demo\nSome FIXME here.\n").unwrap();
}

fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            collect(base, &path, out);
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
}

/// A fully-wired backend over real stores, plus the handles a test needs.
struct Harness {
    _tmp: TempDir,
    backend: WorktreeCowBackend<LooseStore, LocalCasAssetStore, FakeClock>,
    clock: FakeClock,
    snapshot: spork_hash::Hash,
    source_root: PathBuf,
    asset_content: Vec<u8>,
    asset_key: AssetKey,
}

/// Build a harness; `with_mount` controls whether the ingested opaque asset is
/// configured as a read-only mount on the backend.
fn harness(with_mount: bool, clock_now: u64) -> Harness {
    let tmp = TempDir::new().unwrap();

    // Capture a snapshot of a project tree into a CAS object store.
    let source_root = tmp.path().join("checkout");
    build_project(&source_root);
    let objects = ObjectStore::new(LooseStore::open(tmp.path().join("cas")).unwrap());
    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);
    let (snapshot, _root_tree, _stats) = objects
        .capture_snapshot(&source_root, &matcher, profile.hash(), None)
        .unwrap();

    // Ingest an opaque asset into a separate asset store.
    let assets = LocalCasAssetStore::open(tmp.path().join("assets")).unwrap();
    let asset_src = tmp.path().join("weights.bin");
    let asset_content = b"opaque model weights v1".to_vec();
    fs::write(&asset_src, &asset_content).unwrap();
    let content_hash = assets.content_hash_for_source(&asset_src).unwrap();
    let asset_key = AssetKey::opaque(content_hash);
    assets.ensure(&asset_key, Some(&asset_src)).unwrap();

    let clock = FakeClock::new(clock_now);
    let ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
    let mut backend = WorktreeCowBackend::with_clock(
        objects,
        assets,
        tmp.path().join("worktrees"),
        ledger,
        clock.clone(),
    )
    .with_lease_params(60_000, 5_000);
    if with_mount {
        backend =
            backend.with_asset_mount(AssetMount::new(asset_key.clone(), "models/weights.bin"));
    }

    Harness {
        _tmp: tmp,
        backend,
        clock,
        snapshot,
        source_root,
        asset_content,
        asset_key,
    }
}

#[test]
fn backend_reports_worktree_cow_tier() {
    let h = harness(false, 0);
    assert_eq!(h.backend.tier(), SandboxTier::WorktreeCow);
    // The ingested asset key is exposed for completeness.
    assert!(matches!(
        h.asset_key.kind,
        spork_asset::AssetKind::Opaque { .. }
    ));
}

#[test]
fn provision_materializes_a_snapshot_byte_identically() {
    let h = harness(false, 1_000);
    let env = EnvManifest::new();
    let ws = h.backend.provision(h.snapshot, &env).unwrap();

    // Every kept file is byte-identical to the original checkout.
    let mut original = Vec::new();
    let mut materialized = Vec::new();
    collect(&h.source_root, &h.source_root, &mut original);
    collect(&ws.root, &ws.root, &mut materialized);
    original.sort();
    materialized.sort();
    assert_eq!(
        original, materialized,
        "provision must reproduce the snapshot byte-for-byte"
    );

    // The workspace records the identity it was provisioned from.
    assert_eq!(ws.snapshot, h.snapshot);
    assert_eq!(ws.env_manifest_hash, env.env_manifest_hash().unwrap());

    h.backend.teardown(ws).unwrap();
}

#[test]
fn provision_mounts_asset_deps_read_only() {
    let h = harness(true, 2_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();

    let mounted = ws.root.join("models/weights.bin");
    assert!(
        mounted.exists(),
        "asset dep is materialized at its mount path"
    );
    assert_eq!(
        fs::read(&mounted).unwrap(),
        h.asset_content,
        "asset bytes match the ingested content"
    );
    assert!(
        fs::metadata(&mounted).unwrap().permissions().readonly(),
        "asset deps are mounted READ-ONLY (DESIGN §10.5)"
    );

    // The snapshot files are still present alongside the mounted dep.
    assert!(ws.root.join("Cargo.toml").exists());

    h.backend.teardown(ws).unwrap();
}

#[test]
fn exec_runs_in_the_worktree_and_returns_raw_output() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();

    let run = PreparedRun::new(
        "/bin/sh",
        ["-c".to_string(), "echo hello-from-ws".to_string()],
    );
    let out = h.backend.exec(&ws, run, &CancelToken::new()).unwrap();
    assert!(out.success(), "exit 0");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello-from-ws");

    let ls = PreparedRun::new("/bin/sh", ["-c".to_string(), "ls".to_string()]);
    let out = h.backend.exec(&ws, ls, &CancelToken::new()).unwrap();
    let listing = String::from_utf8_lossy(&out.stdout);
    assert!(listing.contains("Cargo.toml"), "cwd is the worktree root");

    h.backend.teardown(ws).unwrap();
}

#[test]
fn exec_env_overlay_is_visible_to_the_command() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let run = PreparedRun::new(
        "/bin/sh",
        ["-c".to_string(), "printf %s \"$SPORK_TEST\"".to_string()],
    )
    .with_env("SPORK_TEST", "overlay-value");
    let out = h.backend.exec(&ws, run, &CancelToken::new()).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "overlay-value");
    h.backend.teardown(ws).unwrap();
}

#[test]
fn exec_in_a_relative_subdir_runs_there() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let run = PreparedRun::new(
        "/bin/sh",
        ["-c".to_string(), "basename \"$(pwd)\"".to_string()],
    )
    .with_cwd("src");
    let out = h.backend.exec(&ws, run, &CancelToken::new()).unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "src");
    h.backend.teardown(ws).unwrap();
}

#[test]
fn exec_reports_nonzero_exit_as_success_not_error() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let run = PreparedRun::new("/bin/sh", ["-c".to_string(), "exit 3".to_string()]);
    let out = h.backend.exec(&ws, run, &CancelToken::new()).unwrap();
    assert_eq!(out.exit_code, Some(3));
    assert!(!out.success());
    h.backend.teardown(ws).unwrap();
}

#[test]
fn exec_against_a_cancelled_token_is_refused() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let run = PreparedRun::new("/bin/sh", ["-c".to_string(), "echo nope".to_string()]);
    let err = h.backend.exec(&ws, run, &cancel).unwrap_err();
    assert!(matches!(err, ExecError::Cancelled));
    h.backend.teardown(ws).unwrap();
}

#[test]
fn exec_with_an_escaping_cwd_is_refused() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let run =
        PreparedRun::new("/bin/sh", ["-c".to_string(), "pwd".to_string()]).with_cwd("../../etc");
    let err = h.backend.exec(&ws, run, &CancelToken::new()).unwrap_err();
    assert!(matches!(err, ExecError::PathEscape { .. }));
    h.backend.teardown(ws).unwrap();
}

#[test]
fn capture_path_content_addresses_a_file_and_round_trips() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();

    // Write a result file inside the worktree (the CoW copy), then capture it.
    fs::write(ws.root.join("result.txt"), b"the result bytes").unwrap();
    let hash = h
        .backend
        .capture_path(&ws, Path::new("result.txt"))
        .unwrap();

    // The captured blob reads back byte-identically from the object store.
    let bytes = h.backend.objects().read_blob(&hash).unwrap();
    assert_eq!(bytes, b"the result bytes");

    // Capturing the same content again yields the same content-addressed id
    // (the capture is a pure function of the bytes).
    fs::write(ws.root.join("result2.txt"), b"the result bytes").unwrap();
    let hash2 = h
        .backend
        .capture_path(&ws, Path::new("result2.txt"))
        .unwrap();
    assert_eq!(
        hash, hash2,
        "identical content => identical content address"
    );

    h.backend.teardown(ws).unwrap();
}

#[test]
fn capture_path_can_capture_a_subtree() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    // The `src` subtree exists from the snapshot; capture it as a tree.
    let tree = h.backend.capture_path(&ws, Path::new("src")).unwrap();
    // Materialize it elsewhere and confirm the file came along.
    let dest = h._tmp.path().join("captured-src");
    h.backend.objects().materialize_tree(&tree, &dest).unwrap();
    assert!(dest.join("main.rs").exists());
    h.backend.teardown(ws).unwrap();
}

#[test]
fn capture_path_outside_the_workspace_is_refused() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let err = h
        .backend
        .capture_path(&ws, Path::new("../escape.txt"))
        .unwrap_err();
    assert!(matches!(err, ExecError::PathEscape { .. }));
    h.backend.teardown(ws).unwrap();
}

#[test]
fn teardown_removes_the_worktree_and_releases_the_lease() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let root = ws.root.clone();
    let lease_id = ws.lease.id;
    assert!(root.exists());

    h.backend.teardown(ws).unwrap();
    assert!(!root.exists(), "worktree directory removed");
    h.backend
        .with_ledger(|l| assert!(l.workspace_root(&lease_id).is_none()));
}

#[test]
fn mutating_a_workspace_with_a_lapsed_lease_is_refused() {
    let h = harness(false, 1_000);
    let ws = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();

    // While the lease is live, exec works.
    let ok = PreparedRun::new("/bin/sh", ["-c".to_string(), "true".to_string()]);
    assert!(h.backend.exec(&ws, ok, &CancelToken::new()).is_ok());

    // Advance the shared clock well past the 60s TTL — the lease lapses.
    h.clock.set(1_000 + 120_000);

    let run = PreparedRun::new("/bin/sh", ["-c".to_string(), "true".to_string()]);
    let err = h.backend.exec(&ws, run, &CancelToken::new()).unwrap_err();
    assert!(matches!(err, ExecError::LeaseNotHeld { .. }));

    // Capture is refused too.
    fs::write(ws.root.join("r.txt"), b"x").unwrap();
    let err = h.backend.capture_path(&ws, Path::new("r.txt")).unwrap_err();
    assert!(matches!(err, ExecError::LeaseNotHeld { .. }));

    // Teardown still works (it does not require a live lease).
    h.backend.teardown(ws).unwrap();
}

#[test]
fn two_provisions_get_distinct_worktrees() {
    let h = harness(false, 1_000);
    let a = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    let b = h
        .backend
        .provision(h.snapshot, &EnvManifest::new())
        .unwrap();
    assert_ne!(a.root, b.root, "each provision gets its own worktree");
    assert_ne!(a.lease.id, b.lease.id, "each gets its own lease");
    h.backend.teardown(a).unwrap();
    h.backend.teardown(b).unwrap();
}
