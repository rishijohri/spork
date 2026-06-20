//! End-to-end and property tests for the public identity API of `spork-hash`.
//!
//! These exercise the crate exactly as a downstream consumer would (only the
//! public surface) and cover the frozen invariants from the F0 contract:
//! deterministic dedup, hex/address round-trips, and generation rejection
//! (DESIGN.md §6.1, Appendix A.7 C-3).

use proptest::prelude::*;
use spork_hash::{hash_bytes, object_id, Hash, HashAlgo, HashError, HashTag, ObjectId, HASH_LEN};

#[test]
fn known_vectors_via_public_api() {
    assert_eq!(
        hash_bytes(b"").to_hex(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(
        hash_bytes(b"abc").to_hex(),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
}

#[test]
fn current_tag_is_blake3_generation_one() {
    assert_eq!(HashTag::CURRENT.algo, HashAlgo::Blake3);
    assert_eq!(HashTag::CURRENT.generation, 1);
}

#[test]
fn object_id_display_is_b3_1_prefixed() {
    let id = object_id(b"abc");
    assert_eq!(
        id.to_string(),
        "b3.1:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
}

#[test]
fn generation_99_is_unknown_generation() {
    let s = "b3.99:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";
    assert_eq!(
        s.parse::<ObjectId>().unwrap_err(),
        HashError::UnknownGeneration(99)
    );
}

proptest! {
    /// Hashing the same bytes twice is always identical (dedup invariant).
    #[test]
    fn hash_is_deterministic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        prop_assert_eq!(hash_bytes(&bytes), hash_bytes(&bytes));
    }

    /// Distinct logical content keys distinctly in practice: a one-byte append
    /// changes the digest. (Not a collision proof — a behavioral sanity check.)
    #[test]
    fn append_changes_hash(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let mut extended = bytes.clone();
        extended.push(0xAB);
        prop_assert_ne!(hash_bytes(&bytes), hash_bytes(&extended));
    }

    /// Any 32 raw bytes round-trip through hex and through Display/FromStr.
    #[test]
    fn hash_hex_round_trip(raw in any::<[u8; HASH_LEN]>()) {
        let h = Hash::from_bytes(raw);
        let hex = h.to_hex();
        prop_assert_eq!(hex.len(), HASH_LEN * 2);
        prop_assert_eq!(Hash::from_hex(&hex).unwrap(), h);
        prop_assert_eq!(h.to_string().parse::<Hash>().unwrap(), h);
        prop_assert_eq!(*h.as_bytes(), raw);
    }

    /// Any object id built from current-generation content round-trips through
    /// its `b3.1:<hex>` textual form.
    #[test]
    fn object_id_text_round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let id = object_id(&bytes);
        let text = id.to_string();
        prop_assert!(text.starts_with("b3.1:"));
        prop_assert_eq!(text.parse::<ObjectId>().unwrap(), id);
    }

    /// Any object id round-trips through serde JSON as a string.
    #[test]
    fn object_id_serde_round_trip(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let id = object_id(&bytes);
        let json = serde_json::to_string(&id).unwrap();
        let back: ObjectId = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(id, back);
    }

    /// Every generation other than the supported one is rejected as an unknown
    /// generation, regardless of the (valid) digest. This is the no-domino gate.
    #[test]
    fn non_current_generation_always_rejected(
        gen in (0u8..=u8::MAX).prop_filter("exclude current", |g| *g != 1),
        raw in any::<[u8; HASH_LEN]>(),
    ) {
        let s = format!("b3.{}:{}", gen, hex::encode(raw));
        prop_assert_eq!(
            s.parse::<ObjectId>().unwrap_err(),
            HashError::UnknownGeneration(gen)
        );
    }
}
