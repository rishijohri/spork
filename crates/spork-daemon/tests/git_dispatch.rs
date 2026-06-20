//! Integration tests for the F3-UI git IPC actions (DESIGN.md §10.4).
//!
//! These drive `git.export` / `git.push` end-to-end through the daemon's frozen
//! [`CommandHandler`](spork_ipc::CommandHandler) seam, proving:
//!
//! - `GitExport` projects a snapshot-owning node's tree into a real Git commit on
//!   a new `spork/<nodeId>` branch and replies inline with
//!   [`CommandResult::Git`] (no [`OpLogEvent`] — it is action-shaped, not a graph
//!   mutation), leaving the user's `.git` HEAD/index/working tree untouched;
//! - `GitPush` exports-then-pushes the branch to a LOCAL bare remote (no network)
//!   so the ref lands on the remote and the reply carries `pushed: true`;
//! - `GitExport` is denied without `snapshot.read`, and `GitPush` is denied
//!   without `net.connect` (deny-by-default — DESIGN.md §15.1).

use spork_broker::{Capability, Grant, Scope};
use spork_daemon::{Command, CommandHandler, CommandResult, Daemon};
use ulid::Ulid;

/// A daemon whose working tree IS a git repo (where the IPC git actions operate),
/// with a grant set we choose.
struct Harness {
    daemon: Daemon,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// Open a daemon rooted at a temp dir with `grants`, then `git init` its
    /// working directory and make a baseline commit so HEAD/index/a branch exist.
    fn new(grants: Vec<Grant>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::builder(dir.path())
            .with_grants(grants)
            .build()
            .unwrap();
        init_committed_repo(&daemon.workdir());
        Harness { daemon, _dir: dir }
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.daemon.workdir().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    /// Capture the working tree and create a snapshot-owning node over it.
    fn create_snapshot_node(&self) -> Ulid {
        let (snapshot_hash, _root) = self.daemon.capture_working_tree().unwrap();
        let result = self
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
        match result {
            CommandResult::Mutation { ids, .. } => {
                Ulid::from_string(ids["nodeId"].as_str().unwrap()).unwrap()
            }
            other => panic!("expected Mutation, got {other:?}"),
        }
    }
}

/// The default grants (snapshot read/write over the worktree). No `net.connect`,
/// so a push is denied unless the test grants it explicitly.
fn snapshot_grants() -> Vec<Grant> {
    let worktree = Scope::new().with_path_globs(["**"]);
    vec![
        Grant::new(Capability::SnapshotRead, worktree.clone()),
        Grant::new(Capability::SnapshotWrite, worktree),
    ]
}

/// `git init` at `root`, make one commit so HEAD/index/a branch exist, then drop
/// the repo handle.
fn init_committed_repo(root: &std::path::Path) {
    let repo = git2::Repository::init(root).unwrap();
    std::fs::write(root.join("existing.txt"), b"on the user's branch\n").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial\n", &tree, &[])
        .unwrap();
    drop(tree);
    drop(index);
}

#[test]
fn git_export_projects_a_branch_inline_without_emitting_an_event() {
    let h = Harness::new(snapshot_grants());
    h.write("src/lib.rs", b"pub fn answer() -> u32 { 42 }\n");
    let node = h.create_snapshot_node();

    // Subscribe AFTER node creation so any event the git action wrongly emits is
    // observed. (Action-shaped commands must emit nothing.)
    let events = h.daemon.subscribe_events();

    let result = h
        .daemon
        .dispatch(Command::GitExport {
            node_id: node,
            branch: None,
        })
        .unwrap();

    let (branch, commit_sha, pushed) = match result {
        CommandResult::Git {
            branch,
            commit_sha,
            pushed,
        } => (branch, commit_sha, pushed),
        other => panic!("expected Git result, got {other:?}"),
    };
    assert_eq!(branch, format!("spork/{node}"));
    assert_eq!(commit_sha.len(), 40, "a 40-char SHA-1 commit id");
    assert!(!pushed, "a plain export is not pushed");

    // No op-log event was emitted (the git action is not a graph mutation).
    assert!(
        events.try_recv().is_err(),
        "git.export must not emit an OpLogEvent"
    );

    // The branch exists in the daemon's working-tree repo and carries the node's
    // state; the user's HEAD is untouched.
    let repo = git2::Repository::open(h.daemon.workdir()).unwrap();
    let exported = repo
        .find_branch(&branch, git2::BranchType::Local)
        .unwrap()
        .get()
        .peel_to_commit()
        .unwrap();
    assert_eq!(exported.id().to_string(), commit_sha);
    assert!(exported
        .tree()
        .unwrap()
        .get_path(std::path::Path::new("src/lib.rs"))
        .is_ok());
    // HEAD still points at the original branch, not the export.
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_ne!(head.id(), exported.id());
}

#[test]
fn git_export_uses_the_requested_branch_name() {
    let h = Harness::new(snapshot_grants());
    h.write("a.txt", b"x");
    let node = h.create_snapshot_node();

    let result = h
        .daemon
        .dispatch(Command::GitExport {
            node_id: node,
            branch: Some("feature/custom".into()),
        })
        .unwrap();
    match result {
        CommandResult::Git { branch, .. } => assert_eq!(branch, "feature/custom"),
        other => panic!("expected Git result, got {other:?}"),
    }
    let repo = git2::Repository::open(h.daemon.workdir()).unwrap();
    assert!(repo
        .find_branch("feature/custom", git2::BranchType::Local)
        .is_ok());
}

#[test]
fn git_export_is_denied_without_snapshot_read() {
    // Grant only snapshot.write (so node create works) but NOT snapshot.read.
    let worktree = Scope::new().with_path_globs(["**"]);
    let grants = vec![Grant::new(Capability::SnapshotWrite, worktree)];
    let h = Harness::new(grants);
    h.write("a.txt", b"x");
    let node = h.create_snapshot_node();

    let err = h
        .daemon
        .dispatch(Command::GitExport {
            node_id: node,
            branch: None,
        })
        .unwrap_err();
    assert!(
        matches!(err, spork_ipc::IpcError::Capability(_)),
        "got {err:?}"
    );
}

#[test]
fn git_push_lands_the_branch_on_a_local_bare_remote() {
    // Grant snapshot read/write (export reads + node create writes) AND
    // net.connect (push reaches the network).
    let worktree = Scope::new().with_path_globs(["**"]);
    let grants = vec![
        Grant::new(Capability::SnapshotRead, worktree.clone()),
        Grant::new(Capability::SnapshotWrite, worktree.clone()),
        // The bare remote is a local file path → host resolves to "localhost".
        // Allow any host so the push authorization passes.
        Grant::new(Capability::NetConnect, Scope::new().with_hosts(["*"])),
    ];
    let h = Harness::new(grants);
    h.write("pushed.txt", b"pushed by spork\n");
    let node = h.create_snapshot_node();

    // A local bare repo on disk is the remote (no network).
    let bare = h._dir.path().join("remote.git");
    git2::Repository::init_bare(&bare).unwrap();
    {
        let repo = git2::Repository::open(h.daemon.workdir()).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
    }

    let result = h
        .daemon
        .dispatch(Command::GitPush {
            node_id: node,
            remote: None,
        })
        .unwrap();
    let (branch, commit_sha, pushed) = match result {
        CommandResult::Git {
            branch,
            commit_sha,
            pushed,
        } => (branch, commit_sha, pushed),
        other => panic!("expected Git result, got {other:?}"),
    };
    assert_eq!(branch, format!("spork/{node}"));
    assert!(pushed, "a push reports pushed = true");

    // The ref landed on the bare remote at the exported commit.
    let remote = git2::Repository::open_bare(&bare).unwrap();
    let landed = remote
        .find_reference(&format!("refs/heads/{branch}"))
        .expect("the pushed branch should exist on the remote")
        .target()
        .unwrap();
    assert_eq!(landed, git2::Oid::from_str(&commit_sha).unwrap());
}

#[test]
fn git_push_is_denied_without_net_connect() {
    // Snapshot read/write but no net.connect → the push authorization fails
    // before any network attempt.
    let h = Harness::new(snapshot_grants());
    h.write("a.txt", b"x");
    let node = h.create_snapshot_node();

    // Configure a remote so host resolution succeeds and the denial happens at
    // the net.connect authorization, not at remote lookup.
    let bare = h._dir.path().join("remote.git");
    git2::Repository::init_bare(&bare).unwrap();
    {
        let repo = git2::Repository::open(h.daemon.workdir()).unwrap();
        repo.remote("origin", bare.to_str().unwrap()).unwrap();
    }

    let err = h
        .daemon
        .dispatch(Command::GitPush {
            node_id: node,
            remote: None,
        })
        .unwrap_err();
    assert!(
        matches!(err, spork_ipc::IpcError::Capability(_)),
        "got {err:?}"
    );
}
