//! The ordered durable rail: [`EventStream`].
//!
//! `EventStream` is the *authoritative* delivery channel — it carries
//! [`OpLogEvent`]s, the renderer-facing projection of the F1 op-log, to every
//! subscriber **in `seq` order with no gaps** (DESIGN.md §5.5, §14.4). This is
//! the only rail that conveys graph state: a mutation's reply is just an
//! `op_id`, and the resulting [`OpLogEvent`]s arrive here (the frozen
//! "op_id-returns-then-state-via-events" rule of the IPC contract).
//!
//! # Why fan-out is the design
//!
//! The F1 log already assigns each event a contiguous `seq` and serializes all
//! writes through one writer actor, so the events handed to [`EventStream`] are
//! already globally ordered. The transport's job is therefore not *re-ordering*
//! but **gapless, ordered fan-out**: one publishing seam, N independent
//! subscriber queues, each receiving the same monotonically-`seq`'d sequence.
//! The publish path validates the invariant — a `seq` that is not exactly
//! `last + 1` is a [`StreamError::SeqGap`], surfaced rather than swallowed.
//!
//! # Independence from the ephemeral rail
//!
//! Subscriber queues here are **unbounded** crossbeam channels, and the
//! ephemeral rail ([`EphemeralBus`](crate::EphemeralBus)) lives on entirely
//! separate channels and threads. A flood of chat tokens or stdout therefore
//! cannot occupy, block, or starve this rail — the non-interference property the
//! future renderer depends on (proven by the crate's integration test).
//!
//! Design references: DESIGN.md §5.5, §6.1, §14.4.

use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use spork_ipc::OpLogEvent;
use spork_log::Reader;

use crate::error::StreamError;

/// The schema/version tag of the ordered-stream transport contract.
///
/// The *payload* it carries ([`OpLogEvent`]) is versioned in `spork-ipc`; this
/// constant pins the transport seam itself (CLAUDE.md C5) so a future change to
/// the delivery discipline (e.g. resumable cursors) is an explicit bump.
pub const EVENT_STREAM_SCHEMA_VERSION: u16 = 1;

/// The publisher and fan-out point for the ordered durable rail.
///
/// Clone-cheap: cloning shares the same subscriber set and seq cursor, so the
/// daemon can hold one publishing handle while the log-tailer holds another.
/// Subscribe with [`EventStream::subscribe`] to get an independent ordered
/// [`Receiver`]; publish with [`EventStream::publish`] (validating gaplessness)
/// or fill a fresh subscriber's backlog with [`EventStream::tail_reader`].
#[derive(Clone)]
pub struct EventStream {
    inner: Arc<Inner>,
}

struct Inner {
    /// The live subscriber sinks. Guarded by a mutex; the lock is held only for
    /// the duration of a single fan-out (a handful of channel sends), never
    /// across blocking work, so publishing stays cheap.
    subscribers: Mutex<Vec<Sender<OpLogEvent>>>,
    /// The `seq` the next published event must carry (last delivered + 1), or
    /// `1` before anything has been published. Lets [`EventStream::publish`]
    /// enforce the gapless invariant.
    next_seq: Mutex<u64>,
}

impl EventStream {
    /// Create an empty ordered stream with no subscribers, expecting `seq == 1`
    /// first.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                subscribers: Mutex::new(Vec::new()),
                next_seq: Mutex::new(1),
            }),
        }
    }

    /// Register a new subscriber and return its ordered, gapless
    /// [`Receiver<OpLogEvent>`].
    ///
    /// Each subscriber gets its own **unbounded** channel, so a slow consumer
    /// cannot stall the publisher or other subscribers, and so the rail can
    /// never exert backpressure that the ephemeral flood could exploit. Events
    /// published *after* this call are delivered in order; to also receive the
    /// already-committed history, prime the stream from a [`Reader`] via
    /// [`EventStream::tail_reader`] before publishing live events, or subscribe
    /// first and then tail.
    #[must_use]
    pub fn subscribe(&self) -> Receiver<OpLogEvent> {
        let (tx, rx) = unbounded();
        self.inner
            .subscribers
            .lock()
            .expect("event-stream subscriber lock poisoned")
            .push(tx);
        rx
    }

    /// The number of currently live subscribers (those whose receiver has not
    /// been dropped is pruned lazily on publish; this counts registered sinks).
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.inner
            .subscribers
            .lock()
            .expect("event-stream subscriber lock poisoned")
            .len()
    }

    /// The `seq` the next published event must carry.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        *self
            .inner
            .next_seq
            .lock()
            .expect("event-stream seq lock poisoned")
    }

    /// Publish one event to every subscriber, enforcing the gapless `seq`
    /// invariant.
    ///
    /// The event's `seq` must be exactly the stream's expected next `seq`
    /// (`1` for the first event, `last + 1` thereafter); otherwise a
    /// [`StreamError::SeqGap`] is returned and **nothing is delivered** — the
    /// cursor does not advance, so the caller can retry with the correct event.
    /// On success the event fans out to every live subscriber in order; sinks
    /// whose receiver was dropped are pruned. Delivery is non-blocking because
    /// every subscriber queue is unbounded.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::SeqGap`] if `event.seq()` is not the expected next
    /// `seq`.
    pub fn publish(&self, event: OpLogEvent) -> Result<(), StreamError> {
        let mut next = self
            .inner
            .next_seq
            .lock()
            .expect("event-stream seq lock poisoned");
        let got = event.seq();
        if got != *next {
            return Err(StreamError::SeqGap {
                expected: *next,
                got,
            });
        }
        // Advance the cursor only after the gap check passes.
        *next = next.saturating_add(1);

        let mut subs = self
            .inner
            .subscribers
            .lock()
            .expect("event-stream subscriber lock poisoned");
        // Send to each subscriber, dropping any whose receiver has gone away.
        subs.retain(|tx| tx.send(event.clone()).is_ok());
        Ok(())
    }

    /// Tail the F1 event log from `from_seq`, mapping each stored [`Event`] to an
    /// [`OpLogEvent`] with `map` and publishing it in order.
    ///
    /// This is the bridge from the durable log (the source of truth) to the
    /// ordered rail: the daemon supplies a `map` closure that turns a persisted
    /// `spork_log::Event` into the renderer-facing [`OpLogEvent`] (a closure
    /// because the log→event projection is the daemon's concern, not the
    /// transport's). The reader yields entries in contiguous log-`seq` order, so
    /// the mapped events — which **reuse the log's `seq`** — are themselves
    /// contiguous and publish gaplessly; any residual hole is still caught by
    /// [`EventStream::publish`] and surfaced as a [`StreamError::SeqGap`] rather
    /// than silently delivered.
    ///
    /// `map` returning `None` means "this log entry is not a renderer-facing
    /// durable transition" (e.g. a projection checkpoint marker). Because the
    /// rail's `seq` *is* the log's `seq`, the supported discipline is that the
    /// set of log entries the daemon emits to the rail forms a contiguous `seq`
    /// run: the daemon writes its renderer-facing graph transitions and its
    /// internal-only markers into *separate* sequences (or filters before the
    /// rail), so the events that do reach this method are gapless. Returning
    /// `None` for an entry that sits *inside* the rail's contiguous run is a
    /// programming error and is correctly reported as a [`StreamError::SeqGap`]
    /// on the following entry — the transport never papers over a hole.
    ///
    /// Returns the number of events published.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Log`] if reading the log fails, or
    /// [`StreamError::SeqGap`] if a mapped event violates the gapless invariant.
    pub fn tail_reader<F>(
        &self,
        reader: &Reader,
        from_seq: u64,
        mut map: F,
    ) -> Result<u64, StreamError>
    where
        F: FnMut(&spork_log::Event) -> Option<OpLogEvent>,
    {
        let mut published = 0u64;
        for item in reader.iter_from(from_seq)? {
            let event = item?;
            if let Some(op_event) = map(&event) {
                self.publish(op_event)?;
                published += 1;
            }
        }
        Ok(published)
    }
}

impl Default for EventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("subscribers", &self.subscriber_count())
            .field("next_seq", &self.next_seq())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_ipc::{EdgeType, OpLogEvent};
    use ulid::Ulid;

    fn node_created(seq: u64) -> OpLogEvent {
        OpLogEvent::NodeCreated {
            seq,
            node_id: Ulid::new(),
            schema_version: 1,
        }
    }

    #[test]
    fn published_events_arrive_in_seq_order_with_no_gaps() {
        let stream = EventStream::new();
        let rx = stream.subscribe();
        for seq in 1..=100 {
            stream.publish(node_created(seq)).unwrap();
        }
        let mut last = 0;
        for _ in 1..=100 {
            let e = rx.recv().unwrap();
            assert_eq!(e.seq(), last + 1, "events must be contiguous");
            last = e.seq();
        }
        assert_eq!(last, 100);
    }

    #[test]
    fn multiple_subscribers_each_see_the_full_ordered_stream() {
        let stream = EventStream::new();
        let a = stream.subscribe();
        let b = stream.subscribe();
        assert_eq!(stream.subscriber_count(), 2);
        for seq in 1..=10 {
            stream.publish(node_created(seq)).unwrap();
        }
        for rx in [&a, &b] {
            let seqs: Vec<u64> = (0..10).map(|_| rx.recv().unwrap().seq()).collect();
            assert_eq!(seqs, (1..=10).collect::<Vec<_>>());
        }
    }

    #[test]
    fn subscriber_only_sees_events_published_after_it_joined() {
        let stream = EventStream::new();
        stream.publish(node_created(1)).unwrap();
        let rx = stream.subscribe();
        stream.publish(node_created(2)).unwrap();
        stream.publish(node_created(3)).unwrap();
        assert_eq!(rx.recv().unwrap().seq(), 2);
        assert_eq!(rx.recv().unwrap().seq(), 3);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn out_of_order_publish_is_a_seq_gap_and_delivers_nothing() {
        let stream = EventStream::new();
        let rx = stream.subscribe();
        stream.publish(node_created(1)).unwrap();
        // Jumping from 1 to 3 skips 2 — a gap.
        let err = stream.publish(node_created(3)).unwrap_err();
        match err {
            StreamError::SeqGap { expected, got } => {
                assert_eq!(expected, 2);
                assert_eq!(got, 3);
            }
            other => panic!("expected SeqGap, got {other:?}"),
        }
        // The cursor did not advance and the bad event was not delivered.
        assert_eq!(stream.next_seq(), 2);
        assert_eq!(rx.recv().unwrap().seq(), 1);
        assert!(rx.try_recv().is_err());
        // Retrying with the correct seq succeeds.
        stream.publish(node_created(2)).unwrap();
        assert_eq!(rx.recv().unwrap().seq(), 2);
    }

    #[test]
    fn first_event_must_be_seq_one() {
        let stream = EventStream::new();
        let err = stream.publish(node_created(5)).unwrap_err();
        assert!(matches!(
            err,
            StreamError::SeqGap {
                expected: 1,
                got: 5
            }
        ));
    }

    #[test]
    fn dropped_subscriber_is_pruned_and_others_unaffected() {
        let stream = EventStream::new();
        let live = stream.subscribe();
        {
            let _dead = stream.subscribe();
            assert_eq!(stream.subscriber_count(), 2);
        } // _dead's receiver drops here
        stream.publish(node_created(1)).unwrap();
        // The dead sink is pruned on the failed send; the live one still works.
        assert_eq!(stream.subscriber_count(), 1);
        assert_eq!(live.recv().unwrap().seq(), 1);
    }

    #[test]
    fn cloned_handles_share_cursor_and_subscribers() {
        let stream = EventStream::new();
        let rx = stream.subscribe();
        let clone = stream.clone();
        clone.publish(node_created(1)).unwrap();
        // The clone's publish advanced the shared cursor...
        assert_eq!(stream.next_seq(), 2);
        // ...and reached the subscriber registered on the original handle.
        assert_eq!(rx.recv().unwrap().seq(), 1);
    }

    #[test]
    fn edge_events_round_trip_through_the_rail() {
        // The rail is payload-agnostic across the OpLogEvent variants.
        let stream = EventStream::new();
        let rx = stream.subscribe();
        let from = Ulid::new();
        let to = Ulid::new();
        stream
            .publish(OpLogEvent::EdgeAdded {
                seq: 1,
                from,
                to,
                edge: EdgeType::ParentChild,
            })
            .unwrap();
        match rx.recv().unwrap() {
            OpLogEvent::EdgeAdded { from: f, to: t, .. } => {
                assert_eq!((f, t), (from, to));
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
}
