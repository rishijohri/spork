//! Spork projections — pure folds over the event log.
//!
//! A projection is a deterministic function of the `spork-log` event stream: it
//! owns no truth of its own and can always be dropped and rebuilt from the log
//! bit-for-bit. This crate provides the generic projection framework — a
//! [`Projection`] trait ([`apply`](Projection::apply) /
//! [`snapshot`](Projection::snapshot) / [`restore`](Projection::restore)),
//! canonical checkpoints ([`Checkpoint`]: a `spork-canon`-serialized snapshot
//! hashed with `spork-hash`), and a rebuild / load-or-rebuild driver
//! ([`ProjectionStore`]) — plus one concrete reference projection
//! ([`EventTypeCounts`]) that exercises replay, checkpointing, and rebuild end
//! to end. Because snapshots are canonically serialized, a full replay and a
//! checkpoint-plus-replay produce an identical snapshot hash, which is the
//! projection-soundness guarantee the design relies on.
//!
//! This realizes the projection layer described in DESIGN.md §6.1 ("The two
//! layers"), supports the reachability/retention model in DESIGN.md A.2
//! ("Garbage Collection & Retention"), and provides the rebuildability
//! guarantee in DESIGN.md A.6 ("Self-Testing, Crash Recovery & Observability").
//!
//! # The seam (CLAUDE.md C3)
//!
//! Everything is driven through the [`Projection`] trait. A new projection is a
//! new `impl Projection` and nothing else — the store's rebuild, checkpoint, and
//! load-or-rebuild paths are entirely generic over it. F1 ships exactly one
//! concrete projection ([`EventTypeCounts`]); F2+ adds the node/edge graph and
//! other projections behind the same seam, additively.
//!
//! # Evolution safety (CLAUDE.md C5)
//!
//! Both persisted shapes — the [`Checkpoint`] envelope and a projection's
//! `Snapshot` — carry an explicit `schema_version`. The reader path through
//! `spork-log` upgrades *old events* on read via the migration registry, so a
//! projection's [`apply`](Projection::apply) always sees current-version
//! payloads; checkpoints, being a pure function of the (already-upgraded) log,
//! can be discarded and rebuilt whenever the snapshot shape changes. Stored
//! events are never edited.
//!
//! # Example
//!
//! ```
//! use spork_log::{EventLog, NewEvent};
//! use spork_projection::{EventTypeCounts, ProjectionStore};
//! use serde_json::json;
//! use tempfile::TempDir;
//!
//! let dir = TempDir::new().unwrap();
//! let log = EventLog::open(&dir.path().join("log.db")).unwrap();
//! let w = log.writer();
//! w.append(NewEvent::new("node.created", 1, json!({"id": "a"}), "t")).unwrap();
//! w.append(NewEvent::new("node.created", 1, json!({"id": "b"}), "t")).unwrap();
//! w.append(NewEvent::new("edge.added", 1, json!({"from": "a"}), "t")).unwrap();
//!
//! // Rebuild the projection purely from the log.
//! let r = log.reader().unwrap();
//! let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();
//! assert_eq!(p.count("node.created"), 2);
//! assert_eq!(p.count("edge.added"), 1);
//! assert_eq!(p.last_seq(), 3);
//!
//! // A checkpoint at seq 3, then a from-scratch rebuild, hash identically.
//! let store = ProjectionStore::<EventTypeCounts>::new();
//! let ckpt = store.checkpoint(&p, 3).unwrap();
//! let rebuilt = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();
//! let ckpt2 = store.checkpoint(&rebuilt, 3).unwrap();
//! assert_eq!(ckpt.snapshot_hash, ckpt2.snapshot_hash);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod event_type_counts;
mod projection;
mod store;

pub use error::{ProjError, Result};
pub use event_type_counts::{EventTypeCounts, EventTypeCountsSnapshot};
pub use projection::{Checkpoint, Projection};
pub use store::ProjectionStore;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spork_log::{EventLog, NewEvent, Reader};
    use tempfile::TempDir;

    /// A scripted sequence of (event_type, payload) appends used across tests.
    fn sample_script() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("node.created", json!({ "id": "a" })),
            ("node.created", json!({ "id": "b" })),
            ("edge.added", json!({ "from": "a", "to": "b" })),
            ("node.created", json!({ "id": "c" })),
            ("snapshot.attached", json!({ "node": "a" })),
            ("edge.added", json!({ "from": "b", "to": "c" })),
            ("restore.performed", json!({ "to": "a" })),
        ]
    }

    /// Open a fresh log under a temp dir and append `script`, returning the dir
    /// (kept alive for the db's lifetime) and the log.
    fn log_with(script: &[(&str, serde_json::Value)]) -> (TempDir, EventLog) {
        let dir = TempDir::new().unwrap();
        let log = EventLog::open(&dir.path().join("log.db")).unwrap();
        let w = log.writer();
        for (ty, payload) in script {
            w.append(NewEvent::new(*ty, 1, payload.clone(), "tester"))
                .unwrap();
        }
        (dir, log)
    }

    fn reader(log: &EventLog) -> Reader {
        log.reader().unwrap()
    }

    // ---- the reference projection folds the log correctly ----

    #[test]
    fn rebuild_counts_event_types() {
        let (_dir, log) = log_with(&sample_script());
        let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();

        assert_eq!(p.count("node.created"), 3);
        assert_eq!(p.count("edge.added"), 2);
        assert_eq!(p.count("snapshot.attached"), 1);
        assert_eq!(p.count("restore.performed"), 1);
        assert_eq!(p.count("never.happened"), 0);
        assert_eq!(p.last_seq(), 7);
        assert_eq!(p.total(), 7);
    }

    #[test]
    fn empty_log_rebuilds_to_default() {
        let dir = TempDir::new().unwrap();
        let log = EventLog::open(&dir.path().join("log.db")).unwrap();
        let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();
        assert_eq!(p, EventTypeCounts::default());
        assert_eq!(p.last_seq(), 0);
        assert_eq!(p.total(), 0);
    }

    // ---- A.6: drop-and-rebuild yields an identical projection ----

    #[test]
    fn drop_and_rebuild_is_identical() {
        let (_dir, log) = log_with(&sample_script());
        let r = reader(&log);

        let first = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();
        // "Drop" it (it goes out of scope conceptually) and rebuild from the log.
        drop(first.clone()); // ensure the value is droppable; keep a copy to compare
        let second = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();

        // Identical in-memory state and identical snapshot.
        assert_eq!(first, second);
        assert_eq!(first.snapshot(), second.snapshot());

        // ...and identical checkpoint hash (the bit-for-bit identity, A.6).
        let store = ProjectionStore::<EventTypeCounts>::new();
        let h1 = store
            .checkpoint(&first, first.last_seq())
            .unwrap()
            .snapshot_hash;
        let h2 = store
            .checkpoint(&second, second.last_seq())
            .unwrap()
            .snapshot_hash;
        assert_eq!(h1, h2);
    }

    // ---- checkpoint + replay-after == full rebuild, bit-for-bit ----

    #[test]
    fn checkpoint_then_replay_equals_full_rebuild() {
        let script = sample_script();
        let (_dir, log) = log_with(&script);
        let r = reader(&log);
        let store = ProjectionStore::<EventTypeCounts>::new();

        // Checkpoint at every seq K in 0..=N, then load_or_rebuild and compare to
        // a full rebuild. K == 0 means "checkpoint of the empty/default state".
        let full = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();
        let full_ckpt = store.checkpoint(&full, full.last_seq()).unwrap();

        for k in 0..=script.len() as u64 {
            // Build a projection that reflects exactly seq 1..=k by folding the
            // prefix, then snapshot it at k.
            let mut prefix = EventTypeCounts::default();
            for item in r.iter_from(1).unwrap() {
                let e = item.unwrap();
                if e.seq > k {
                    break;
                }
                prefix.apply(&e);
            }
            let ckpt = store.checkpoint(&prefix, k).unwrap();

            // Load from that checkpoint and replay the tail k+1..
            let loaded =
                ProjectionStore::<EventTypeCounts>::load_or_rebuild(&r, Some(ckpt)).unwrap();

            assert_eq!(
                loaded, full,
                "checkpoint at seq {k} then replay must equal full rebuild"
            );
            let loaded_ckpt = store.checkpoint(&loaded, loaded.last_seq()).unwrap();
            assert_eq!(
                loaded_ckpt.snapshot_hash, full_ckpt.snapshot_hash,
                "snapshot hash must be bit-for-bit identical (checkpoint at seq {k})"
            );
            assert_eq!(loaded_ckpt.snapshot_bytes, full_ckpt.snapshot_bytes);
        }
    }

    #[test]
    fn load_or_rebuild_without_checkpoint_is_full_rebuild() {
        let (_dir, log) = log_with(&sample_script());
        let r = reader(&log);
        let direct = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();
        let via_none = ProjectionStore::<EventTypeCounts>::load_or_rebuild(&r, None).unwrap();
        assert_eq!(direct, via_none);
    }

    // ---- canonical snapshot hashes are stable across runs ----

    #[test]
    fn snapshot_hash_is_stable_across_independent_logs() {
        // Two separate logs, built independently from the same script, must
        // produce byte-identical snapshots and the same hash — the cross-run /
        // cross-machine stability the design depends on.
        let script = sample_script();
        let (_d1, log1) = log_with(&script);
        let (_d2, log2) = log_with(&script);

        let p1 = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log1)).unwrap();
        let p2 = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log2)).unwrap();

        let store = ProjectionStore::<EventTypeCounts>::new();
        let c1 = store.checkpoint(&p1, p1.last_seq()).unwrap();
        let c2 = store.checkpoint(&p2, p2.last_seq()).unwrap();
        assert_eq!(c1.snapshot_bytes, c2.snapshot_bytes);
        assert_eq!(c1.snapshot_hash, c2.snapshot_hash);
    }

    #[test]
    fn snapshot_hash_is_independent_of_apply_order_for_counts() {
        // EventTypeCounts is order-insensitive in its *final* count map (only the
        // last_seq tracks order). Feeding the same multiset of types in two
        // different orders, with last_seq forced equal, yields the same hash.
        let a = [("x", 1u64), ("y", 1), ("x", 1), ("z", 1)];
        let b = [("z", 1u64), ("x", 1), ("x", 1), ("y", 1)];

        let mut pa = EventTypeCounts::default();
        let mut pb = EventTypeCounts::default();
        for (i, (ty, _)) in a.iter().enumerate() {
            pa.apply(&mk_event((i as u64) + 1, ty));
        }
        for (i, (ty, _)) in b.iter().enumerate() {
            pb.apply(&mk_event((i as u64) + 1, ty));
        }
        // Same counts, same last_seq => same snapshot.
        assert_eq!(pa.snapshot(), pb.snapshot());
        let store = ProjectionStore::<EventTypeCounts>::new();
        assert_eq!(
            store.checkpoint(&pa, pa.last_seq()).unwrap().snapshot_hash,
            store.checkpoint(&pb, pb.last_seq()).unwrap().snapshot_hash
        );
    }

    /// Build a minimal valid [`spork_log::Event`] for unit-level apply tests.
    fn mk_event(seq: u64, ty: &str) -> spork_log::Event {
        use spork_log::{compute_event_hash, GENESIS_PREV_HASH};
        let payload = json!({ "seq": seq });
        let this = compute_event_hash(&GENESIS_PREV_HASH, &payload, seq, ty, 1).unwrap();
        spork_log::Event {
            event_id: ulid_for(seq),
            seq,
            event_type: ty.into(),
            schema_version: 1,
            payload,
            prev_event_hash: GENESIS_PREV_HASH,
            this_event_hash: this,
            actor: "test".into(),
        }
    }

    /// A deterministic ULID stand-in (its value is irrelevant to the projection,
    /// which keys only on `event_type`/`seq`).
    fn ulid_for(seq: u64) -> ulid::Ulid {
        ulid::Ulid::from_parts(seq, seq as u128)
    }

    // ---- checkpoint integrity ----

    #[test]
    fn checkpoint_verify_detects_tampered_bytes() {
        let (_dir, log) = log_with(&sample_script());
        let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();
        let store = ProjectionStore::<EventTypeCounts>::new();
        let mut ckpt = store.checkpoint(&p, p.last_seq()).unwrap();
        ckpt.verify().unwrap();

        // Flip a byte in the stored snapshot; the recorded hash no longer matches.
        ckpt.snapshot_bytes[0] ^= 0xFF;
        assert_eq!(ckpt.verify().unwrap_err(), ProjError::CheckpointMismatch);

        // load_or_rebuild surfaces the same mismatch rather than trusting it.
        let r = reader(&log);
        assert_eq!(
            ProjectionStore::<EventTypeCounts>::load_or_rebuild(&r, Some(ckpt)).unwrap_err(),
            ProjError::CheckpointMismatch
        );
    }

    #[test]
    fn checkpoint_round_trips_through_decode() {
        let (_dir, log) = log_with(&sample_script());
        let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();
        let store = ProjectionStore::<EventTypeCounts>::new();
        let ckpt = store.checkpoint(&p, p.last_seq()).unwrap();

        let snap: EventTypeCountsSnapshot = ckpt.decode_snapshot().unwrap();
        assert_eq!(snap, p.snapshot());
        let restored = EventTypeCounts::restore(snap);
        assert_eq!(restored, p);
    }

    #[test]
    fn checkpoint_envelope_serializes_and_round_trips() {
        let (_dir, log) = log_with(&sample_script());
        let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();
        let store = ProjectionStore::<EventTypeCounts>::new();
        let ckpt = store.checkpoint(&p, p.last_seq()).unwrap();
        assert_eq!(ckpt.schema_version, Checkpoint::SCHEMA_VERSION);

        // The Checkpoint itself is serde-round-trippable so callers can persist it.
        let bytes = serde_json::to_vec(&ckpt).unwrap();
        let back: Checkpoint = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(ckpt, back);
        back.verify().unwrap();
    }

    // ---- the restore round-trip law ----

    #[test]
    fn restore_of_snapshot_is_identity() {
        let (_dir, log) = log_with(&sample_script());
        let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();
        let restored = EventTypeCounts::restore(p.snapshot());
        assert_eq!(p, restored);
        assert_eq!(p.snapshot(), restored.snapshot());
    }

    // ---- migration-on-read flows through the projection (C5) ----

    #[test]
    fn projection_sees_upgraded_payloads_but_counts_unchanged() {
        use spork_log::CurrentVersions;
        use spork_migrate::{EventMigration, MigrationError, MigrationRegistry};
        use std::sync::Arc;

        // A v1->v2 migration that adds a field. EventTypeCounts keys only on
        // event_type, so the upgrade must not change the counts — but it proves
        // the projection is fed through the migration-on-read path.
        struct AddField;
        impl EventMigration for AddField {
            fn event_type(&self) -> &str {
                "node.created"
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
                    .insert("upgraded".into(), json!(true));
                Ok(p)
            }
        }

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");
        let log = EventLog::open(&path).unwrap();
        let w = log.writer();
        w.append(NewEvent::new("node.created", 1, json!({ "id": "a" }), "t"))
            .unwrap();
        w.append(NewEvent::new("edge.added", 1, json!({ "from": "a" }), "t"))
            .unwrap();
        w.append(NewEvent::new("node.created", 1, json!({ "id": "b" }), "t"))
            .unwrap();

        // Plain rebuild.
        let plain = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&reader(&log)).unwrap();

        // Migration-on-read rebuild.
        let mut reg = MigrationRegistry::new();
        reg.register(Box::new(AddField)).unwrap();
        let mut current = CurrentVersions::new();
        current.insert("node.created".into(), 2);
        #[allow(clippy::arc_with_non_send_sync)]
        let arc = Arc::new(reg);
        let mr = log.reader_with_migrations(arc, current).unwrap();
        let migrated = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&mr).unwrap();

        // The histogram is identical: the upgrade touched payloads, not types.
        assert_eq!(plain, migrated);
        assert_eq!(plain.snapshot(), migrated.snapshot());
    }
}

// ---- property-based tests ----

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;
    use spork_log::{EventLog, NewEvent};
    use tempfile::TempDir;

    /// A small alphabet of event types so the histogram has repeats to count.
    fn arb_type() -> impl Strategy<Value = &'static str> {
        prop_oneof![
            Just("alpha"),
            Just("beta"),
            Just("gamma"),
            Just("delta"),
            Just("epsilon"),
        ]
    }

    proptest! {
        /// For any sequence of appends and any checkpoint sequence K, restoring
        /// from the checkpoint and replaying the tail equals a full rebuild —
        /// bit-for-bit (same snapshot hash and bytes). This is the central F1
        /// invariant, fuzzed over random logs and split points.
        #[test]
        fn checkpoint_split_equals_full_rebuild(
            types in proptest::collection::vec(arb_type(), 0..40),
            split_frac in 0u64..=100,
        ) {
            let dir = TempDir::new().unwrap();
            let log = EventLog::open(&dir.path().join("log.db")).unwrap();
            let w = log.writer();
            for (i, ty) in types.iter().enumerate() {
                w.append(NewEvent::new(*ty, 1, json!({ "i": i }), "p")).unwrap();
            }
            let r = log.reader().unwrap();
            let n = types.len() as u64;
            // Pick a split point K in 0..=n derived from the fuzzed fraction.
            let k = if n == 0 { 0 } else { (split_frac * n) / 100 };

            let store = ProjectionStore::<EventTypeCounts>::new();
            let full = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();
            let full_ckpt = store.checkpoint(&full, full.last_seq()).unwrap();

            // Build the prefix projection at seq <= K and checkpoint it.
            let mut prefix = EventTypeCounts::default();
            for item in r.iter_from(1).unwrap() {
                let e = item.unwrap();
                if e.seq > k { break; }
                prefix.apply(&e);
            }
            let ckpt = store.checkpoint(&prefix, k).unwrap();

            let loaded =
                ProjectionStore::<EventTypeCounts>::load_or_rebuild(&r, Some(ckpt)).unwrap();
            let loaded_ckpt = store.checkpoint(&loaded, loaded.last_seq()).unwrap();

            prop_assert_eq!(loaded, full);
            prop_assert_eq!(&loaded_ckpt.snapshot_bytes, &full_ckpt.snapshot_bytes);
            prop_assert_eq!(loaded_ckpt.snapshot_hash, full_ckpt.snapshot_hash);
        }

        /// Counts always sum to the number of events and last_seq equals the log
        /// length, for any random append sequence.
        #[test]
        fn counts_sum_and_last_seq_are_consistent(
            types in proptest::collection::vec(arb_type(), 0..60),
        ) {
            let dir = TempDir::new().unwrap();
            let log = EventLog::open(&dir.path().join("log.db")).unwrap();
            let w = log.writer();
            for (i, ty) in types.iter().enumerate() {
                w.append(NewEvent::new(*ty, 1, json!({ "i": i }), "p")).unwrap();
            }
            let r = log.reader().unwrap();
            let p = ProjectionStore::<EventTypeCounts>::rebuild_from_log(&r).unwrap();

            prop_assert_eq!(p.total(), types.len() as u64);
            prop_assert_eq!(p.last_seq(), types.len() as u64);
            // Each type's count matches a direct tally.
            for ty in ["alpha", "beta", "gamma", "delta", "epsilon"] {
                let want = types.iter().filter(|t| **t == ty).count() as u64;
                prop_assert_eq!(p.count(ty), want);
            }
        }
    }
}
