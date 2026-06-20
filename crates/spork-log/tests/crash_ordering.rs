//! Crash-ordering fault injection across the two stores (DESIGN.md §5.2, §10.4,
//! A.6 crash-recovery matrix).
//!
//! Spork keeps code bytes in a content-addressed object store (`spork-cas`) and
//! references them from events in the log (`spork-log`). The frozen invariant is
//! **write-objects-then-log**: an object is fsynced into the CAS *before* the
//! event that references it commits. This test injects the worst crash window —
//! crash *after* the object fsync but *before* the event commit — and proves the
//! two recovery properties:
//!
//! 1. **A reclaimable orphan object** remains in the CAS (it is content-addressed
//!    and immutable; a later GC mark-sweep reclaims it because no live event
//!    references it).
//! 2. **No dangling event reference**: the log contains no event pointing at the
//!    orphan, and the chain still verifies.

use serde_json::json;
use spork_cas::{LooseStore, ObjectStore, StorageBackend};
use spork_log::{EventLog, NewEvent};
use tempfile::TempDir;

/// Simulate: write object (fsynced) -> CRASH before appending the referencing
/// event. Recovery must find a reclaimable orphan and no dangling reference.
#[test]
fn crash_after_object_fsync_before_log_commit_leaves_orphan_not_dangling_ref() {
    let dir = TempDir::new().unwrap();
    let cas = ObjectStore::new(LooseStore::open(dir.path().join("objects")).unwrap());
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();

    // Phase 1: a couple of clean object+event pairs land normally.
    let w = log.writer();
    let mut live_object_ids = Vec::new();
    for i in 0..2 {
        let (id, _stats) = cas
            .put_blob_bytes(format!("clean blob {i}").as_bytes())
            .unwrap();
        live_object_ids.push(id);
        w.append(NewEvent::new(
            "snapshot.attached",
            1,
            json!({ "blob": id.to_hex(), "i": i }),
            "tester",
        ))
        .unwrap();
    }

    // Phase 2: the crash window. Put the object (this fsyncs it durably), then
    // "crash" before appending its event — modeled by simply NOT appending.
    let (orphan_id, stats) = cas.put_blob_bytes(b"orphaned-by-crash").unwrap();
    assert_eq!(stats.new_objects, 1, "the orphan was genuinely written");
    // <-- crash here: no w.append(...) for orphan_id.

    let len_before_recovery = w.len().unwrap();
    drop(w);
    drop(log);

    // Recovery: reopen both stores (as a restarted daemon would).
    let cas = ObjectStore::new(LooseStore::open(dir.path().join("objects")).unwrap());
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let r = log.reader().unwrap();

    // (1) The orphan object survived the crash and is present (durable, fsynced).
    assert!(
        cas.backend().has(&orphan_id).unwrap(),
        "object fsynced before the crash must survive"
    );

    // (2) No event references the orphan: the log only has the clean events.
    assert_eq!(
        r.len().unwrap(),
        len_before_recovery,
        "the uncommitted event must not have landed"
    );
    let referenced: std::collections::HashSet<String> = r
        .iter_from(1)
        .unwrap()
        .map(|e| e.unwrap())
        .filter_map(|e| {
            e.payload
                .get("blob")
                .and_then(|b| b.as_str().map(str::to_string))
        })
        .collect();
    assert!(
        !referenced.contains(&orphan_id.to_hex()),
        "no event may reference the orphaned object (no dangling reference)"
    );
    // The live objects are all still referenced.
    for id in &live_object_ids {
        assert!(referenced.contains(&id.to_hex()));
    }

    // (3) The chain is intact despite the crash.
    r.verify_chain().unwrap();

    // (4) Reclaimability: a reachability mark over the (only) root — the events —
    // identifies the orphan as collectable. We model the GC mark step the design
    // mandates (A.2: mark from log+projection, then sweep objects).
    let reclaimable = !referenced.contains(&orphan_id.to_hex());
    assert!(
        reclaimable,
        "the orphan is unreferenced, hence reclaimable by a mark-sweep GC"
    );
}

/// The mirror property: when the object *and* its event both land (no crash),
/// the object is reachable and therefore NOT reclaimable — confirming the orphan
/// test above is detecting absence-of-reference, not just always-true.
#[test]
fn committed_object_event_pair_is_reachable_and_not_orphan() {
    let dir = TempDir::new().unwrap();
    let cas = ObjectStore::new(LooseStore::open(dir.path().join("objects")).unwrap());
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let w = log.writer();

    let (id, _) = cas.put_blob_bytes(b"committed").unwrap();
    w.append(NewEvent::new(
        "snapshot.attached",
        1,
        json!({ "blob": id.to_hex() }),
        "tester",
    ))
    .unwrap();

    let r = log.reader().unwrap();
    let referenced: std::collections::HashSet<String> = r
        .iter_from(1)
        .unwrap()
        .map(|e| e.unwrap())
        .filter_map(|e| {
            e.payload
                .get("blob")
                .and_then(|b| b.as_str().map(str::to_string))
        })
        .collect();

    assert!(cas.backend().has(&id).unwrap());
    assert!(
        referenced.contains(&id.to_hex()),
        "a committed object is reachable from the log and not an orphan"
    );
}
