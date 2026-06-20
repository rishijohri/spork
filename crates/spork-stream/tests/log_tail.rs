//! Integration test: the ordered rail tails the **real** F1 event log and
//! delivers gapless, in-`seq`-order [`OpLogEvent`]s.
//!
//! This exercises the bridge from `spork-log` (the source of truth) to
//! [`EventStream`] over an actual SQLite-WAL log — not a mock — proving the
//! transport preserves the log's contiguous `seq` ordering end to end
//! (DESIGN.md §5.5, §6.1, §14.4).

use spork_ipc::OpLogEvent;
use spork_log::{EventLog, NewEvent};
use spork_stream::{EventStream, StreamError};
use tempfile::TempDir;
use ulid::Ulid;

/// Map a stored log event to its renderer-facing `OpLogEvent`. This stands in
/// for the daemon's log->event projection: a `"node.created"` log entry becomes
/// an [`OpLogEvent::NodeCreated`], reusing the log-assigned `seq` so the rail's
/// ordering is exactly the log's ordering.
fn map_event(e: &spork_log::Event) -> Option<OpLogEvent> {
    match e.event_type.as_str() {
        "node.created" => {
            let node_id = e
                .payload
                .get("nodeId")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Ulid>().ok())
                .unwrap_or_else(Ulid::nil);
            Some(OpLogEvent::NodeCreated {
                seq: e.seq,
                node_id,
                schema_version: e.schema_version,
            })
        }
        // Other log types have no renderer projection in this test fixture.
        _ => None,
    }
}

#[test]
fn tailing_the_real_log_delivers_events_in_seq_order_with_no_gaps() {
    let dir = TempDir::new().unwrap();
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let w = log.writer();

    // Append 200 renderer-relevant events through the real F1 writer actor.
    let mut node_ids = Vec::new();
    for _ in 0..200 {
        let id = Ulid::new();
        node_ids.push(id);
        w.append(NewEvent::new(
            "node.created",
            1,
            serde_json::json!({ "nodeId": id.to_string() }),
            "tester",
        ))
        .unwrap();
    }

    // Tail the committed log onto the ordered rail.
    let stream = EventStream::new();
    let rx = stream.subscribe();
    let reader = log.reader().unwrap();
    let published = stream.tail_reader(&reader, 1, map_event).unwrap();
    assert_eq!(published, 200);

    // The rail delivered every event, contiguous from seq 1, in node order.
    let mut last = 0u64;
    for expected_id in &node_ids {
        let e = rx.recv().unwrap();
        assert_eq!(e.seq(), last + 1, "ordered rail must be gapless");
        last = e.seq();
        match e {
            OpLogEvent::NodeCreated { node_id, .. } => assert_eq!(node_id, *expected_id),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(last, 200);
    assert!(rx.try_recv().is_err(), "no extra events");
}

#[test]
fn resuming_the_tail_continues_gaplessly_across_two_passes() {
    // The rail's `seq` IS the log's `seq`. A daemon that tails, stops, then
    // resumes from where it left off (e.g. after a projection checkpoint) gets a
    // single contiguous ordered stream across both passes.
    let dir = TempDir::new().unwrap();
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let w = log.writer();

    let stream = EventStream::new();
    let rx = stream.subscribe();

    // First batch, first pass.
    for _ in 0..40 {
        w.append(NewEvent::new(
            "node.created",
            1,
            serde_json::json!({ "nodeId": Ulid::new().to_string() }),
            "tester",
        ))
        .unwrap();
    }
    let n1 = stream
        .tail_reader(&log.reader().unwrap(), 1, map_event)
        .unwrap();
    assert_eq!(n1, 40);
    assert_eq!(stream.next_seq(), 41);

    // More events arrive; resume the tail from the rail's cursor.
    for _ in 0..35 {
        w.append(NewEvent::new(
            "node.created",
            1,
            serde_json::json!({ "nodeId": Ulid::new().to_string() }),
            "tester",
        ))
        .unwrap();
    }
    let n2 = stream
        .tail_reader(&log.reader().unwrap(), stream.next_seq(), map_event)
        .unwrap();
    assert_eq!(n2, 35);

    // The subscriber observed one gapless sequence 1..=75 across both passes.
    let mut last = 0u64;
    for _ in 0..75 {
        let e = rx.recv().unwrap();
        assert_eq!(e.seq(), last + 1, "resumed tail must stay gapless");
        last = e.seq();
    }
    assert_eq!(last, 75);
    assert!(rx.try_recv().is_err());
}

#[test]
fn tailing_from_a_later_seq_yields_only_the_suffix() {
    let dir = TempDir::new().unwrap();
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let w = log.writer();
    let mut ids = Vec::new();
    for _ in 0..10 {
        let id = Ulid::new();
        ids.push(id);
        w.append(NewEvent::new(
            "node.created",
            1,
            serde_json::json!({ "nodeId": id.to_string() }),
            "tester",
        ))
        .unwrap();
    }

    // A fresh rail primed to start at seq 6 (e.g. resuming after a checkpoint).
    let stream = EventStream::new();
    // Publish a synthetic preamble so the rail's cursor sits at 6 before tail.
    for seq in 1..=5 {
        stream
            .publish(OpLogEvent::NodeCreated {
                seq,
                node_id: ids[(seq - 1) as usize],
                schema_version: 1,
            })
            .unwrap();
    }
    let rx = stream.subscribe();
    let reader = log.reader().unwrap();
    let published = stream.tail_reader(&reader, 6, map_event).unwrap();
    assert_eq!(published, 5);
    let seqs: Vec<u64> = (0..5).map(|_| rx.recv().unwrap().seq()).collect();
    assert_eq!(seqs, vec![6, 7, 8, 9, 10]);
}

#[test]
fn tail_surfaces_a_seq_gap_if_the_mapping_violates_ordering() {
    // If a (buggy) mapping produced a non-contiguous seq, the rail catches it
    // instead of silently delivering a gap. We force this by mapping the log's
    // own seq through a hole.
    let dir = TempDir::new().unwrap();
    let log = EventLog::open(&dir.path().join("log.db")).unwrap();
    let w = log.writer();
    for _ in 0..5 {
        w.append(NewEvent::new(
            "node.created",
            1,
            serde_json::json!({ "nodeId": Ulid::new().to_string() }),
            "tester",
        ))
        .unwrap();
    }
    let stream = EventStream::new();
    let _rx = stream.subscribe();
    let reader = log.reader().unwrap();
    // Skip the log's seq 3, leaving a hole on the rail.
    let err = stream
        .tail_reader(&reader, 1, |e| {
            if e.seq == 3 {
                None
            } else {
                Some(OpLogEvent::NodeCreated {
                    seq: e.seq,
                    node_id: Ulid::nil(),
                    schema_version: 1,
                })
            }
        })
        .unwrap_err();
    assert!(matches!(
        err,
        StreamError::SeqGap {
            expected: 3,
            got: 4
        }
    ));
}
