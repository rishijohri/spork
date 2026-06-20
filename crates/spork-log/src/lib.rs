//! Spork append-only, hash-chained event log — the system's single source of
//! truth.
//!
//! Every change to a Spork project is recorded as an immutable [`Event`] appended
//! through one serializing **writer actor** (the only write path), persisted in
//! SQLite running in WAL mode (a single writer with many concurrent readers).
//! Each event links to its predecessor through a BLAKE3 hash chain
//! (`prev_event_hash` → `this_event_hash`) computed over the canonical bytes of
//! the payload (reusing `spork-canon`) plus the event's framing fields, so any
//! tamper with stored history is detectable by recomputing the chain
//! ([`Reader::verify_chain`]). The node/edge graph and all other state are pure
//! **projections** of this log: droppable and rebuildable bit-for-bit (see the
//! `spork-projection` crate). Older payloads are upgraded on read through the
//! `spork-migrate` registry without rewriting storage
//! ([`EventLog::reader_with_migrations`]).
//!
//! This realizes the engine's event-sourcing core described in DESIGN.md §5.2
//! ("Engine internals") and §6.1 ("The two layers"), with the core schemas and
//! durability/crash-recovery guarantees in DESIGN.md A.1 ("IPC Contract & Core
//! Schemas") and A.6 ("Self-Testing, Crash Recovery & Observability").
//!
//! # Frozen contract
//!
//! The hash-chain preimage is **frozen** (CLAUDE.md C2): a change to how
//! `this_event_hash` is computed is a new generation, never an in-place rehash
//! of stored events. The exact preimage is documented on [`compute_event_hash`]
//! and pinned by a golden-vector test. The STRICT SQLite-WAL schema lives in the
//! `schema` module.
//!
//! # The pieces
//!
//! - [`Event`] / [`NewEvent`] — the persisted envelope and the caller-supplied
//!   input (the log assigns `seq`, `event_id`, and the chain hashes).
//! - [`EventLog`] — owns the WAL database path and the single writer actor.
//! - [`WriterHandle`] — the cloneable handle to that actor; [`WriterHandle::append`]
//!   is the only write path. Appends are linearized and durable (committed and
//!   fsynced under `synchronous=FULL`) before they return.
//! - [`Reader`] — read-only access on its own connection: [`Reader::len`],
//!   [`Reader::get`], [`Reader::iter_from`], [`Reader::verify_chain`], plus
//!   lazy upgrade-on-read.
//! - [`LogError`] — the crate error type.
//!
//! # Example
//!
//! ```
//! use spork_log::{EventLog, NewEvent};
//! use serde_json::json;
//! use tempfile::TempDir;
//!
//! let dir = TempDir::new().unwrap();
//! let log = EventLog::open(&dir.path().join("log.db")).unwrap();
//!
//! // Append through the single writer actor.
//! let w = log.writer();
//! let e1 = w.append(NewEvent::new("node.created", 1, json!({"id": "a"}), "tester")).unwrap();
//! let e2 = w.append(NewEvent::new("edge.added", 1, json!({"from": "a"}), "tester")).unwrap();
//! assert_eq!(e1.seq, 1);
//! assert_eq!(e2.seq, 2);
//! assert_eq!(e2.prev_event_hash, e1.this_event_hash); // the chain links
//!
//! // Read it back and verify the whole chain.
//! let r = log.reader().unwrap();
//! assert_eq!(r.len().unwrap(), 2);
//! r.verify_chain().unwrap();
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod event;
mod reader;
mod schema;
mod store;
mod writer;

pub use error::{LogError, Result};
pub use event::{compute_event_hash, Event, NewEvent, GENESIS_PREV_HASH};
pub use reader::{CurrentVersions, Reader};
pub use store::EventLog;
pub use writer::WriterHandle;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Open a fresh log under a temp dir, returning the dir (kept alive) and log.
    fn fresh() -> (TempDir, EventLog) {
        let dir = TempDir::new().unwrap();
        let log = EventLog::open(&dir.path().join("log.db")).unwrap();
        (dir, log)
    }

    /// Wrap a registry in the `Arc` the public migration API takes.
    ///
    /// The frozen API ([`EventLog::reader_with_migrations`]) accepts
    /// `Arc<MigrationRegistry>`. A `MigrationRegistry` holds boxed trait objects
    /// that are not `Send + Sync`, so Clippy's `arc_with_non_send_sync` would
    /// otherwise fire; the allow is correct because a `Reader` is single-threaded
    /// (it owns a non-`Sync` SQLite connection) and never shares the registry
    /// across threads — the `Arc` is purely shared-by-reference ownership, not a
    /// cross-thread handoff.
    #[allow(clippy::arc_with_non_send_sync)]
    fn arc_registry(reg: MigrationRegistry) -> Arc<MigrationRegistry> {
        Arc::new(reg)
    }

    #[test]
    fn append_assigns_contiguous_seq_and_chains() {
        let (_dir, log) = fresh();
        let w = log.writer();

        let mut prev = GENESIS_PREV_HASH;
        for i in 1..=10u64 {
            let e = w
                .append(NewEvent::new("t", 1, json!({ "i": i }), "actor"))
                .unwrap();
            assert_eq!(e.seq, i, "seq must be contiguous from 1");
            assert_eq!(e.prev_event_hash, prev, "prev must link to last tip");
            // The recomputed hash matches what was stored.
            e.verify_self().unwrap();
            prev = e.this_event_hash;
        }

        let r = log.reader().unwrap();
        assert_eq!(r.len().unwrap(), 10);
        r.verify_chain().unwrap();
    }

    #[test]
    fn genesis_event_chains_from_zero_hash() {
        let (_dir, log) = fresh();
        let e = log
            .writer()
            .append(NewEvent::new("t", 1, json!({}), "a"))
            .unwrap();
        assert_eq!(e.seq, 1);
        assert_eq!(e.prev_event_hash, GENESIS_PREV_HASH);
    }

    #[test]
    fn get_returns_stored_event_or_none() {
        let (_dir, log) = fresh();
        let w = log.writer();
        let e1 = w
            .append(NewEvent::new("t", 1, json!({"k": 1}), "a"))
            .unwrap();
        let r = log.reader().unwrap();

        let got = r.get(1).unwrap().unwrap();
        assert_eq!(got, e1);
        assert!(r.get(2).unwrap().is_none());
        assert!(r.get(0).unwrap().is_none());
    }

    #[test]
    fn iter_from_yields_ordered_suffix() {
        let (_dir, log) = fresh();
        let w = log.writer();
        for i in 1..=5u64 {
            w.append(NewEvent::new("t", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        let r = log.reader().unwrap();
        let seqs: Vec<u64> = r.iter_from(3).unwrap().map(|e| e.unwrap().seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);

        // From 1 yields the whole log; from past-the-end yields nothing.
        assert_eq!(r.iter_from(1).unwrap().count(), 5);
        assert_eq!(r.iter_from(99).unwrap().count(), 0);
    }

    #[test]
    fn empty_log_is_empty() {
        let (_dir, log) = fresh();
        let r = log.reader().unwrap();
        assert!(r.is_empty().unwrap());
        assert_eq!(r.len().unwrap(), 0);
        r.verify_chain().unwrap(); // vacuously valid
        assert!(log.writer().is_empty().unwrap());
    }

    #[test]
    fn reopen_continues_chain() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");

        let last_hash;
        {
            let log = EventLog::open(&path).unwrap();
            let w = log.writer();
            w.append(NewEvent::new("t", 1, json!({"i": 1}), "a"))
                .unwrap();
            let e2 = w
                .append(NewEvent::new("t", 1, json!({"i": 2}), "a"))
                .unwrap();
            last_hash = e2.this_event_hash;
            // log (and its writer actor) drops here, closing the db.
        }

        // Reopen: the next append must continue from seq 3 chaining off seq 2.
        let log = EventLog::open(&path).unwrap();
        let e3 = log
            .writer()
            .append(NewEvent::new("t", 1, json!({"i": 3}), "a"))
            .unwrap();
        assert_eq!(e3.seq, 3);
        assert_eq!(e3.prev_event_hash, last_hash);

        let r = log.reader().unwrap();
        assert_eq!(r.len().unwrap(), 3);
        r.verify_chain().unwrap();
    }

    #[test]
    fn writer_len_tracks_appends() {
        let (_dir, log) = fresh();
        let w = log.writer();
        assert_eq!(w.len().unwrap(), 0);
        w.append(NewEvent::new("t", 1, json!({}), "a")).unwrap();
        assert_eq!(w.len().unwrap(), 1);
        w.append(NewEvent::new("t", 1, json!({}), "a")).unwrap();
        assert_eq!(w.len().unwrap(), 2);
    }

    #[test]
    fn tamper_with_payload_is_detected_at_right_seq() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        let w = log.writer();
        for i in 1..=5u64 {
            w.append(NewEvent::new("t", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        log.reader().unwrap().verify_chain().unwrap();
        drop(log); // release the writer's connection before tampering directly.

        // Directly mutate the stored payload of seq 3 — simulating tamper.
        let conn = rusqlite::Connection::open(&path).unwrap();
        let tampered = spork_canon::canonicalize_value(&json!({ "i": 999 })).unwrap();
        conn.execute("UPDATE event SET payload = ?1 WHERE seq = 3", [tampered])
            .unwrap();
        drop(conn);

        let log = EventLog::open(&path).unwrap();
        let r = log.reader().unwrap();
        let err = r.verify_chain().unwrap_err();
        assert_eq!(err, LogError::HashChainBroken { seq: 3 });
    }

    #[test]
    fn tamper_with_stored_hash_is_detected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        let w = log.writer();
        for i in 1..=4u64 {
            w.append(NewEvent::new("t", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        drop(log);

        // Flip the stored this_event_hash of seq 2. This breaks both seq 2's
        // self-check and seq 3's prev-link; verify_chain reports the *first*
        // detected break, which is seq 2 (its self-hash no longer matches).
        let conn = rusqlite::Connection::open(&path).unwrap();
        let bad = [0xABu8; spork_hash::HASH_LEN];
        conn.execute(
            "UPDATE event SET this_event_hash = ?1 WHERE seq = 2",
            [bad.as_slice()],
        )
        .unwrap();
        drop(conn);

        let log = EventLog::open(&path).unwrap();
        let err = log.reader().unwrap().verify_chain().unwrap_err();
        assert_eq!(err, LogError::HashChainBroken { seq: 2 });
    }

    #[test]
    fn tamper_with_prev_link_is_detected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        let w = log.writer();
        for i in 1..=4u64 {
            w.append(NewEvent::new("t", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        drop(log);

        // Corrupt seq 3's prev_event_hash. Its self-hash is computed over its
        // own prev field, so this changes the recomputed this_event_hash; the
        // self-check at seq 3 fails. The break is reported at seq 3.
        let conn = rusqlite::Connection::open(&path).unwrap();
        let bad = [0x11u8; spork_hash::HASH_LEN];
        conn.execute(
            "UPDATE event SET prev_event_hash = ?1 WHERE seq = 3",
            [bad.as_slice()],
        )
        .unwrap();
        drop(conn);

        let log = EventLog::open(&path).unwrap();
        let err = log.reader().unwrap().verify_chain().unwrap_err();
        assert_eq!(err, LogError::HashChainBroken { seq: 3 });
    }

    #[test]
    fn concurrent_readers_see_committed_events() {
        let (_dir, log) = fresh();
        let w = log.writer();
        for i in 1..=20u64 {
            w.append(NewEvent::new("t", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        // Several independent readers, each its own connection, all see the same
        // committed state concurrently.
        let r1 = log.reader().unwrap();
        let r2 = log.reader().unwrap();
        let r3 = log.reader().unwrap();
        assert_eq!(r1.len().unwrap(), 20);
        assert_eq!(r2.len().unwrap(), 20);
        assert_eq!(r3.len().unwrap(), 20);
        r1.verify_chain().unwrap();
    }

    #[test]
    fn many_threads_appending_are_linearized() {
        use std::thread;
        let (_dir, log) = fresh();
        let w = log.writer();

        let mut handles = Vec::new();
        for t in 0..8 {
            let wc = w.clone();
            handles.push(thread::spawn(move || {
                for i in 0..50 {
                    wc.append(NewEvent::new("t", 1, json!({ "thread": t, "i": i }), "a"))
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let r = log.reader().unwrap();
        // All 8*50 appends landed with a contiguous, verifiable chain — proof of
        // serialization despite concurrent producers.
        assert_eq!(r.len().unwrap(), 400);
        r.verify_chain().unwrap();
    }

    #[test]
    fn float_payload_append_is_rejected() {
        let (_dir, log) = fresh();
        let err = log
            .writer()
            .append(NewEvent::new("t", 1, json!({ "x": 1.5 }), "a"))
            .unwrap_err();
        assert!(matches!(err, LogError::Canon(_)));
        // The rejected append did not advance the chain.
        assert_eq!(log.writer().len().unwrap(), 0);
    }

    #[test]
    fn append_throughput_meets_budget() {
        // A.5 budget: >= 2k events/s through the serializing writer actor. We
        // measure a modest batch and assert the floor with generous headroom so
        // the test is not flaky on slow CI; the real number is reported in the
        // crate summary.
        use std::time::Instant;
        let (_dir, log) = fresh();
        let w = log.writer();
        let n = 4_000u64;
        let start = Instant::now();
        for i in 0..n {
            w.append(NewEvent::new("t", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        let elapsed = start.elapsed();
        let per_sec = (n as f64) / elapsed.as_secs_f64();
        assert!(
            per_sec >= 2_000.0,
            "append throughput {per_sec:.0}/s is below the 2k/s budget",
        );
        log.reader().unwrap().verify_chain().unwrap();
    }

    // ---- migration-on-read ----

    use spork_migrate::{EventMigration, MigrationError, MigrationRegistry};

    /// v1 -> v2: add an `archived: false` field.
    struct AddArchived;
    impl EventMigration for AddArchived {
        fn event_type(&self) -> &str {
            "doc"
        }
        fn from_version(&self) -> u16 {
            1
        }
        fn to_version(&self) -> u16 {
            2
        }
        fn upgrade(
            &self,
            mut p: serde_json::Value,
        ) -> std::result::Result<serde_json::Value, MigrationError> {
            p.as_object_mut()
                .ok_or_else(|| MigrationError::Upgrade("expected object".into()))?
                .insert("archived".into(), json!(false));
            Ok(p)
        }
    }

    #[test]
    fn migration_on_read_upgrades_old_event_without_rewriting_storage() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        // Write a "doc" at the OLD schema version 1.
        log.writer()
            .append(NewEvent::new("doc", 1, json!({ "title": "x" }), "a"))
            .unwrap();
        // Also a different type that has no migration registered.
        log.writer()
            .append(NewEvent::new("other", 1, json!({ "k": 1 }), "a"))
            .unwrap();

        let mut reg = MigrationRegistry::new();
        reg.register(Box::new(AddArchived)).unwrap();
        let mut current = CurrentVersions::new();
        current.insert("doc".into(), 2);

        let r = log
            .reader_with_migrations(arc_registry(reg), current)
            .unwrap();

        let doc = r.get(1).unwrap().unwrap();
        assert_eq!(doc.schema_version, 2, "version reflects upgrade");
        assert_eq!(doc.payload, json!({ "title": "x", "archived": false }));

        // Untouched type passes through unchanged.
        let other = r.get(2).unwrap().unwrap();
        assert_eq!(other.schema_version, 1);
        assert_eq!(other.payload, json!({ "k": 1 }));

        // Crucially, STORAGE was not rewritten: a plain reader still sees v1.
        let plain = log.reader().unwrap();
        let stored = plain.get(1).unwrap().unwrap();
        assert_eq!(stored.schema_version, 1);
        assert_eq!(stored.payload, json!({ "title": "x" }));

        // ...and the hash chain still verifies against the ORIGINAL bytes.
        plain.verify_chain().unwrap();
    }

    #[test]
    fn migration_on_read_via_iter_from() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        for i in 0..3 {
            log.writer()
                .append(NewEvent::new("doc", 1, json!({ "i": i }), "a"))
                .unwrap();
        }
        let mut reg = MigrationRegistry::new();
        reg.register(Box::new(AddArchived)).unwrap();
        let mut current = CurrentVersions::new();
        current.insert("doc".into(), 2);
        let r = log
            .reader_with_migrations(arc_registry(reg), current)
            .unwrap();

        for item in r.iter_from(1).unwrap() {
            let e = item.unwrap();
            assert_eq!(e.schema_version, 2);
            assert_eq!(e.payload["archived"], json!(false));
        }
    }

    #[test]
    fn already_current_event_not_touched_by_migration_reader() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        // Stored at v2 already.
        log.writer()
            .append(NewEvent::new(
                "doc",
                2,
                json!({ "title": "x", "archived": true }),
                "a",
            ))
            .unwrap();
        let mut reg = MigrationRegistry::new();
        reg.register(Box::new(AddArchived)).unwrap();
        let mut current = CurrentVersions::new();
        current.insert("doc".into(), 2);
        let r = log
            .reader_with_migrations(arc_registry(reg), current)
            .unwrap();
        let e = r.get(1).unwrap().unwrap();
        // No downgrade, no double-apply: archived stays true.
        assert_eq!(e.schema_version, 2);
        assert_eq!(e.payload, json!({ "title": "x", "archived": true }));
    }
}
