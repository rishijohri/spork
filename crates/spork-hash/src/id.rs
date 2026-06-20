//! The full content address: [`ObjectId`].
//!
//! An [`ObjectId`] is the pair `(tag, hash)` — a self-describing
//! [`crate::HashTag`] plus the bare [`crate::Hash`] digest. This is *the
//! address* used throughout Spork's content-addressed store: it says both
//! "which object" (the digest) and "how it was hashed" (the tag), so the system
//! can refuse an object minted under a hashing scheme it does not understand
//! instead of misreading it.
//!
//! The canonical textual form is `"<algo>.<generation>:<hex>"`, e.g.
//! `b3.1:6437b3ac...`. Parsing this form is where the no-domino guarantee is
//! enforced: an unknown algorithm yields [`crate::HashError::UnknownAlgo`] and
//! an unknown generation yields [`crate::HashError::UnknownGeneration`].
//!
//! Design references: DESIGN.md §6.1, §16, Appendix A.7 constraint C-3.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::algo::{HashAlgo, HashTag};
use crate::error::HashError;
use crate::hash::Hash;

/// A complete, self-describing content address: a [`HashTag`] and a [`Hash`].
///
/// `ObjectId` is what callers store and exchange when an address must carry its
/// own provenance (algorithm + generation). The digest alone ([`Hash`]) is
/// enough for in-memory keying within a single generation; the `ObjectId` is
/// what gets persisted, displayed, and parsed at trust boundaries — which is
/// where generation rejection matters.
///
/// # Text and serde format
/// Renders and parses as `"<algo>.<generation>:<64-hex>"`. The serde
/// representation is the same string, keeping persisted addresses human-diffable
/// and portable across machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct ObjectId {
    /// The self-describing hash header (algorithm + generation).
    pub tag: HashTag,
    /// The BLAKE3 digest of the object's bytes.
    pub hash: Hash,
}

impl ObjectId {
    /// Construct an [`ObjectId`] from an explicit tag and digest.
    ///
    /// Most callers should instead use [`crate::object_id`], which stamps the
    /// current generation tag automatically. This constructor exists for
    /// reconstructing an address whose tag was read from storage.
    #[must_use]
    pub const fn new(tag: HashTag, hash: Hash) -> Self {
        ObjectId { tag, hash }
    }

    /// Whether this build understands this address's hashing scheme.
    ///
    /// Delegates to [`HashTag::is_supported`]. An address can be *well-formed*
    /// yet unsupported (e.g. a future generation); the textual parser rejects
    /// unsupported addresses, but a value deserialized through other paths can
    /// be checked here.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        self.tag.is_supported()
    }
}

impl fmt::Display for ObjectId {
    /// Renders as `"<algo>.<generation>:<hex>"`, e.g. `b3.1:6437b3ac...`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.tag, self.hash)
    }
}

impl FromStr for ObjectId {
    type Err = HashError;

    /// Parse `"<algo>.<generation>:<hex>"`, rejecting unknown algorithms and
    /// generations.
    ///
    /// # Errors
    /// - [`HashError::Malformed`] — the string is not shaped like
    ///   `tag:hex`, the generation is not a `u8`, or the hex digest is invalid.
    /// - [`HashError::UnknownAlgo`] — the algorithm label is unrecognized.
    /// - [`HashError::UnknownGeneration`] — the algorithm is known but the
    ///   `(algo, generation)` pair is not supported by this build. This is the
    ///   no-domino rejection (DESIGN.md A.7 C-3): an object from a newer hashing
    ///   scheme is refused, never reinterpreted.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split exactly once on ':' into the tag and the hex digest.
        let (tag_str, hex_str) = s.split_once(':').ok_or_else(|| {
            HashError::Malformed(format!("missing ':' separator in object id {s:?}"))
        })?;

        // The tag is "<algo>.<generation>".
        let (algo_str, gen_str) = tag_str.split_once('.').ok_or_else(|| {
            HashError::Malformed(format!(
                "missing '.' between algorithm and generation in tag {tag_str:?}"
            ))
        })?;

        if algo_str.is_empty() {
            return Err(HashError::Malformed("empty algorithm label".to_string()));
        }

        let algo = HashAlgo::from_label(algo_str)
            .ok_or_else(|| HashError::UnknownAlgo(algo_str.to_string()))?;

        let generation: u8 = gen_str
            .parse()
            .map_err(|_| HashError::Malformed(format!("invalid generation {gen_str:?}")))?;

        let tag = HashTag::new(algo, generation);

        // The crucial gate: refuse any tag this build does not support. We know
        // the algorithm parsed, so an unsupported pair is specifically an
        // unknown *generation*.
        if !tag.is_supported() {
            return Err(HashError::UnknownGeneration(generation));
        }

        // Parse the digest only after the tag is validated.
        let hash = Hash::from_hex(hex_str)?;

        Ok(ObjectId { tag, hash })
    }
}

impl From<ObjectId> for String {
    fn from(id: ObjectId) -> Self {
        id.to_string()
    }
}

impl TryFrom<String> for ObjectId {
    type Error = HashError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl TryFrom<&str> for ObjectId {
    type Error = HashError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hash() -> Hash {
        // The official BLAKE3 test vector for "abc".
        Hash::from_hex("6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85").unwrap()
    }

    #[test]
    fn display_uses_b3_1_prefix() {
        let id = ObjectId::new(HashTag::CURRENT, sample_hash());
        assert_eq!(
            id.to_string(),
            "b3.1:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

    #[test]
    fn display_from_str_round_trip() {
        let id = ObjectId::new(HashTag::CURRENT, sample_hash());
        let text = id.to_string();
        let back: ObjectId = text.parse().unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn unknown_generation_is_rejected() {
        let s = "b3.99:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";
        let err = s.parse::<ObjectId>().unwrap_err();
        assert_eq!(err, HashError::UnknownGeneration(99));
    }

    #[test]
    fn generation_zero_is_rejected() {
        let s = "b3.0:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";
        assert_eq!(
            s.parse::<ObjectId>().unwrap_err(),
            HashError::UnknownGeneration(0)
        );
    }

    #[test]
    fn unknown_algo_is_rejected() {
        let s = "sha256.1:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85";
        assert_eq!(
            s.parse::<ObjectId>().unwrap_err(),
            HashError::UnknownAlgo("sha256".to_string())
        );
    }

    #[test]
    fn malformed_inputs_are_rejected() {
        // No separator at all.
        assert!(matches!(
            "deadbeef".parse::<ObjectId>(),
            Err(HashError::Malformed(_))
        ));
        // Tag without a generation.
        assert!(matches!(
            "b3:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
                .parse::<ObjectId>(),
            Err(HashError::Malformed(_))
        ));
        // Empty algorithm label.
        assert!(matches!(
            ".1:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
                .parse::<ObjectId>(),
            Err(HashError::Malformed(_))
        ));
        // Non-numeric generation.
        assert!(matches!(
            "b3.x:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
                .parse::<ObjectId>(),
            Err(HashError::Malformed(_))
        ));
        // Generation overflows u8 -> not parseable as u8 -> Malformed.
        assert!(matches!(
            "b3.256:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
                .parse::<ObjectId>(),
            Err(HashError::Malformed(_))
        ));
        // Bad hex digest.
        assert!(matches!(
            "b3.1:zzzz".parse::<ObjectId>(),
            Err(HashError::Malformed(_))
        ));
    }

    #[test]
    fn unknown_generation_takes_precedence_over_bad_hex() {
        // The tag is checked before the digest, so a future-generation address
        // with a garbage digest still reports the generation problem — the
        // actionable, no-domino signal.
        let s = "b3.99:not-valid-hex";
        assert_eq!(
            s.parse::<ObjectId>().unwrap_err(),
            HashError::UnknownGeneration(99)
        );
    }

    #[test]
    fn serde_round_trips_as_string() {
        let id = ObjectId::new(HashTag::CURRENT, sample_hash());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        let back: ObjectId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn serde_rejects_unknown_generation() {
        let json = "\"b3.99:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85\"";
        let err = serde_json::from_str::<ObjectId>(json).unwrap_err();
        assert!(err.to_string().contains("generation"));
    }

    #[test]
    fn try_from_str_and_string() {
        let id = ObjectId::try_from(
            "b3.1:6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
        )
        .unwrap();
        assert_eq!(id.tag, HashTag::CURRENT);
        let id2 = ObjectId::try_from(id.to_string()).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn is_supported_reflects_tag() {
        let good = ObjectId::new(HashTag::CURRENT, sample_hash());
        assert!(good.is_supported());
        let bad = ObjectId::new(HashTag::new(HashAlgo::Blake3, 7), sample_hash());
        assert!(!bad.is_supported());
    }
}
