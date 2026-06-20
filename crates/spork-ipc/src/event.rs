//! The [`OpLogEvent`] durable event set — the ordered source of truth the
//! renderer reduces over.
//!
//! Durable state transitions ride the daemon's append-only, hash-chained op-log
//! (DESIGN.md §6.1, §14.4). [`OpLogEvent`] is the renderer-facing projection of
//! that log: each variant is a node/edge/ref transition, stamped with a
//! monotonic `seq` so the renderer can reduce them in order, detect gaps, and
//! drive optimistic-UI reconciliation against the `op_id` it got back from the
//! mutation (DESIGN.md §5.5, §14.4).
//!
//! This is the channel that carries the *result of a mutation* — never the
//! mutation's return value. [`Command::NodeCreate`](crate::Command::NodeCreate)
//! returns an `op_id`; the created node and its edges show up here as
//! [`OpLogEvent::NodeCreated`] + [`OpLogEvent::EdgeAdded`].
//!
//! # Additive forward-tolerance (CLAUDE.md C2/C5)
//!
//! `OpLogEvent` is **additive**: the durable log gains new event variants over
//! the product's life (a future `MergePerformed`, `ResultRecorded`, …). An older
//! reducer that meets a variant it does not recognize must **ignore it**, not
//! crash and not corrupt its fold — exactly the way the F1 log promises old
//! reducers tolerate unknown event types.
//!
//! To make that property a *type*, this module pairs the closed [`OpLogEvent`]
//! enum with [`MaybeEvent`], a forward-tolerant wrapper used on the *read* side.
//! Deserializing a stream element into [`MaybeEvent`] yields
//! [`MaybeEvent::Known`] for a recognized variant and
//! [`MaybeEvent::Unknown`] (capturing the raw `type`/`seq`) for anything else —
//! so an old reducer keeps ordering by `seq` and folds only what it understands.
//! The enum itself stays closed for exhaustive matching in the daemon; the
//! tolerance lives in the wrapper, which is where untrusted future bytes enter.
//!
//! Design references: DESIGN.md §5.5, §6.1, §14.1, §14.4, A.1.

use serde::{Deserialize, Serialize};
use spork_graph::EdgeType;
use ulid::Ulid;

/// The schema version of the [`OpLogEvent`] envelope. The contract evolves by
/// *appending* variants (see the module docs), so this version moves only with a
/// backward-compatible addition.
pub const OP_LOG_EVENT_SCHEMA_VERSION: u16 = 1;

/// A durable, ordered op-log event the renderer reduces over.
///
/// Internally tagged on a `"type"` field with `SCREAMING_SNAKE_CASE` variant
/// names (matching A.1's `NODE_CREATED`/`EDGE_ADDED`/… event names) and
/// `camelCase` fields. Every variant carries a `seq`: the monotonic position in
/// the ordered stream, used by the reducer to maintain order and detect gaps.
///
/// The enum is **closed** so the daemon matches it exhaustively. Read-side
/// forward-tolerance — ignoring unknown *future* variants — is provided by
/// [`MaybeEvent`], not by making this enum open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
#[serde(rename_all_fields = "camelCase")]
pub enum OpLogEvent {
    /// A node was created (A.1 `NODE_CREATED`). Emitted by
    /// [`Command::NodeCreate`](crate::Command::NodeCreate).
    NodeCreated {
        /// Ordered position in the stream.
        seq: u64,
        /// The created node's id.
        node_id: Ulid,
        /// The persisted-envelope schema version the node was written under, so
        /// the reducer can apply the right migration (DESIGN.md §7.2).
        schema_version: u16,
    },

    /// An edge was added between two nodes (A.1 `EDGE_ADDED`). Accompanies
    /// [`OpLogEvent::NodeCreated`] for each parent link.
    EdgeAdded {
        /// Ordered position in the stream.
        seq: u64,
        /// The source node.
        from: Ulid,
        /// The destination node.
        to: Ulid,
        /// The typed edge relationship (DESIGN.md §6.3).
        edge: EdgeType,
    },

    /// A ref was moved to a different node (A.1 `REF_MOVED`). Emitted by
    /// [`Command::RefMove`](crate::Command::RefMove) and as part of a restore.
    RefMoved {
        /// Ordered position in the stream.
        seq: u64,
        /// The ref name that moved.
        #[serde(rename = "ref")]
        ref_name: String,
        /// The node the ref now points at.
        to: Ulid,
    },

    /// A ref was created (A.1 `REF_CREATED`). Emitted by
    /// [`Command::RefCreate`](crate::Command::RefCreate) and the create half of
    /// a branch fork.
    RefCreated {
        /// Ordered position in the stream.
        seq: u64,
        /// The new ref name.
        #[serde(rename = "ref")]
        ref_name: String,
    },

    /// A branch was forked (A.1 `BRANCH_FORKED`). Emitted by
    /// [`Command::BranchFork`](crate::Command::BranchFork); metadata-only, no
    /// bytes copied (DESIGN.md §10.3).
    BranchForked {
        /// Ordered position in the stream.
        seq: u64,
        /// The new branch ref name.
        #[serde(rename = "ref")]
        ref_name: String,
    },

    /// A restore was performed (A.1 `RESTORE_PERFORMED`). Emitted by
    /// [`Command::NodeRestore`](crate::Command::NodeRestore). Restore is an
    /// event, not an overwrite — forward history survives (DESIGN.md §6.4,
    /// §10.3).
    RestorePerformed {
        /// Ordered position in the stream.
        seq: u64,
        /// The node that was restored.
        node_id: Ulid,
    },

    /// An operation was undone (A.1 `OP_UNDONE`). Emitted by
    /// [`Command::OpUndo`](crate::Command::OpUndo).
    OpUndone {
        /// Ordered position in the stream.
        seq: u64,
    },

    /// An operation was redone (A.1 `OP_REDONE`). Emitted by
    /// [`Command::OpRedo`](crate::Command::OpRedo).
    OpRedone {
        /// Ordered position in the stream.
        seq: u64,
    },

    /// Garbage collection ran (A.1 `GC_PERFORMED`). Emitted by a non-dry-run
    /// [`Command::GcRun`](crate::Command::GcRun).
    GcPerformed {
        /// Ordered position in the stream.
        seq: u64,
    },
}

impl OpLogEvent {
    /// The ordered position of this event in the durable stream.
    ///
    /// Every variant carries a `seq`; this accessor lets a reducer order and
    /// gap-check events without matching each variant.
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            OpLogEvent::NodeCreated { seq, .. }
            | OpLogEvent::EdgeAdded { seq, .. }
            | OpLogEvent::RefMoved { seq, .. }
            | OpLogEvent::RefCreated { seq, .. }
            | OpLogEvent::BranchForked { seq, .. }
            | OpLogEvent::RestorePerformed { seq, .. }
            | OpLogEvent::OpUndone { seq }
            | OpLogEvent::OpRedone { seq }
            | OpLogEvent::GcPerformed { seq } => *seq,
        }
    }

    /// The stable wire tag of this event (the `"type"` string), e.g.
    /// `"NODE_CREATED"`. Useful for logging and for tests that assert the
    /// frozen A.1 names.
    #[must_use]
    pub fn type_tag(&self) -> &'static str {
        match self {
            OpLogEvent::NodeCreated { .. } => "NODE_CREATED",
            OpLogEvent::EdgeAdded { .. } => "EDGE_ADDED",
            OpLogEvent::RefMoved { .. } => "REF_MOVED",
            OpLogEvent::RefCreated { .. } => "REF_CREATED",
            OpLogEvent::BranchForked { .. } => "BRANCH_FORKED",
            OpLogEvent::RestorePerformed { .. } => "RESTORE_PERFORMED",
            OpLogEvent::OpUndone { .. } => "OP_UNDONE",
            OpLogEvent::OpRedone { .. } => "OP_REDONE",
            OpLogEvent::GcPerformed { .. } => "GC_PERFORMED",
        }
    }
}

/// A forward-tolerant view of one element of the durable event stream.
///
/// This is the *read-side* type that makes the additive-evolution guarantee
/// concrete. Deserializing a stream element into `MaybeEvent`:
/// - yields [`MaybeEvent::Known`] for any variant this build of
///   [`OpLogEvent`] understands, and
/// - yields [`MaybeEvent::Unknown`] — capturing the raw `type` tag, the `seq`,
///   and the whole JSON body — for any *future* variant it does not.
///
/// An older reducer therefore never fails on a newer log: it keeps stream
/// ordering by [`MaybeEvent::seq`] and folds only the [`MaybeEvent::Known`]
/// events, ignoring [`MaybeEvent::Unknown`] exactly as the F1 log contract
/// promises (DESIGN.md §6.1, §14.4; CLAUDE.md C2/C5).
///
/// `Unknown` requires the future variant to still carry a `"type"` string and a
/// numeric `"seq"`, which is part of the frozen envelope shape — so ordering is
/// preserved even across variants this build has never seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaybeEvent {
    /// A variant this build recognizes, fully decoded.
    Known(OpLogEvent),
    /// A future variant this build does not recognize. Retained (not dropped)
    /// so the reducer can still order by `seq` and, if desired, forward the raw
    /// body to a newer consumer.
    Unknown {
        /// The unrecognized `"type"` tag from the wire.
        type_tag: String,
        /// The event's ordered position, parsed from the frozen `"seq"` field.
        seq: u64,
        /// The full raw JSON body of the event, preserved verbatim.
        raw: serde_json::Value,
    },
}

impl MaybeEvent {
    /// The ordered position of this stream element, whether it is a known or an
    /// unknown variant — so a reducer keeps global ordering across the boundary
    /// of what it understands.
    #[must_use]
    pub fn seq(&self) -> u64 {
        match self {
            MaybeEvent::Known(e) => e.seq(),
            MaybeEvent::Unknown { seq, .. } => *seq,
        }
    }

    /// The known event, if recognized; `None` for a future/unknown variant.
    #[must_use]
    pub fn known(&self) -> Option<&OpLogEvent> {
        match self {
            MaybeEvent::Known(e) => Some(e),
            MaybeEvent::Unknown { .. } => None,
        }
    }

    /// Whether this element is a future/unknown variant (one this build ignores).
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, MaybeEvent::Unknown { .. })
    }
}

impl<'de> Deserialize<'de> for MaybeEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Decode once into a generic value, then attempt the strict decode.
        // This is the forward-tolerant seam: a recognized tag decodes into the
        // closed enum; anything else is retained with its frozen `type`/`seq`.
        let raw = serde_json::Value::deserialize(deserializer)?;
        match serde_json::from_value::<OpLogEvent>(raw.clone()) {
            Ok(event) => Ok(MaybeEvent::Known(event)),
            Err(_) => {
                let type_tag = raw
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let seq = raw
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                Ok(MaybeEvent::Unknown { type_tag, seq, raw })
            }
        }
    }
}

impl Serialize for MaybeEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Round-trips both arms: a known event re-serializes to its canonical
        // wire form; an unknown one re-emits its preserved raw body verbatim, so
        // forwarding an unrecognized event to a newer consumer is lossless.
        match self {
            MaybeEvent::Known(e) => e.serialize(serializer),
            MaybeEvent::Unknown { raw, .. } => raw.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<OpLogEvent> {
        let n = Ulid::new();
        let m = Ulid::new();
        vec![
            OpLogEvent::NodeCreated {
                seq: 1,
                node_id: n,
                schema_version: 1,
            },
            OpLogEvent::EdgeAdded {
                seq: 2,
                from: n,
                to: m,
                edge: EdgeType::ParentChild,
            },
            OpLogEvent::RefMoved {
                seq: 3,
                ref_name: "main".into(),
                to: m,
            },
            OpLogEvent::RefCreated {
                seq: 4,
                ref_name: "experiment".into(),
            },
            OpLogEvent::BranchForked {
                seq: 5,
                ref_name: "experiment".into(),
            },
            OpLogEvent::RestorePerformed { seq: 6, node_id: n },
            OpLogEvent::OpUndone { seq: 7 },
            OpLogEvent::OpRedone { seq: 8 },
            OpLogEvent::GcPerformed { seq: 9 },
        ]
    }

    #[test]
    fn every_event_round_trips_through_json() {
        for e in samples() {
            let json = serde_json::to_string(&e).expect("serialize");
            let back: OpLogEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(e, back, "round-trip mismatch for {e:?}");
        }
    }

    #[test]
    fn wire_uses_frozen_a1_type_names_and_ref_field() {
        let e = OpLogEvent::RefMoved {
            seq: 3,
            ref_name: "main".into(),
            to: Ulid::new(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "REF_MOVED");
        // `ref` is a Rust keyword but is the frozen wire field name.
        assert_eq!(v["ref"], "main");
        assert_eq!(
            serde_json::to_value(OpLogEvent::NodeCreated {
                seq: 1,
                node_id: Ulid::new(),
                schema_version: 1
            })
            .unwrap()["type"],
            "NODE_CREATED"
        );
    }

    #[test]
    fn seq_accessor_returns_each_variants_seq() {
        for (i, e) in samples().into_iter().enumerate() {
            assert_eq!(e.seq(), (i + 1) as u64);
        }
    }

    #[test]
    fn type_tag_matches_serialized_type() {
        for e in samples() {
            let v = serde_json::to_value(&e).unwrap();
            assert_eq!(v["type"], e.type_tag());
        }
    }

    #[test]
    fn known_variant_decodes_as_maybe_event_known() {
        let e = OpLogEvent::NodeCreated {
            seq: 42,
            node_id: Ulid::new(),
            schema_version: 1,
        };
        let json = serde_json::to_string(&e).unwrap();
        let me: MaybeEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(me, MaybeEvent::Known(e));
        assert!(!me.is_unknown());
        assert_eq!(me.seq(), 42);
    }

    #[test]
    fn unknown_future_variant_is_forward_tolerated() {
        // Simulate a NEWER daemon emitting a variant this build has never seen.
        // An old reducer must NOT fail; it must retain ordering and skip it.
        let future = serde_json::json!({
            "type": "MERGE_PERFORMED",
            "seq": 7,
            "intoRef": "main",
            "nodeId": Ulid::new().to_string(),
            "futureField": {"nested": true}
        });
        let json = serde_json::to_string(&future).unwrap();

        // Strict decode into the closed enum fails (as it should)...
        assert!(serde_json::from_str::<OpLogEvent>(&json).is_err());

        // ...but the forward-tolerant wrapper succeeds and preserves order.
        let me: MaybeEvent = serde_json::from_str(&json).unwrap();
        assert!(me.is_unknown());
        assert_eq!(me.seq(), 7);
        assert_eq!(me.known(), None);
        match &me {
            MaybeEvent::Unknown { type_tag, raw, .. } => {
                assert_eq!(type_tag, "MERGE_PERFORMED");
                // The raw body is preserved verbatim for forwarding.
                assert_eq!(raw["futureField"]["nested"], true);
            }
            MaybeEvent::Known(_) => panic!("should be unknown"),
        }

        // And re-serializing forwards it losslessly to a newer consumer.
        let reser: serde_json::Value = serde_json::to_value(&me).unwrap();
        assert_eq!(reser, future);
    }

    #[test]
    fn mixed_stream_reduces_in_seq_order_ignoring_unknowns() {
        // An old reducer tailing a stream that interleaves known and future
        // events folds only the known ones, in order, with no gaps in its view.
        let stream = serde_json::json!([
            {"type":"NODE_CREATED","seq":1,"nodeId":Ulid::new().to_string(),"schemaVersion":1},
            {"type":"MERGE_PERFORMED","seq":2,"intoRef":"main"},
            {"type":"REF_MOVED","seq":3,"ref":"main","to":Ulid::new().to_string()},
            {"type":"SOME_FUTURE_EVENT","seq":4},
        ]);
        let events: Vec<MaybeEvent> = serde_json::from_value(stream).unwrap();

        // Ordering is preserved across the whole stream, knowns and unknowns.
        let seqs: Vec<u64> = events.iter().map(MaybeEvent::seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);

        // The reducer folds only what it understands.
        let known: Vec<&OpLogEvent> = events.iter().filter_map(MaybeEvent::known).collect();
        assert_eq!(known.len(), 2);
        assert_eq!(known[0].type_tag(), "NODE_CREATED");
        assert_eq!(known[1].type_tag(), "REF_MOVED");

        // Exactly the future events were skipped.
        assert_eq!(events.iter().filter(|e| e.is_unknown()).count(), 2);
    }
}
