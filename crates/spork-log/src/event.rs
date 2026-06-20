//! The persisted event envelope ([`Event`]), the caller-supplied input
//! ([`NewEvent`]), and the **FROZEN** hash-chain preimage.
//!
//! See DESIGN.md §6.1 ("Timeline layer — append-only log + materialized graph")
//! and A.1 ("Event-log entry ... `this_event_hash = H(prev ‖ canonical(payload)
//! ‖ seq)`"). The contract here refines A.1's formula by pinning the *exact*
//! preimage byte layout (below), which the daemon must reproduce identically on
//! every machine and every future build.

use serde::{Deserialize, Serialize};
use spork_hash::{Hash, HASH_LEN};
use ulid::Ulid;

use crate::error::{LogError, Result};

/// The genesis predecessor hash: the all-zero [`Hash`].
///
/// The first event in any log (`seq == 1`) chains from this sentinel, exactly as
/// Git's first commit has the all-zero parent. It is a fixed, well-known value so
/// `verify_chain` can validate the very first link without special-casing it.
pub const GENESIS_PREV_HASH: Hash = Hash::from_bytes([0u8; HASH_LEN]);

/// A persisted, hash-chained log event — the system's atomic unit of truth.
///
/// Every mutation to a Spork project (node created, edge added, snapshot
/// attached, restore performed, …) is one immutable `Event`, assigned a
/// contiguous [`Event::seq`] and linked to its predecessor through the BLAKE3
/// hash chain. Stored events are **never edited**; schema growth happens on read
/// via the migration registry (CLAUDE.md C5, see [`crate::Reader`]).
///
/// Fields are assigned by the log on append, except those a caller controls via
/// [`NewEvent`] (`event_type`, `schema_version`, `payload`, `actor`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// A time-sortable unique id (ULID), assigned by the log at append time.
    pub event_id: Ulid,
    /// The contiguous sequence number, starting at 1 with no gaps.
    pub seq: u64,
    /// The event type discriminator (e.g. `"node.created"`).
    pub event_type: String,
    /// The schema version of `payload` *as stored*. A reader may upgrade an
    /// older payload to the current version on read; this field always reflects
    /// the version the bytes were written under.
    pub schema_version: u16,
    /// The event's body. Persisted as canonical bytes (see `spork-canon`); the
    /// in-memory form is a [`serde_json::Value`].
    pub payload: serde_json::Value,
    /// The `this_event_hash` of the predecessor event, or [`GENESIS_PREV_HASH`]
    /// for `seq == 1`.
    pub prev_event_hash: Hash,
    /// This event's chain hash, computed over the frozen preimage (see
    /// [`compute_event_hash`]).
    pub this_event_hash: Hash,
    /// Who/what produced the event (an actor identifier).
    pub actor: String,
}

/// A caller-supplied event, before the log assigns identity and chains it.
///
/// The caller controls only the semantic fields. The log assigns `event_id` (a
/// fresh ULID), `seq` (`last + 1`), `prev_event_hash` (the previous event's
/// `this_event_hash`, or [`GENESIS_PREV_HASH`]), and computes `this_event_hash`
/// — all inside the single serializing writer so concurrent appends are
/// linearized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewEvent {
    /// The event type discriminator.
    pub event_type: String,
    /// The schema version the payload is written under.
    pub schema_version: u16,
    /// The event body.
    pub payload: serde_json::Value,
    /// Who/what is producing the event.
    pub actor: String,
}

impl NewEvent {
    /// Construct a [`NewEvent`] from its parts.
    ///
    /// A small convenience so call sites need not name every field.
    pub fn new(
        event_type: impl Into<String>,
        schema_version: u16,
        payload: serde_json::Value,
        actor: impl Into<String>,
    ) -> Self {
        NewEvent {
            event_type: event_type.into(),
            schema_version,
            payload,
            actor: actor.into(),
        }
    }
}

/// Compute the **FROZEN** `this_event_hash` for an event.
///
/// # Frozen preimage (do not change in place — CLAUDE.md C2)
///
/// `this_event_hash` is the BLAKE3 digest of the concatenation, in this exact
/// order, of:
///
/// 1. `prev_event_hash.as_bytes()` — the 32 raw bytes of the predecessor hash
///    (or [`GENESIS_PREV_HASH`] for `seq == 1`).
/// 2. `canonicalize_value(payload)` — the canonical UTF-8 bytes of the payload
///    (`spork-canon`, the same encoder the whole substrate hashes through).
/// 3. `seq.to_le_bytes()` — the 8-byte little-endian sequence number.
/// 4. `event_type.as_bytes()` — the UTF-8 bytes of the event type.
/// 5. `schema_version.to_le_bytes()` — the 2-byte little-endian schema version.
///
/// There are **no length prefixes or separators** between fields; the layout is
/// frozen exactly as written here. A change to this computation is a new
/// *generation* (a new function and a re-keyed chain), never an in-place rehash
/// of stored events — the whole point of the hash chain is that the stored
/// bytes are immutable evidence.
///
/// The canonical encoding of `payload` is what makes the hash machine- and
/// toolchain-independent: a payload built in any field order hashes identically
/// (see `spork-canon`'s frozen rules).
///
/// # Errors
///
/// Returns [`LogError::Canon`] if `payload` cannot be canonicalized (for
/// example, it contains a floating-point number, which is forbidden in
/// identity-bearing data).
pub fn compute_event_hash(
    prev_event_hash: &Hash,
    payload: &serde_json::Value,
    seq: u64,
    event_type: &str,
    schema_version: u16,
) -> Result<Hash> {
    let payload_canon = spork_canon::canonicalize_value(payload)?;

    // A single BLAKE3 hasher fed the preimage in the frozen order. Using the
    // streaming hasher avoids allocating one big concatenated buffer while
    // producing exactly the same digest as hashing the concatenation would.
    let mut hasher = blake3::Hasher::new();
    hasher.update(prev_event_hash.as_bytes()); // (1) 32 bytes
    hasher.update(&payload_canon); // (2) canonical payload
    hasher.update(&seq.to_le_bytes()); // (3) 8 bytes LE
    hasher.update(event_type.as_bytes()); // (4) UTF-8 type
    hasher.update(&schema_version.to_le_bytes()); // (5) 2 bytes LE

    Ok(Hash::from_bytes(*hasher.finalize().as_bytes()))
}

impl Event {
    /// Recompute this event's chain hash from its own fields.
    ///
    /// Used by [`crate::Reader::verify_chain`] to detect tamper: if the stored
    /// `this_event_hash` differs from this recomputation, the payload or framing
    /// fields were altered after the fact.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Canon`] if the payload cannot be canonicalized.
    pub fn recompute_hash(&self) -> Result<Hash> {
        compute_event_hash(
            &self.prev_event_hash,
            &self.payload,
            self.seq,
            &self.event_type,
            self.schema_version,
        )
    }

    /// Verify this event's `this_event_hash` is consistent with its contents.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::HashChainBroken`] at this event's `seq` if the stored
    /// hash does not match the recomputation, or [`LogError::Canon`] if the
    /// payload cannot be canonicalized.
    pub fn verify_self(&self) -> Result<()> {
        if self.recompute_hash()? != self.this_event_hash {
            return Err(LogError::HashChainBroken { seq: self.seq });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn genesis_prev_hash_is_all_zero() {
        assert_eq!(GENESIS_PREV_HASH.as_bytes(), &[0u8; HASH_LEN]);
    }

    #[test]
    fn hash_is_deterministic_and_order_independent_in_payload() {
        // Payload key order must not change the hash (canonicalization).
        let a = json!({ "b": 1, "a": 2 });
        let b = json!({ "a": 2, "b": 1 });
        let ha = compute_event_hash(&GENESIS_PREV_HASH, &a, 1, "t", 1).unwrap();
        let hb = compute_event_hash(&GENESIS_PREV_HASH, &b, 1, "t", 1).unwrap();
        assert_eq!(ha, hb);
    }

    #[test]
    fn hash_changes_with_each_field() {
        let base = compute_event_hash(&GENESIS_PREV_HASH, &json!({"x":1}), 1, "t", 1).unwrap();
        // Different payload.
        assert_ne!(
            base,
            compute_event_hash(&GENESIS_PREV_HASH, &json!({"x":2}), 1, "t", 1).unwrap()
        );
        // Different seq.
        assert_ne!(
            base,
            compute_event_hash(&GENESIS_PREV_HASH, &json!({"x":1}), 2, "t", 1).unwrap()
        );
        // Different type.
        assert_ne!(
            base,
            compute_event_hash(&GENESIS_PREV_HASH, &json!({"x":1}), 1, "u", 1).unwrap()
        );
        // Different schema version.
        assert_ne!(
            base,
            compute_event_hash(&GENESIS_PREV_HASH, &json!({"x":1}), 1, "t", 2).unwrap()
        );
        // Different prev hash.
        let other_prev = Hash::from_bytes([7u8; HASH_LEN]);
        assert_ne!(
            base,
            compute_event_hash(&other_prev, &json!({"x":1}), 1, "t", 1).unwrap()
        );
    }

    #[test]
    fn frozen_preimage_golden_vector() {
        // This pins the exact byte layout of the frozen preimage. If this value
        // ever changes, the hash chain has changed generation — a deliberate,
        // loud event, never a silent edit (CLAUDE.md C2).
        let h = compute_event_hash(
            &GENESIS_PREV_HASH,
            &json!({ "hello": "world", "n": 42 }),
            1,
            "node.created",
            1,
        )
        .unwrap();

        // Recompute the expected digest the long way (concatenate the preimage)
        // so the test is self-checking against the documented layout.
        let payload_canon =
            spork_canon::canonicalize_value(&json!({ "hello": "world", "n": 42 })).unwrap();
        let mut preimage = Vec::new();
        preimage.extend_from_slice(GENESIS_PREV_HASH.as_bytes());
        preimage.extend_from_slice(&payload_canon);
        preimage.extend_from_slice(&1u64.to_le_bytes());
        preimage.extend_from_slice(b"node.created");
        preimage.extend_from_slice(&1u16.to_le_bytes());
        let expected = spork_hash::hash_bytes(&preimage);

        assert_eq!(h, expected);
        // And a literal hex pin so a reordering of the update() calls is caught
        // even if both sides (the function and the long-way recomputation above)
        // were changed together. This hard-coded digest is the true golden
        // vector: it must be reproduced byte-for-byte on every machine and every
        // future build, and any change to it is a deliberate chain-generation
        // bump (CLAUDE.md C2), never a silent edit.
        const FROZEN_DIGEST_HEX: &str =
            "774a94e7dea8598df77364b3667ef2d113750854db75ab129f198eaadeaf9981";
        assert_eq!(h.to_hex(), FROZEN_DIGEST_HEX);
        assert_eq!(expected.to_hex(), FROZEN_DIGEST_HEX);
    }

    #[test]
    fn float_payload_is_rejected() {
        let err =
            compute_event_hash(&GENESIS_PREV_HASH, &json!({ "x": 1.5 }), 1, "t", 1).unwrap_err();
        assert!(matches!(err, LogError::Canon(_)));
    }

    #[test]
    fn verify_self_detects_tamper() {
        let payload = json!({ "ok": true });
        let h = compute_event_hash(&GENESIS_PREV_HASH, &payload, 1, "t", 1).unwrap();
        let mut ev = Event {
            event_id: Ulid::new(),
            seq: 1,
            event_type: "t".into(),
            schema_version: 1,
            payload,
            prev_event_hash: GENESIS_PREV_HASH,
            this_event_hash: h,
            actor: "test".into(),
        };
        ev.verify_self().unwrap();

        // Tamper with the payload but keep the stored hash: detected.
        ev.payload = json!({ "ok": false });
        assert_eq!(
            ev.verify_self().unwrap_err(),
            LogError::HashChainBroken { seq: 1 }
        );
    }

    #[test]
    fn new_event_constructor() {
        let ne = NewEvent::new("t", 3, json!({"a":1}), "actor");
        assert_eq!(ne.event_type, "t");
        assert_eq!(ne.schema_version, 3);
        assert_eq!(ne.actor, "actor");
    }
}
