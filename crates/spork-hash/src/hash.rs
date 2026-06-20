//! The raw digest type: [`Hash`].
//!
//! A [`Hash`] is exactly a 32-byte BLAKE3 digest with no algorithm/generation
//! header — that header lives in [`crate::HashTag`] and the two combine into an
//! [`crate::ObjectId`]. Keeping the bare digest as its own type lets the CAS
//! and event log key maps and reuse digests cheaply (it is `Copy`), while text
//! and serde representations are uniform lowercase hex.
//!
//! Design references: DESIGN.md §6.1, §16 (BLAKE3 is the sole computed hash).

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::HashError;

/// The number of bytes in a BLAKE3 digest as Spork uses it (256-bit output).
pub const HASH_LEN: usize = 32;

/// A BLAKE3 digest: 32 raw bytes.
///
/// `Hash` is the content-addressing primitive. Identical input bytes always
/// produce an identical `Hash` (this is the dedup invariant the whole CAS rests
/// on). It is `Copy` and cheap to pass around, hash into maps, and compare.
///
/// # Representations
/// - **Display / [`FromStr`]**: 64 lowercase hex characters.
/// - **serde**: a hex string (so JSON snapshots are human-diffable and
///   machine-portable).
/// - **bytes**: [`Hash::from_bytes`] / [`Hash::as_bytes`] for the raw 32 bytes.
///
/// Note that a `Hash` carries no algorithm tag; pair it with a
/// [`crate::HashTag`] (via [`crate::ObjectId`]) when the algorithm/generation
/// must travel with the digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash([u8; HASH_LEN]);

impl Hash {
    /// Wrap 32 raw digest bytes as a [`Hash`].
    #[must_use]
    pub const fn from_bytes(bytes: [u8; HASH_LEN]) -> Self {
        Hash(bytes)
    }

    /// Borrow the raw 32 digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HASH_LEN] {
        &self.0
    }

    /// Consume the [`Hash`], returning the owned 32 digest bytes.
    #[must_use]
    pub const fn into_bytes(self) -> [u8; HASH_LEN] {
        self.0
    }

    /// Render the digest as a 64-character lowercase hex [`String`].
    ///
    /// Equivalent to the [`Display`](fmt::Display) representation; provided as a
    /// convenience for call sites that want an owned string directly.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a 64-character lowercase hex string into a [`Hash`].
    ///
    /// # Errors
    /// Returns [`HashError::Malformed`] if the input is not exactly 32 bytes of
    /// hexadecimal, or if it contains uppercase hex digits (the canonical form
    /// is lowercase; rejecting uppercase keeps a digest's text form unique).
    pub fn from_hex(s: &str) -> Result<Self, HashError> {
        if s.len() != HASH_LEN * 2 {
            return Err(HashError::Malformed(format!(
                "expected {} hex chars, got {}",
                HASH_LEN * 2,
                s.len()
            )));
        }
        // Enforce lowercase so the textual form of a digest is canonical and a
        // round-trip is byte-stable.
        if s.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(HashError::Malformed(
                "hex digest must be lowercase".to_string(),
            ));
        }
        let mut out = [0u8; HASH_LEN];
        hex::decode_to_slice(s, &mut out)
            .map_err(|e| HashError::Malformed(format!("invalid hex: {e}")))?;
        Ok(Hash(out))
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash {
    /// Debug prints `Hash(<hex>)` so logs are readable without dumping a byte
    /// array.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.to_hex())
    }
}

impl FromStr for Hash {
    type Err = HashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Hash::from_hex(s)
    }
}

impl From<[u8; HASH_LEN]> for Hash {
    fn from(bytes: [u8; HASH_LEN]) -> Self {
        Hash(bytes)
    }
}

impl AsRef<[u8]> for Hash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Serialize for Hash {
    /// Serializes as a lowercase hex string.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    /// Deserializes from a lowercase hex string.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HexVisitor;

        impl Visitor<'_> for HexVisitor {
            type Value = Hash;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a 64-character lowercase hex BLAKE3 digest")
            }

            fn visit_str<E>(self, v: &str) -> Result<Hash, E>
            where
                E: de::Error,
            {
                Hash::from_hex(v).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(HexVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Hash {
        let mut b = [0u8; HASH_LEN];
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = i as u8;
        }
        Hash::from_bytes(b)
    }

    #[test]
    fn hex_round_trip() {
        let h = sample();
        let s = h.to_hex();
        assert_eq!(s.len(), 64);
        let back = Hash::from_hex(&s).unwrap();
        assert_eq!(h, back);
        // Display and to_hex agree, and FromStr matches Display.
        assert_eq!(h.to_string(), s);
        assert_eq!(s.parse::<Hash>().unwrap(), h);
    }

    #[test]
    fn bytes_round_trip() {
        let h = sample();
        let bytes = *h.as_bytes();
        assert_eq!(Hash::from_bytes(bytes), h);
        assert_eq!(h.into_bytes(), bytes);
        assert_eq!(<Hash as From<[u8; 32]>>::from(bytes), h);
        assert_eq!(h.as_ref(), &bytes[..]);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(matches!(
            Hash::from_hex("abcd"),
            Err(HashError::Malformed(_))
        ));
        assert!(matches!(
            Hash::from_hex(&"a".repeat(63)),
            Err(HashError::Malformed(_))
        ));
        assert!(matches!(
            Hash::from_hex(&"a".repeat(65)),
            Err(HashError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_uppercase_and_non_hex() {
        let upper = "A".repeat(64);
        assert!(matches!(
            Hash::from_hex(&upper),
            Err(HashError::Malformed(_))
        ));
        let nonhex = "z".repeat(64);
        assert!(matches!(
            Hash::from_hex(&nonhex),
            Err(HashError::Malformed(_))
        ));
    }

    #[test]
    fn debug_is_readable() {
        let h = Hash::from_bytes([0u8; 32]);
        assert_eq!(format!("{h:?}"), format!("Hash({})", "0".repeat(64)));
    }

    #[test]
    fn serde_round_trips_as_hex_string() {
        let h = sample();
        let json = serde_json::to_string(&h).unwrap();
        // Serialized form is a quoted hex string, not a byte array.
        assert_eq!(json, format!("\"{}\"", h.to_hex()));
        let back: Hash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }

    #[test]
    fn serde_rejects_malformed_hex() {
        let bad = "\"not-a-hash\"";
        assert!(serde_json::from_str::<Hash>(bad).is_err());
    }

    #[test]
    fn ordering_is_lexicographic_over_bytes() {
        let a = Hash::from_bytes([0u8; 32]);
        let mut bb = [0u8; 32];
        bb[0] = 1;
        let b = Hash::from_bytes(bb);
        assert!(a < b);
    }
}
