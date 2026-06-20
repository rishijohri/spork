//! Spork BLAKE3 identity primitives — the byte-identity substrate's address layer.
//!
//! This crate defines *how Spork names bytes*. Every hash Spork computes — blob,
//! tree, snapshot, lineage, and the event-log `prev_event_hash` chain — is
//! BLAKE3, so the entire design rests on a single primitive (DESIGN.md §6.1,
//! §16). What this crate adds on top of the bare digest is a *self-describing*
//! address so the substrate can evolve without a flag day.
//!
//! # The three types
//! - [`Hash`] — a bare 32-byte BLAKE3 digest. Identical bytes always hash to an
//!   identical `Hash`; this is the dedup invariant the content-addressed store
//!   is built on.
//! - [`HashTag`] — the self-describing header `(algo, generation)`. The current
//!   tag is [`HashTag::CURRENT`] = BLAKE3, generation 1.
//! - [`ObjectId`] — the full address `(tag, hash)`, rendered as
//!   `"b3.1:<hex>"`. Parsing an address is where unsupported algorithms and
//!   generations are *rejected*.
//!
//! # The no-domino seam
//! BLAKE3 is the sole computed hash today, but the way a digest is computed
//! might one day need to change (a new canonical encoding, new domain
//! separation, or a new hash family). Because every object carries its own
//! [`HashTag`], such a change is *additive*: new objects get a higher
//! [`HashTag::generation`] (or a new [`HashAlgo`] variant) while old objects
//! keep theirs forever. A binary that meets an address it doesn't understand
//! returns [`HashError::UnknownGeneration`] (or [`HashError::UnknownAlgo`])
//! rather than rehashing or reinterpreting in place. This is constraint C-3 in
//! DESIGN.md Appendix A.7 ("BLAKE3 sole computed hash; generation tag is the
//! no-domino seam for future hash algorithms").
//!
//! # Example
//! ```
//! use spork_hash::{hash_bytes, object_id, ObjectId, HashTag};
//!
//! // Identical bytes dedup to the same digest.
//! assert_eq!(hash_bytes(b"hello"), hash_bytes(b"hello"));
//!
//! // A fresh address carries the current generation tag.
//! let id = object_id(b"hello");
//! assert_eq!(id.tag, HashTag::CURRENT);
//!
//! // Addresses round-trip through their textual form...
//! let text = id.to_string();
//! assert!(text.starts_with("b3.1:"));
//! assert_eq!(text.parse::<ObjectId>().unwrap(), id);
//!
//! // ...and an unknown generation is refused, not guessed at.
//! let future = "b3.99:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";
//! assert!(future.parse::<ObjectId>().is_err());
//! ```
//!
//! Design references: DESIGN.md §6.1 (content identity / two layers), §10.1
//! (object model), §16 (technology stack — BLAKE3), Appendix A.7 constraint C-3.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod algo;
mod error;
mod hash;
mod id;

pub use algo::{HashAlgo, HashTag};
pub use error::HashError;
pub use hash::{Hash, HASH_LEN};
pub use id::ObjectId;

/// Compute the BLAKE3 digest of `bytes`.
///
/// This is the single function through which Spork turns content into identity.
/// It is deterministic and machine-independent: the same bytes always yield the
/// same [`Hash`] (the dedup invariant of the content-addressed store).
///
/// # Example
/// ```
/// use spork_hash::hash_bytes;
/// // The official BLAKE3 test vector for "abc".
/// assert_eq!(
///     hash_bytes(b"abc").to_hex(),
///     "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
/// );
/// ```
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> Hash {
    let digest = blake3::hash(bytes);
    Hash::from_bytes(*digest.as_bytes())
}

/// Compute the full [`ObjectId`] address for `bytes` under the current tag.
///
/// Equivalent to `ObjectId { tag: HashTag::CURRENT, hash: hash_bytes(bytes) }`.
/// New objects are always stamped with [`HashTag::CURRENT`]; older generations
/// are produced only by reading existing storage, never minted fresh.
///
/// # Example
/// ```
/// use spork_hash::{object_id, HashTag, hash_bytes};
/// let id = object_id(b"abc");
/// assert_eq!(id.tag, HashTag::CURRENT);
/// assert_eq!(id.hash, hash_bytes(b"abc"));
/// ```
#[must_use]
pub fn object_id(bytes: &[u8]) -> ObjectId {
    ObjectId::new(HashTag::CURRENT, hash_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The official BLAKE3 test vector for the empty input.
    const EMPTY_VECTOR: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    /// The official BLAKE3 test vector for "abc".
    const ABC_VECTOR: &str = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";

    #[test]
    fn known_blake3_vectors() {
        assert_eq!(hash_bytes(b"").to_hex(), EMPTY_VECTOR);
        assert_eq!(hash_bytes(b"abc").to_hex(), ABC_VECTOR);
    }

    #[test]
    fn identical_bytes_produce_identical_hash() {
        // The dedup invariant: equal content => equal digest.
        let a = hash_bytes(b"the quick brown fox");
        let b = hash_bytes(b"the quick brown fox");
        assert_eq!(a, b);

        // ...and different content => different digest.
        let c = hash_bytes(b"the quick brown fox.");
        assert_ne!(a, c);
    }

    #[test]
    fn object_id_uses_current_tag_and_matches_hash_bytes() {
        let id = object_id(b"abc");
        assert_eq!(id.tag, HashTag::CURRENT);
        assert_eq!(id.hash, hash_bytes(b"abc"));
        assert_eq!(id.to_string(), format!("b3.1:{ABC_VECTOR}"));
    }

    #[test]
    fn object_id_text_round_trip() {
        let id = object_id(b"some content");
        let parsed: ObjectId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn parsing_generation_99_yields_unknown_generation() {
        let s = format!("b3.99:{ABC_VECTOR}");
        let err = s.parse::<ObjectId>().unwrap_err();
        assert_eq!(err, HashError::UnknownGeneration(99));
    }

    #[test]
    fn empty_input_is_addressable() {
        // Empty content is a legitimate object (e.g. an empty file's chunk).
        let id = object_id(b"");
        assert_eq!(id.to_string(), format!("b3.1:{EMPTY_VECTOR}"));
        assert_eq!(id.to_string().parse::<ObjectId>().unwrap(), id);
    }

    #[test]
    fn large_input_hashes_deterministically() {
        // Exercise BLAKE3's tree mode on multi-block input and confirm
        // determinism over a non-trivial size.
        let big: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(hash_bytes(&big), hash_bytes(&big));
    }
}
