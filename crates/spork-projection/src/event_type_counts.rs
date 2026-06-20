//! The reference projection: [`EventTypeCounts`].
//!
//! A deterministic histogram of event types — the smallest non-trivial fold
//! that still exercises every part of the framework (replay, checkpoint,
//! restore, rebuild). F1 has no node/graph semantics yet (those land in F2), so
//! this projection exists to *prove* the projection layer end to end: its
//! snapshot canonically serializes deterministically, so a full replay and a
//! checkpoint-plus-replay produce an identical snapshot hash, and dropping it
//! and rebuilding from the log reproduces it bit-for-bit (DESIGN.md §6.1, A.6).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use spork_log::Event;

use crate::projection::Projection;

/// A projection counting how many events of each type the log holds.
///
/// State is a map from event type to its occurrence count, plus the highest
/// sequence number folded so far. The map is a [`BTreeMap`] — key-ordered, not
/// hash-ordered — which is what makes the snapshot serialize the same way every
/// time and therefore hash stably across machines and runs (the canonical
/// stability law on [`Projection`]).
///
/// # Example
///
/// ```
/// use spork_projection::{EventTypeCounts, Projection};
/// use spork_log::{Event, GENESIS_PREV_HASH, compute_event_hash};
/// use serde_json::json;
/// use ulid::Ulid;
///
/// fn ev(seq: u64, ty: &str) -> Event {
///     let payload = json!({ "seq": seq });
///     let this = compute_event_hash(&GENESIS_PREV_HASH, &payload, seq, ty, 1).unwrap();
///     Event {
///         event_id: Ulid::new(),
///         seq,
///         event_type: ty.into(),
///         schema_version: 1,
///         payload,
///         prev_event_hash: GENESIS_PREV_HASH,
///         this_event_hash: this,
///         actor: "doc".into(),
///     }
/// }
///
/// let mut p = EventTypeCounts::default();
/// p.apply(&ev(1, "node.created"));
/// p.apply(&ev(2, "node.created"));
/// p.apply(&ev(3, "edge.added"));
///
/// assert_eq!(p.count("node.created"), 2);
/// assert_eq!(p.count("edge.added"), 1);
/// assert_eq!(p.count("missing"), 0);
/// assert_eq!(p.last_seq(), 3);
/// assert_eq!(p.total(), 3);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventTypeCounts {
    counts: BTreeMap<String, u64>,
    last_seq: u64,
}

impl EventTypeCounts {
    /// The number of events seen of `event_type` (`0` if none).
    #[must_use]
    pub fn count(&self, event_type: &str) -> u64 {
        self.counts.get(event_type).copied().unwrap_or(0)
    }

    /// The highest `seq` folded into this projection (`0` if empty).
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// The total number of events folded (the sum of all per-type counts).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.counts.values().copied().sum()
    }

    /// Borrow the per-type counts as a key-ordered map.
    #[must_use]
    pub fn counts(&self) -> &BTreeMap<String, u64> {
        &self.counts
    }
}

/// The canonically-serializable snapshot of an [`EventTypeCounts`].
///
/// A plain, field-ordered, derive-`Serialize` struct over a [`BTreeMap`]. Run
/// through `spork-canon` it produces byte-identical output for equal states (map
/// keys are byte-sorted by the canonical encoder, counts are integers), which is
/// exactly the determinism a checkpoint hash depends on. It carries a
/// `schema_version` (CLAUDE.md C5) so the snapshot's persisted shape can evolve
/// additively via a migration registry rather than in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTypeCountsSnapshot {
    /// The schema version of this snapshot's shape (CLAUDE.md C5).
    ///
    /// Frozen at [`EventTypeCountsSnapshot::SCHEMA_VERSION`] for F1.
    pub schema_version: u16,
    /// The highest folded `seq` (`0` when empty).
    pub last_seq: u64,
    /// The per-type counts, key-ordered for canonical stability.
    pub counts: BTreeMap<String, u64>,
}

impl EventTypeCountsSnapshot {
    /// The frozen schema version of this snapshot shape (CLAUDE.md C5).
    pub const SCHEMA_VERSION: u16 = 1;
}

impl Projection for EventTypeCounts {
    type Snapshot = EventTypeCountsSnapshot;

    fn apply(&mut self, event: &Event) {
        *self.counts.entry(event.event_type.clone()).or_insert(0) += 1;
        // `seq` is contiguous and ascending, so the last event applied carries
        // the highest seq; tracking the max is robust even if a caller replays a
        // suffix onto a restored snapshot.
        if event.seq > self.last_seq {
            self.last_seq = event.seq;
        }
    }

    fn snapshot(&self) -> Self::Snapshot {
        EventTypeCountsSnapshot {
            schema_version: EventTypeCountsSnapshot::SCHEMA_VERSION,
            last_seq: self.last_seq,
            counts: self.counts.clone(),
        }
    }

    fn restore(snapshot: Self::Snapshot) -> Self {
        EventTypeCounts {
            counts: snapshot.counts,
            last_seq: snapshot.last_seq,
        }
    }
}
