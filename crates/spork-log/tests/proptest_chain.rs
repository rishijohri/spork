//! Property test: random append sequences keep the chain valid and `seq`
//! contiguous (DESIGN.md A.6 event-sourcing soundness — "for any random op
//! sequence" invariants).

use proptest::prelude::*;
use serde_json::{json, Value};
use spork_log::{EventLog, NewEvent};
use tempfile::TempDir;

/// A small strategy for arbitrary *float-free* JSON payloads (canonicalization
/// forbids floats, which the writer correctly rejects; the chain invariants are
/// about the accepted appends).
fn payload_strategy() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        ".*".prop_map(Value::String),
        Just(Value::Null),
    ];
    leaf.prop_recursive(3, 16, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::hash_map("[a-z]{1,6}", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// One append: an event type, a schema version, an actor, and a payload.
fn append_strategy() -> impl Strategy<Value = (String, u16, String, Value)> {
    (
        "[a-z]{1,8}(\\.[a-z]{1,8})?",
        1u16..=5,
        "[a-z]{1,6}",
        payload_strategy(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40))]

    #[test]
    fn random_appends_keep_chain_valid_and_contiguous(
        appends in prop::collection::vec(append_strategy(), 1..30)
    ) {
        let dir = TempDir::new().unwrap();
        let log = EventLog::open(&dir.path().join("log.db")).unwrap();
        let w = log.writer();

        let mut prev = spork_log::GENESIS_PREV_HASH;
        for (expected_seq, (ty, ver, actor, payload)) in (1u64..).zip(appends.iter()) {
            let e = w
                .append(NewEvent::new(ty.clone(), *ver, payload.clone(), actor.clone()))
                .unwrap();
            prop_assert_eq!(e.seq, expected_seq);
            prop_assert_eq!(e.prev_event_hash, prev);
            // Stored hash reproduces from the event's own fields.
            e.verify_self().unwrap();
            prev = e.this_event_hash;
        }

        // The whole chain verifies and the length matches.
        let r = log.reader().unwrap();
        prop_assert_eq!(r.len().unwrap(), appends.len() as u64);
        r.verify_chain().unwrap();

        // Round-trip: every appended payload reads back equal (canonicalization
        // is value-preserving, so logical equality holds).
        for (i, (ty, ver, _actor, payload)) in appends.iter().enumerate() {
            let got = r.get((i + 1) as u64).unwrap().unwrap();
            prop_assert_eq!(&got.event_type, ty);
            prop_assert_eq!(got.schema_version, *ver);
            prop_assert_eq!(&got.payload, payload);
        }
    }

    #[test]
    fn reopen_then_append_preserves_chain(
        first in prop::collection::vec(append_strategy(), 1..15),
        second in prop::collection::vec(append_strategy(), 0..15),
    ) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.db");

        {
            let log = EventLog::open(&path).unwrap();
            let w = log.writer();
            for (ty, ver, actor, payload) in &first {
                w.append(NewEvent::new(ty.clone(), *ver, payload.clone(), actor.clone())).unwrap();
            }
        }

        let log = EventLog::open(&path).unwrap();
        let w = log.writer();
        for (ty, ver, actor, payload) in &second {
            w.append(NewEvent::new(ty.clone(), *ver, payload.clone(), actor.clone())).unwrap();
        }

        let r = log.reader().unwrap();
        prop_assert_eq!(r.len().unwrap(), (first.len() + second.len()) as u64);
        r.verify_chain().unwrap();
    }
}
