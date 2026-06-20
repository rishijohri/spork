//! Spork F3 dual-delivery transport — one ordered stream, many side-channels.
//!
//! State and chatter travel on deliberately different rails. The [`EventStream`]
//! is the durable rail: it tails the F1 log/projection in `seq` order and
//! delivers [`OpLogEvent`](spork_ipc::OpLogEvent)s to subscribers with no gaps
//! and no reordering — this is the only channel that carries authoritative graph
//! state. The [`EphemeralBus`] is the chatter rail: it carries
//! [`EphemeralFrame`](spork_ipc::EphemeralFrame)s (chat tokens, run stdout)
//! keyed by node id on separate channels, so that flooding it with thousands of
//! frames can never stall or drop an ordered
//! [`OpLogEvent`](spork_ipc::OpLogEvent).
//!
//! Keeping these two rails physically separate is the property the future
//! renderer depends on: live token/stdout volume on a busy node must not delay
//! the graph updates the canvas reduces over.
//!
//! # The two rails, contrasted
//!
//! | | [`EventStream`] (durable) | [`EphemeralBus`] (ephemeral) |
//! |---|---|---|
//! | Payload | [`OpLogEvent`](spork_ipc::OpLogEvent) | [`EphemeralFrame`](spork_ipc::EphemeralFrame) |
//! | Ordering | global `seq`, gapless | per-node FIFO, no global order |
//! | Keying | broadcast to all subscribers | routed by `node_id` |
//! | Queues | **unbounded** (never drops state) | **bounded** (drops under flood) |
//! | On backpressure | never blocks publisher; consumer queue grows | never blocks publisher; overflow dropped |
//! | Durability | the source of truth | safe to drop/coalesce |
//!
//! The two share no channels and no threads, so neither can exert backpressure
//! on the other. That non-interference is proven directly by
//! `tests/non_interference.rs`: while a flood of millions-of-bytes' worth of
//! ephemeral frames runs, the ordered rail still delivers every
//! [`OpLogEvent`](spork_ipc::OpLogEvent) in order, gaplessly, and within a tight
//! latency budget.
//!
//! # Usage
//!
//! ```
//! use spork_stream::{EventStream, EphemeralBus};
//! use spork_ipc::{OpLogEvent, EphemeralFrame, EphemeralChannel};
//! use ulid::Ulid;
//!
//! // Durable rail: subscribe, then publish ordered events.
//! let stream = EventStream::new();
//! let events = stream.subscribe();
//! let node = Ulid::new();
//! stream.publish(OpLogEvent::NodeCreated { seq: 1, node_id: node, schema_version: 1 }).unwrap();
//! assert_eq!(events.recv().unwrap().seq(), 1);
//!
//! // Ephemeral rail: subscribe per node, publish frames (never blocks).
//! let bus = EphemeralBus::new();
//! let tokens = bus.subscribe(node);
//! bus.publish(EphemeralFrame::new(node, EphemeralChannel::ChatTokens, "hi"));
//! assert_eq!(tokens.recv().unwrap().data, "hi");
//! ```
//!
//! This realizes the headless-core delivery contract in DESIGN.md §5.5 ("Real-
//! time data flow and security boundary") and the durable-vs-ephemeral split in
//! §14.4 ("Real-Time Updates & Optimistic UI").
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod ephemeral;
mod error;
mod stream;

pub use ephemeral::{EphemeralBus, DEFAULT_EPHEMERAL_CAPACITY, EPHEMERAL_BUS_SCHEMA_VERSION};
pub use error::StreamError;
pub use stream::{EventStream, EVENT_STREAM_SCHEMA_VERSION};

// Re-export the contract vocabulary the transport carries, so a consumer binds
// to one crate (DESIGN.md §5.5, §14.4).
pub use spork_ipc::{EphemeralChannel, EphemeralFrame, OpLogEvent};

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn transport_schema_versions_are_pinned() {
        // Each transport seam carries an explicit schema version (CLAUDE.md C5).
        assert_eq!(EVENT_STREAM_SCHEMA_VERSION, 1);
        assert_eq!(EPHEMERAL_BUS_SCHEMA_VERSION, 1);
    }
}
