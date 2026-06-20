//! Contract-level integration tests for the daemon: the IPC load-bearing rule,
//! undo/redo, gc, the vault round-trip, reads, and the rebuild-after-restart
//! property — complementing the end-to-end DoD suite in `headless_dod.rs`.

use spork_broker::{Capability, Grant, Scope};
use spork_daemon::{Command, CommandHandler, CommandResult, Daemon, IpcError, OpLogEvent};
use spork_vault::{Secret, VaultRef};
use ulid::Ulid;

/// Build a daemon with the default grants plus a `secrets.get` grant, so the
/// vault round-trip is authorized.
fn daemon_with_secrets(root: &std::path::Path) -> Daemon {
    let worktree = Scope::new().with_path_globs(["**"]);
    let grants = vec![
        Grant::new(Capability::SnapshotRead, worktree.clone()),
        Grant::new(Capability::SnapshotWrite, worktree),
        Grant::new(Capability::SecretsGet, Scope::new()),
    ];
    Daemon::builder(root).with_grants(grants).build().unwrap()
}

fn create_node(daemon: &Daemon) -> Ulid {
    let (snapshot_hash, _) = daemon.capture_working_tree().unwrap();
    let result = daemon
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

#[test]
fn every_dispatch_result_honors_the_load_bearing_rule() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    std::fs::write(daemon.workdir().join("a.txt"), b"a").unwrap();
    let node = create_node(&daemon);
    let (snapshot_hash, root_tree) = daemon.capture_working_tree().unwrap();
    let _ = snapshot_hash;

    let cmds = vec![
        Command::NodeDiff {
            node_id: node,
            against: None,
        },
        Command::BlobRead {
            tree_hash: root_tree,
            path: "a.txt".into(),
        },
        Command::GcRun { dry_run: true },
        Command::RefCreate {
            name: "tag-1".into(),
            kind: spork_daemon::RefKind::Tag,
            to: node,
        },
    ];
    for cmd in cmds {
        let result = daemon.dispatch(cmd.clone()).unwrap();
        // The daemon's own debug_assert mirrors this; assert it in release too.
        assert!(
            result.matches_command(&cmd),
            "result shape violates the rule for {cmd:?}: {result:?}"
        );
        // Reads carry no op_id; mutations do.
        assert_eq!(
            result.op_id().is_some(),
            cmd.is_mutation() && !matches!(cmd, Command::GcRun { .. })
        );
    }
}

#[test]
fn reads_return_inline_data_and_emit_no_events() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    std::fs::write(daemon.workdir().join("hello.txt"), b"world").unwrap();
    let node = create_node(&daemon);
    let (_snap, root_tree) = daemon.capture_working_tree().unwrap();

    // Subscribe, then issue reads: no event should arrive.
    let events = daemon.subscribe_events();
    let diff = daemon
        .dispatch(Command::NodeDiff {
            node_id: node,
            against: None,
        })
        .unwrap();
    assert!(matches!(diff, CommandResult::Diff { .. }));

    let blob = daemon
        .dispatch(Command::BlobRead {
            tree_hash: root_tree,
            path: "hello.txt".into(),
        })
        .unwrap();
    match blob {
        CommandResult::Blob { bytes } => assert_eq!(bytes, b"world"),
        other => panic!("expected Blob, got {other:?}"),
    }

    assert!(events.try_recv().is_err(), "reads emit no ordered events");
}

#[test]
fn blob_read_of_a_missing_path_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    std::fs::write(daemon.workdir().join("present.txt"), b"x").unwrap();
    let _ = create_node(&daemon);
    let (_snap, root_tree) = daemon.capture_working_tree().unwrap();

    let err = daemon
        .dispatch(Command::BlobRead {
            tree_hash: root_tree,
            path: "absent.txt".into(),
        })
        .unwrap_err();
    assert!(matches!(err, IpcError::NotFound(_)), "got {err:?}");
}

#[test]
fn undo_then_redo_records_ordered_events() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    std::fs::write(daemon.workdir().join("a.txt"), b"a").unwrap();

    let events = daemon.subscribe_events();
    // One mutation puts an op on the cursor.
    let _ = create_node(&daemon);
    // Drain the NodeCreated event.
    let _ = events
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();

    // Undo the latest op → OP_UNDONE; the result echoes the undone op_id.
    let undo = daemon.dispatch(Command::OpUndo { op_id: None }).unwrap();
    assert!(undo.op_id().is_some());
    assert!(matches!(
        events
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        OpLogEvent::OpUndone { .. }
    ));

    // Redo it → OP_REDONE.
    let redo = daemon.dispatch(Command::OpRedo { op_id: None }).unwrap();
    assert_eq!(redo.op_id(), undo.op_id(), "redo restores the same op");
    assert!(matches!(
        events
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        OpLogEvent::OpRedone { .. }
    ));
}

#[test]
fn undo_with_nothing_to_undo_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    let err = daemon
        .dispatch(Command::OpUndo { op_id: None })
        .unwrap_err();
    assert!(matches!(err, IpcError::NotFound(_)), "got {err:?}");
}

#[test]
fn gc_dry_run_reports_nothing_and_emits_no_event() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    let events = daemon.subscribe_events();

    let result = daemon.dispatch(Command::GcRun { dry_run: true }).unwrap();
    match result {
        CommandResult::Gc { reclaimable, bytes } => {
            assert!(reclaimable.is_empty(), "conservative GC reclaims nothing");
            assert_eq!(bytes, 0);
        }
        other => panic!("expected Gc, got {other:?}"),
    }
    assert!(events.try_recv().is_err(), "a dry run emits no GC event");
}

#[test]
fn gc_real_run_emits_gc_performed() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Daemon::open(dir.path()).unwrap();
    let events = daemon.subscribe_events();
    let _ = daemon.dispatch(Command::GcRun { dry_run: false }).unwrap();
    assert!(matches!(
        events
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        OpLogEvent::GcPerformed { .. }
    ));
}

#[test]
fn vault_round_trips_and_never_persists_the_secret_in_plaintext_config() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = daemon_with_secrets(dir.path());

    let secret_bytes = b"super-secret-api-key-ABC123".to_vec();
    let vault_ref = daemon
        .vault_put("openai", Secret::new(secret_bytes.clone()))
        .unwrap();

    // The opaque ref is NOT the secret.
    assert_ne!(vault_ref.as_str().as_bytes(), secret_bytes.as_slice());

    // Resolving the ref inside the daemon returns the secret.
    let resolved = daemon.vault_get(&vault_ref).unwrap();
    assert_eq!(resolved.expose(), secret_bytes.as_slice());

    // The secret never lands in the CAS (the renderer-exportable store).
    let objects = daemon.cas_dir().join("objects");
    assert_no_file_contains(&objects, &secret_bytes);

    // Delete works.
    daemon.vault_delete(&vault_ref).unwrap();
    assert!(daemon.vault_get(&vault_ref).is_err());
}

#[test]
fn vault_without_grant_is_denied_and_audited() {
    let dir = tempfile::tempdir().unwrap();
    // Default grants do NOT include secrets.get.
    let daemon = Daemon::open(dir.path()).unwrap();
    let err = daemon
        .vault_get(&VaultRef::from_string("x".into()))
        .unwrap_err();
    assert!(
        matches!(err, spork_daemon::DaemonError::Capability(_)),
        "got {err:?}"
    );
    let audit = daemon.audit_log();
    assert!(audit
        .iter()
        .any(|e| e.capability == Capability::SecretsGet && !e.allowed));
}

#[test]
fn graph_survives_a_daemon_restart_as_a_pure_projection() {
    let dir = tempfile::tempdir().unwrap();
    let node_id;
    {
        let daemon = Daemon::open(dir.path()).unwrap();
        std::fs::write(daemon.workdir().join("a.txt"), b"a").unwrap();
        node_id = create_node(&daemon);
        assert!(daemon.graph_view().node(node_id).is_some());
    }
    // Reopen the same root: the graph is rebuilt from the durable log.
    let reopened = Daemon::open(dir.path()).unwrap();
    assert!(
        reopened.graph_view().node(node_id).is_some(),
        "the node survives a restart (graph is a pure projection of the log)"
    );
}

/// Assert no file under `dir` contains `needle`.
fn assert_no_file_contains(dir: &std::path::Path, needle: &[u8]) {
    if !dir.exists() {
        return;
    }
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            assert_no_file_contains(&path, needle);
        } else if meta.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            let leaked = bytes.windows(needle.len().max(1)).any(|w| w == needle);
            assert!(!leaked, "secret leaked into {path:?}");
        }
    }
}
