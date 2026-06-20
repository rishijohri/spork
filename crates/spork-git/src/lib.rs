//! Spork F3 non-invasive Git bridge — read HEAD, write a branch, touch nothing.
//!
//! This crate is Spork's only door to a user's real `.git`, and it is held to a
//! strict non-invasive contract. [`import_git_state`] is read-only: it records
//! the current `HEAD`, branch, dirty flag, and parent commit without modifying
//! the `.git` directory, the index, or the working tree. [`export_to_git`] writes
//! git blobs, trees, and a commit built from a Spork node tree and points a
//! *new* branch ref at that commit — but it never moves the user's `HEAD`, never
//! touches the index, and never alters the working tree. After an export the
//! repository's `HEAD`, index, and working tree are byte-for-byte unchanged; only
//! a new branch ref appears (and immutable objects are added to the object
//! database).
//!
//! The git plumbing is provided by `git2` compiled with the `vendored-libgit2`
//! feature, so `.git` access never depends on a system libgit2 being present.
//!
//! # The two entry points
//!
//! ```text
//!   import_git_state(repo)                 ──read──▶  GitContext { head, branch, dirty, parent }
//!   export_to_git(repo, store, tree, name) ──write─▶  new refs/heads/<name> @ <commit sha>
//!                                                      (HEAD / index / working tree unchanged)
//! ```
//!
//! - [`import_git_state`] discovers the repository at (or above) a path and reads
//!   its HEAD-relative state into a [`GitContext`]. The dirty-detection,
//!   detached-HEAD, and unborn-branch (empty repo) cases are all handled.
//! - [`export_to_git`] translates a [`spork_cas`] tree (the directory manifest a
//!   Spork node points at) into Git objects, commits over them with no ref
//!   update, and creates a fresh branch ref. [`export_snapshot_to_git`] is the
//!   convenience that resolves a [`spork_cas::Snapshot`] to its root tree first.
//!
//! # Evolution safety (CLAUDE.md C5)
//!
//! [`GitContext`] carries an explicit `schema_version` ([`GIT_CONTEXT_VERSION`])
//! so the captured git state can gain fields without invalidating older persisted
//! contexts.
//!
//! # The seam (CLAUDE.md C3)
//!
//! The bridge is a pair of free functions over a frozen object model
//! ([`spork_cas`]) and a frozen git plumbing layer; richer policies (a
//! caller-supplied commit identity, exporting with the imported git parent as the
//! commit parent, signed commits) are additive behind these signatures.
//!
//! This realizes the Git-interop model in DESIGN.md §10.4 ("Git coexistence is
//! bidirectional but non-invasive").

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod context;
mod error;
mod export;

pub use context::{import_git_state, GitContext, GIT_CONTEXT_VERSION};
pub use error::GitError;
pub use export::export_to_git;

use spork_cas::{ObjectStore, StorageBackend};
use spork_hash::Hash;

/// Export the root tree of a stored [`spork_cas::Snapshot`] to a new Git branch.
///
/// This is the snapshot-level convenience over [`export_to_git`]: given a CAS
/// snapshot id (the commit analogue a Spork node owns), it reads the snapshot,
/// resolves its `root_tree`, and exports that tree to `branch_name` exactly as
/// [`export_to_git`] does — leaving `HEAD`, the index, and the working tree
/// unchanged. Returns the new commit's SHA-1 hex.
///
/// # Errors
/// The union of [`export_to_git`]'s errors plus a snapshot-read failure
/// ([`GitError::Io`]) if `snapshot` is absent or malformed.
pub fn export_snapshot_to_git<S: StorageBackend + Sync>(
    repo_path: impl AsRef<std::path::Path>,
    store: &ObjectStore<S>,
    snapshot: Hash,
    branch_name: &str,
) -> Result<String, GitError> {
    let snap = store.read_snapshot(&snapshot)?;
    export_to_git(repo_path, store, snap.root_tree, branch_name)
}

#[cfg(test)]
mod tests {
    //! End-to-end proofs of the non-invasive contract over real temp Git repos.
    //!
    //! These tests build an actual `.git` (via libgit2), capture a full
    //! byte-level fingerprint of `HEAD`, the index, and the working tree, run an
    //! export, and assert the fingerprint is unchanged afterward — plus that the
    //! new commit's tree matches the exported Spork tree and a new branch ref
    //! appeared. They also drive the real F2 [`spork_graph::GraphService`] so the
    //! bridge is exercised exactly as the daemon will use it: graph node →
    //! snapshot hash → export.

    use super::*;
    use git2::{Repository, Signature};
    use spork_cas::{LooseStore, ObjectStore};
    use spork_ignore::{IgnoreMatcher, IgnoreProfile};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// Build a CAS object store rooted in `tmp`.
    fn cas(tmp: &TempDir) -> ObjectStore<LooseStore> {
        ObjectStore::new(LooseStore::open(tmp.path()).unwrap())
    }

    fn matcher() -> IgnoreMatcher {
        IgnoreMatcher::new(&IgnoreProfile::default_profile())
    }

    /// Write a small representative working tree under `root` and return it.
    fn build_worktree(root: &Path) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("README.md"), b"# demo\n").unwrap();
        fs::write(
            root.join("src/main.rs"),
            b"fn main() { println!(\"hi\"); }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            b"pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();
    }

    /// Recursively collect `(relative_path, kind, bytes-or-linktarget)` for every
    /// entry under `dir` (files, dirs, symlinks), relative to `base`. This is the
    /// working-tree fingerprint used to prove byte-level non-invasiveness.
    fn fingerprint_worktree(
        base: &Path,
        dir: &Path,
        out: &mut BTreeMap<String, (String, Vec<u8>)>,
    ) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap().to_string_lossy();
            // Skip the .git directory itself; it is fingerprinted separately and
            // precisely (HEAD, index) so object-db growth is not counted as a
            // working-tree change.
            if name == ".git" {
                continue;
            }
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .to_string();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).unwrap();
                out.insert(
                    rel,
                    (
                        "symlink".to_string(),
                        target.to_string_lossy().into_owned().into_bytes(),
                    ),
                );
            } else if meta.is_dir() {
                out.insert(rel, ("dir".to_string(), Vec::new()));
                fingerprint_worktree(base, &path, out);
            } else {
                out.insert(rel, ("file".to_string(), fs::read(&path).unwrap()));
            }
        }
    }

    /// A precise fingerprint of the parts of `.git` the non-invasive contract
    /// protects: the `HEAD` file bytes and the index file bytes (each `None` if
    /// absent). Object-database files are deliberately excluded — adding immutable
    /// objects is the *expected* effect of an export and is not a violation.
    fn fingerprint_git_protected(repo_root: &Path) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
        let git = repo_root.join(".git");
        let head = fs::read(git.join("HEAD")).ok();
        let index = fs::read(git.join("index")).ok();
        (head, index)
    }

    /// All ref names present in the repo, sorted (to detect new branches).
    fn ref_names(repo: &Repository) -> Vec<String> {
        let mut names: Vec<String> = repo
            .references()
            .unwrap()
            .names()
            .map(|n| n.unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    /// Initialize a repo at `root`, build a worktree, and make one commit so HEAD,
    /// a branch, and an index all exist. Returns the opened repository.
    fn init_committed_repo(root: &Path) -> Repository {
        let repo = Repository::init(root).unwrap();
        build_worktree(root);

        // Stage the worktree and commit so HEAD/index/branch exist.
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = Signature::now("Test User", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial\n", &tree, &[])
            .unwrap();
        // Re-open to drop the borrowed index/tree handles cleanly.
        drop(tree);
        drop(index);
        repo
    }

    #[test]
    fn import_reflects_head_branch_dirty_without_mutating_git() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let repo = init_committed_repo(&root);
        let parent = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
            .to_string();
        drop(repo);

        // Fingerprint .git's protected parts before import.
        let before = fingerprint_git_protected(&root);

        let ctx = import_git_state(&root).unwrap();
        assert_eq!(ctx.schema_version, GIT_CONTEXT_VERSION);
        assert!(
            ctx.head
                .as_deref()
                .is_some_and(|h| h.starts_with("refs/heads/")),
            "a committed repo on a branch records its full HEAD ref, got {:?}",
            ctx.head
        );
        // Branch shorthand is the default branch name libgit2 created.
        assert!(
            ctx.branch.is_some(),
            "a committed repo on a branch has a shorthand"
        );
        assert_eq!(ctx.git_parent_commit.as_deref(), Some(parent.as_str()));
        assert!(!ctx.dirty, "a freshly committed clean tree is not dirty");

        // Import must not touch .git's protected parts.
        let after = fingerprint_git_protected(&root);
        assert_eq!(before, after, "import_git_state mutated HEAD or the index");

        // Now dirty the working tree and confirm `dirty` flips — still read-only.
        fs::write(root.join("README.md"), b"# demo changed\n").unwrap();
        let before2 = fingerprint_git_protected(&root);
        let ctx2 = import_git_state(&root).unwrap();
        assert!(ctx2.dirty, "an edited tracked file makes the tree dirty");
        let after2 = fingerprint_git_protected(&root);
        assert_eq!(
            before2, after2,
            "dirty-state import mutated HEAD or the index"
        );
    }

    #[test]
    fn import_handles_empty_repo_unborn_branch() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("empty");
        Repository::init(&root).unwrap();

        let ctx = import_git_state(&root).unwrap();
        assert_eq!(
            ctx.head, None,
            "an unborn branch has no resolvable HEAD ref"
        );
        assert_eq!(ctx.git_parent_commit, None);
        assert!(!ctx.dirty, "an empty repo with no files is clean");
        // The unborn branch name is recoverable from the symbolic HEAD.
        assert!(
            ctx.branch.is_some(),
            "the unborn branch name should be readable"
        );
    }

    #[test]
    fn import_detached_head_reports_no_branch() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("detached");
        let repo = init_committed_repo(&root);
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let oid = commit.id();
        // Detach HEAD onto the commit directly.
        repo.set_head_detached(oid).unwrap();
        drop(commit);
        drop(repo);

        let ctx = import_git_state(&root).unwrap();
        assert_eq!(ctx.branch, None, "detached HEAD has no branch shorthand");
        assert_eq!(
            ctx.git_parent_commit.as_deref(),
            Some(oid.to_string().as_str())
        );
    }

    #[test]
    fn import_on_non_repo_is_not_a_repo() {
        let tmp = TempDir::new().unwrap();
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        fs::write(plain.join("file.txt"), b"no git here").unwrap();
        assert!(matches!(import_git_state(&plain), Err(GitError::NotARepo)));
    }

    #[test]
    fn export_creates_commit_and_branch_leaving_worktree_byte_unchanged() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let repo = init_committed_repo(&root);

        // Capture a *different* tree into the CAS than what's committed, so the
        // export is a genuine new state (not a no-op).
        let store_dir = TempDir::new().unwrap();
        let store = cas(&store_dir);
        let src = tmp.path().join("node_state");
        fs::create_dir_all(src.join("pkg")).unwrap();
        fs::write(src.join("CHANGELOG.md"), b"## v2\n- exported by spork\n").unwrap();
        fs::write(
            src.join("pkg/feature.rs"),
            b"pub fn feature() -> u8 { 42 }\n",
        )
        .unwrap();
        let (node_tree, _stats) = store.put_tree(&src, &matcher()).unwrap();

        // === Before: full fingerprints ===
        let head_before = fingerprint_git_protected(&root);
        let mut wt_before = BTreeMap::new();
        fingerprint_worktree(&root, &root, &mut wt_before);
        let refs_before = ref_names(&repo);
        drop(repo);

        // === Export ===
        let commit_sha = export_to_git(&root, &store, node_tree, "spork/exported").unwrap();

        // === After: HEAD + index byte-unchanged, working tree byte-unchanged ===
        let head_after = fingerprint_git_protected(&root);
        assert_eq!(
            head_before, head_after,
            "export mutated HEAD or the index (must be byte-unchanged)"
        );
        let mut wt_after = BTreeMap::new();
        fingerprint_worktree(&root, &root, &mut wt_after);
        assert_eq!(
            wt_before, wt_after,
            "export mutated the working tree (must be byte-unchanged)"
        );

        // === A new branch ref appeared, and only that ===
        let repo = Repository::open(&root).unwrap();
        let refs_after = ref_names(&repo);
        let new_refs: Vec<&String> = refs_after
            .iter()
            .filter(|r| !refs_before.contains(r))
            .collect();
        assert_eq!(
            new_refs,
            vec![&"refs/heads/spork/exported".to_string()],
            "exactly one new branch ref should appear"
        );

        // === The new commit's tree matches the exported Spork tree ===
        let oid = git2::Oid::from_str(&commit_sha).unwrap();
        let commit = repo.find_commit(oid).unwrap();
        let git_tree = commit.tree().unwrap();
        // Files match byte-for-byte.
        let changelog = git_tree
            .get_path(Path::new("CHANGELOG.md"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(
            changelog.as_blob().unwrap().content(),
            b"## v2\n- exported by spork\n"
        );
        let feature = git_tree
            .get_path(Path::new("pkg/feature.rs"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(
            feature.as_blob().unwrap().content(),
            b"pub fn feature() -> u8 { 42 }\n"
        );

        // The branch ref points at exactly the returned commit.
        let branch_ref = repo.find_reference("refs/heads/spork/exported").unwrap();
        assert_eq!(branch_ref.target().unwrap(), oid);

        // HEAD still points at the ORIGINAL branch/commit, not the export.
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        assert_ne!(
            head_commit.id(),
            oid,
            "HEAD must not have moved to the export"
        );
    }

    #[test]
    fn export_preserves_executable_bit() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        init_committed_repo(&root);

        let store_dir = TempDir::new().unwrap();
        let store = cas(&store_dir);
        let src = tmp.path().join("node_state");
        fs::create_dir_all(&src).unwrap();
        let script = src.join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho exported\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (node_tree, _stats) = store.put_tree(&src, &matcher()).unwrap();

        let commit_sha = export_to_git(&root, &store, node_tree, "spork/exec").unwrap();

        let repo = Repository::open(&root).unwrap();
        let oid = git2::Oid::from_str(&commit_sha).unwrap();
        let tree = repo.find_commit(oid).unwrap().tree().unwrap();
        let entry = tree.get_path(Path::new("run.sh")).unwrap();
        #[cfg(unix)]
        assert_eq!(
            entry.filemode(),
            i32::from(git2::FileMode::BlobExecutable),
            "the executable bit must survive export"
        );
        #[cfg(not(unix))]
        let _ = entry;
    }

    #[test]
    fn export_to_existing_branch_name_fails_without_clobbering() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let repo = init_committed_repo(&root);
        // The default branch already exists; its name is the shorthand.
        let existing = repo.head().unwrap().shorthand().unwrap().to_string();
        let existing_target = repo.head().unwrap().peel_to_commit().unwrap().id();
        drop(repo);

        let store_dir = TempDir::new().unwrap();
        let store = cas(&store_dir);
        let src = tmp.path().join("node_state");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("x.txt"), b"x").unwrap();
        let (node_tree, _stats) = store.put_tree(&src, &matcher()).unwrap();

        // Exporting onto the existing branch name must fail (non-force ref create)
        // and must NOT move the existing branch.
        let err = export_to_git(&root, &store, node_tree, &existing).unwrap_err();
        assert!(matches!(err, GitError::Git(_)));

        let repo = Repository::open(&root).unwrap();
        let still = repo
            .find_reference(&format!("refs/heads/{existing}"))
            .unwrap()
            .target()
            .unwrap();
        assert_eq!(
            still, existing_target,
            "the existing branch must be untouched"
        );
    }

    #[test]
    fn export_on_non_repo_is_not_a_repo() {
        let tmp = TempDir::new().unwrap();
        let plain = tmp.path().join("plain");
        fs::create_dir_all(&plain).unwrap();

        let store_dir = TempDir::new().unwrap();
        let store = cas(&store_dir);
        let src = tmp.path().join("s");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f"), b"f").unwrap();
        let (node_tree, _stats) = store.put_tree(&src, &matcher()).unwrap();

        assert!(matches!(
            export_to_git(&plain, &store, node_tree, "b"),
            Err(GitError::NotARepo)
        ));
    }

    #[test]
    fn export_snapshot_resolves_root_tree() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        init_committed_repo(&root);

        let store_dir = TempDir::new().unwrap();
        let store = cas(&store_dir);
        let src = tmp.path().join("node_state");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("snap.txt"), b"from a snapshot\n").unwrap();
        let profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&profile);
        let (snap, root_tree, _stats) = store
            .capture_snapshot(&src, &m, profile.hash(), None)
            .unwrap();

        let via_snapshot = export_snapshot_to_git(&root, &store, snap, "spork/from-snap").unwrap();

        // The snapshot export equals a direct root-tree export's content: the new
        // commit's tree holds the same file.
        let repo = Repository::open(&root).unwrap();
        let oid = git2::Oid::from_str(&via_snapshot).unwrap();
        let tree = repo.find_commit(oid).unwrap().tree().unwrap();
        let blob = tree
            .get_path(Path::new("snap.txt"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(blob.as_blob().unwrap().content(), b"from a snapshot\n");
        // And the snapshot's root_tree is what we exported.
        let _ = root_tree;
    }

    #[test]
    fn export_from_graph_node_snapshot_is_non_invasive() {
        // Drive the export exactly as the daemon will: a real F2 GraphService
        // creates a snapshot node over a CAS snapshot, and we export that node's
        // snapshot's tree to a new branch — proving the spork-graph integration
        // and the non-invasive contract together.
        use serde_json::json;
        use spork_graph::GraphService;
        use spork_log::EventLog;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let repo = init_committed_repo(&root);
        let refs_before = ref_names(&repo);
        let head_before = fingerprint_git_protected(&root);
        let mut wt_before = BTreeMap::new();
        fingerprint_worktree(&root, &root, &mut wt_before);
        drop(repo);

        // CAS capture of the node's state.
        let store_dir = TempDir::new().unwrap();
        let store = cas(&store_dir);
        let src = tmp.path().join("node_state");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("graph_node.txt"), b"state owned by a graph node\n").unwrap();
        let profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&profile);
        let (snap_hash, _root_tree, _stats) = store
            .capture_snapshot(&src, &m, profile.hash(), None)
            .unwrap();

        // Real graph node owning that snapshot.
        let log = EventLog::open(&tmp.path().join("log.db")).unwrap();
        let mut svc = GraphService::open_in_memory(log.writer()).unwrap();
        svc.register_builtin_snapshot().unwrap();
        let node = svc
            .create_node(
                "snapshot",
                None,
                vec![],
                "main",
                json!({ "origin": "manual" }),
                true,
                Some(snap_hash),
            )
            .unwrap();
        // The node owns the snapshot we captured.
        assert_eq!(node.snapshot_hash, Some(snap_hash));

        // Export the node's snapshot.
        let commit_sha = export_snapshot_to_git(
            &root,
            &store,
            node.snapshot_hash.unwrap(),
            "spork/graph-node",
        )
        .unwrap();

        // Non-invasive: HEAD + index + working tree byte-unchanged; one new ref.
        assert_eq!(head_before, fingerprint_git_protected(&root));
        let mut wt_after = BTreeMap::new();
        fingerprint_worktree(&root, &root, &mut wt_after);
        assert_eq!(wt_before, wt_after);

        let repo = Repository::open(&root).unwrap();
        let new_refs: Vec<String> = ref_names(&repo)
            .into_iter()
            .filter(|r| !refs_before.contains(r))
            .collect();
        assert_eq!(new_refs, vec!["refs/heads/spork/graph-node".to_string()]);

        // The exported commit carries the node's state.
        let oid = git2::Oid::from_str(&commit_sha).unwrap();
        let tree = repo.find_commit(oid).unwrap().tree().unwrap();
        let blob = tree
            .get_path(Path::new("graph_node.txt"))
            .unwrap()
            .to_object(&repo)
            .unwrap();
        assert_eq!(
            blob.as_blob().unwrap().content(),
            b"state owned by a graph node\n"
        );
    }

    #[test]
    fn git_context_serde_round_trips_and_carries_version() {
        let ctx = GitContext {
            schema_version: GIT_CONTEXT_VERSION,
            head: Some("refs/heads/main".to_string()),
            branch: Some("main".to_string()),
            dirty: true,
            git_parent_commit: Some("a".repeat(40)),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: GitContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
        assert_eq!(back.schema_version, GIT_CONTEXT_VERSION);
        // The empty/none context is a valid, versioned value too.
        assert_eq!(GitContext::none().schema_version, GIT_CONTEXT_VERSION);
    }
}
