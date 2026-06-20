//! The chatter rail: [`EphemeralBus`].
//!
//! High-frequency, low-durability data — agent chat tokens, run/test stdout —
//! must reach the renderer *without ever stalling graph state*. The
//! [`EphemeralBus`] is that rail: it carries [`EphemeralFrame`]s keyed by
//! `node_id` on channels **physically separate** from the ordered
//! [`EventStream`](crate::EventStream), so a flood of frames on a busy node can
//! never occupy, block, or delay durable [`OpLogEvent`] delivery (DESIGN.md
//! §5.5, §14.4).
//!
//! # Per-node routing
//!
//! A subscriber asks for one `node_id` and receives only that node's frames —
//! the renderer routes them to that node's chat panel / run rail (DESIGN.md
//! §14.2/§14.5). Frames carry **no `seq`** and have no cross-node ordering;
//! within a single node's channel they are delivered FIFO.
//!
//! # Bounded, droppable — by design
//!
//! Unlike the durable rail, each per-node subscriber sink is a **bounded**
//! channel. Ephemeral data is explicitly safe to drop or coalesce under
//! backpressure (DESIGN.md §5.5: "token volume never stalls the graph"), so
//! [`EphemeralBus::publish`] **never blocks**: if a subscriber's bounded queue
//! is full it drops the frame for that subscriber (counting it) rather than
//! waiting. This is what guarantees a slow or absent ephemeral consumer can
//! never become backpressure that bleeds into the ordered rail.
//!
//! Design references: DESIGN.md §5.5, §14.2, §14.4.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use spork_ipc::EphemeralFrame;
use ulid::Ulid;

/// The schema/version tag of the ephemeral-bus transport contract.
///
/// The frame payload ([`EphemeralFrame`]) is versioned in `spork-ipc`; this pins
/// the transport seam itself (CLAUDE.md C5).
pub const EPHEMERAL_BUS_SCHEMA_VERSION: u16 = 1;

/// The default bound on each per-node subscriber queue.
///
/// Generous enough that a well-behaved consumer never loses frames, small enough
/// that a runaway producer cannot grow memory without bound. When the queue
/// fills, [`EphemeralBus::publish`] drops the newest frame for that subscriber
/// (and increments the drop counter) instead of blocking — the explicit
/// "ephemeral data is droppable" contract (DESIGN.md §5.5).
pub const DEFAULT_EPHEMERAL_CAPACITY: usize = 16_384;

/// The node-keyed ephemeral side-channel bus.
///
/// Clone-cheap (shares one subscriber table). Publish frames with
/// [`EphemeralBus::publish`]; subscribe per node with
/// [`EphemeralBus::subscribe`]. The bus owns no threads of its own — it is a
/// non-blocking fan-out point — which is precisely why it cannot interfere with
/// the ordered rail.
#[derive(Clone)]
pub struct EphemeralBus {
    inner: Arc<Inner>,
}

struct Inner {
    /// Per-node subscriber sinks. A node may have many subscribers (e.g. several
    /// renderer panels); each gets its own bounded queue.
    subscribers: Mutex<HashMap<Ulid, Vec<Sender<EphemeralFrame>>>>,
    /// The bound applied to each new subscriber queue.
    capacity: usize,
    /// Total frames dropped across all subscribers because a bounded queue was
    /// full. Observability for the "ephemeral is droppable" contract; the
    /// integration test asserts the ordered rail is unaffected regardless.
    dropped: AtomicU64,
    /// Total frames delivered to at least one subscriber.
    delivered: AtomicU64,
}

impl EphemeralBus {
    /// Create a bus with the [`DEFAULT_EPHEMERAL_CAPACITY`] per-subscriber bound.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_EPHEMERAL_CAPACITY)
    }

    /// Create a bus with an explicit per-subscriber queue bound.
    ///
    /// A bound of `0` is treated as `1` (crossbeam's `bounded(0)` is a
    /// rendezvous channel, which would make `publish` block — incompatible with
    /// the never-stall contract), preserving the non-blocking guarantee.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                subscribers: Mutex::new(HashMap::new()),
                capacity: capacity.max(1),
                dropped: AtomicU64::new(0),
                delivered: AtomicU64::new(0),
            }),
        }
    }

    /// Subscribe to one node's ephemeral frames.
    ///
    /// Returns a bounded [`Receiver<EphemeralFrame>`] that yields only frames
    /// whose `node_id` matches `node_id`, in publish order. Multiple subscribers
    /// per node are allowed; each gets an independent queue.
    #[must_use]
    pub fn subscribe(&self, node_id: Ulid) -> Receiver<EphemeralFrame> {
        let (tx, rx) = bounded(self.inner.capacity);
        self.inner
            .subscribers
            .lock()
            .expect("ephemeral subscriber lock poisoned")
            .entry(node_id)
            .or_default()
            .push(tx);
        rx
    }

    /// Publish a frame to every subscriber of its node — **never blocking**.
    ///
    /// Routing is by `frame.node_id`. For each matching subscriber the frame is
    /// offered with a non-blocking try-send: if that subscriber's bounded queue
    /// is full the frame is dropped *for that subscriber* (the drop counter is
    /// incremented) rather than blocking the publisher. Subscribers whose
    /// receiver has been dropped are pruned. A frame for a node with no
    /// subscribers is silently discarded — ephemeral data has no durable home.
    ///
    /// This non-blocking behaviour is the load-bearing property: because the
    /// publisher can never wait here, no volume of ephemeral traffic can become
    /// backpressure on the caller, and the ordered rail (separate channels,
    /// unbounded) stays unaffected (DESIGN.md §5.5, §14.4).
    pub fn publish(&self, frame: EphemeralFrame) {
        let mut subs = self
            .inner
            .subscribers
            .lock()
            .expect("ephemeral subscriber lock poisoned");
        let Some(sinks) = subs.get_mut(&frame.node_id) else {
            return;
        };
        let mut any_delivered = false;
        sinks.retain(|tx| match tx.try_send(frame.clone()) {
            Ok(()) => {
                any_delivered = true;
                true
            }
            // Queue full: drop this frame for this subscriber, keep the sink.
            Err(TrySendError::Full(_)) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            // Receiver gone: prune the sink.
            Err(TrySendError::Disconnected(_)) => false,
        });
        // Clean up an emptied node entry so the table does not grow unbounded.
        if sinks.is_empty() {
            subs.remove(&frame.node_id);
        }
        if any_delivered {
            self.inner.delivered.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The number of subscribers currently registered for `node_id`.
    #[must_use]
    pub fn subscriber_count(&self, node_id: Ulid) -> usize {
        self.inner
            .subscribers
            .lock()
            .expect("ephemeral subscriber lock poisoned")
            .get(&node_id)
            .map_or(0, Vec::len)
    }

    /// Total frames dropped (across all subscribers) due to a full bounded queue.
    ///
    /// Observability for the droppable-under-backpressure contract; it is
    /// expected to be non-zero under a deliberate flood and does not indicate an
    /// error.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Total frames delivered to at least one subscriber.
    #[must_use]
    pub fn delivered(&self) -> u64 {
        self.inner.delivered.load(Ordering::Relaxed)
    }
}

impl Default for EphemeralBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EphemeralBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EphemeralBus")
            .field("capacity", &self.inner.capacity)
            .field("delivered", &self.delivered())
            .field("dropped", &self.dropped())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_ipc::EphemeralChannel;

    fn frame(node: Ulid, data: &str) -> EphemeralFrame {
        EphemeralFrame::new(node, EphemeralChannel::ChatTokens, data)
    }

    #[test]
    fn frames_are_routed_only_to_their_nodes_subscribers() {
        let bus = EphemeralBus::new();
        let a = Ulid::new();
        let b = Ulid::new();
        let rx_a = bus.subscribe(a);
        let rx_b = bus.subscribe(b);

        bus.publish(frame(a, "for-a-1"));
        bus.publish(frame(b, "for-b-1"));
        bus.publish(frame(a, "for-a-2"));

        assert_eq!(rx_a.recv().unwrap().data, "for-a-1");
        assert_eq!(rx_a.recv().unwrap().data, "for-a-2");
        assert!(rx_a.try_recv().is_err());

        assert_eq!(rx_b.recv().unwrap().data, "for-b-1");
        assert!(rx_b.try_recv().is_err());
    }

    #[test]
    fn frames_within_a_node_are_fifo() {
        let bus = EphemeralBus::new();
        let n = Ulid::new();
        let rx = bus.subscribe(n);
        for i in 0..50 {
            bus.publish(frame(n, &format!("t{i}")));
        }
        for i in 0..50 {
            assert_eq!(rx.recv().unwrap().data, format!("t{i}"));
        }
    }

    #[test]
    fn multiple_subscribers_per_node_each_get_every_frame() {
        let bus = EphemeralBus::new();
        let n = Ulid::new();
        let r1 = bus.subscribe(n);
        let r2 = bus.subscribe(n);
        assert_eq!(bus.subscriber_count(n), 2);
        bus.publish(frame(n, "x"));
        assert_eq!(r1.recv().unwrap().data, "x");
        assert_eq!(r2.recv().unwrap().data, "x");
    }

    #[test]
    fn publish_to_node_with_no_subscribers_is_a_noop() {
        let bus = EphemeralBus::new();
        // No subscribers: must not block or panic, just discard.
        bus.publish(frame(Ulid::new(), "lost"));
        assert_eq!(bus.delivered(), 0);
    }

    #[test]
    fn publish_never_blocks_even_when_the_queue_overflows() {
        // A tiny queue and a consumer that never reads: publish must return
        // immediately, dropping the overflow rather than blocking the producer.
        let bus = EphemeralBus::with_capacity(4);
        let n = Ulid::new();
        let _rx = bus.subscribe(n); // held but never drained
        for i in 0..1_000 {
            bus.publish(frame(n, &format!("f{i}")));
        }
        // Far more frames were published than the queue can hold, so some were
        // dropped — but publish never blocked (the test completing proves it).
        assert!(bus.dropped() > 0, "overflow should have dropped frames");
        assert!(bus.dropped() >= 1_000 - 4);
    }

    #[test]
    fn dropped_subscriber_is_pruned_on_publish() {
        let bus = EphemeralBus::new();
        let n = Ulid::new();
        {
            let _rx = bus.subscribe(n);
            assert_eq!(bus.subscriber_count(n), 1);
        } // receiver dropped
        bus.publish(frame(n, "x")); // triggers prune of the dead sink
        assert_eq!(bus.subscriber_count(n), 0);
    }

    #[test]
    fn zero_capacity_is_clamped_to_non_blocking() {
        // bounded(0) would be a rendezvous channel; we clamp to 1 so publish
        // stays non-blocking.
        let bus = EphemeralBus::with_capacity(0);
        let n = Ulid::new();
        let rx = bus.subscribe(n);
        bus.publish(frame(n, "a"));
        bus.publish(frame(n, "b")); // must not block even with no reader yet
        assert_eq!(rx.recv().unwrap().data, "a");
    }
}
