//! The self-describing hash header: [`HashAlgo`] and [`HashTag`].
//!
//! Every address Spork mints carries a tiny, explicit header describing *how*
//! the digest beneath it was produced. This is what makes the substrate
//! evolvable without a flag day: a new hash family or a revised canonical
//! encoding ships as a new [`HashAlgo`] variant and/or a higher
//! [`HashTag::generation`], and old objects keep their original tag forever.
//! Binaries that don't understand a tag reject it (see
//! [`crate::HashError::UnknownGeneration`]) rather than rehashing in place.
//!
//! Design references: DESIGN.md §6.1 (BLAKE3 is the sole computed hash),
//! Appendix A.7 constraint C-3 (the algorithm/generation tag is the no-domino
//! seam for future hash algorithms).

use serde::{Deserialize, Serialize};

/// The family of hash function used to compute a digest.
///
/// This enum is the extension point for hash algorithms. v1 ships exactly one
/// variant, [`HashAlgo::Blake3`], because BLAKE3 is Spork's sole computed hash
/// (DESIGN.md §6.1). A future algorithm is *added* as a new variant behind this
/// same type; existing addresses, which carry their algorithm explicitly, are
/// never affected.
///
/// `#[non_exhaustive]` is deliberately omitted: the variant set is a frozen v1
/// contract, and adding a variant later is an additive change that downstream
/// `match`es should be forced to consider (so the compiler flags the sites that
/// must handle the new algorithm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum HashAlgo {
    /// BLAKE3 — parallel, tree-hashable, collision-safe; Spork's only computed
    /// hash. Serialized in addresses as the short label `b3`.
    Blake3,
}

impl HashAlgo {
    /// The short, stable label used for this algorithm in textual addresses.
    ///
    /// This label is part of the frozen wire format (`b3.1:<hex>`); changing it
    /// would be a domino break, so it is fixed here once.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            HashAlgo::Blake3 => "b3",
        }
    }

    /// Parse an algorithm label back into a [`HashAlgo`].
    ///
    /// Returns `None` for any label this build does not recognize; the caller
    /// maps that to [`crate::HashError::UnknownAlgo`].
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "b3" => Some(HashAlgo::Blake3),
            _ => None,
        }
    }
}

/// The self-describing header attached to every Spork address.
///
/// A `HashTag` answers "which algorithm, which generation?" for the digest that
/// follows it. It is persisted/serialized (serde) and forms the first half of
/// an [`crate::ObjectId`]. The pair `(algo, generation)` is rendered textually
/// as `"<label>.<generation>"`, e.g. the current tag is `b3.1`.
///
/// The `generation` is the version counter on the *hashing scheme itself*. It
/// is bumped (never an in-place change) when the way a digest is computed must
/// evolve — a new canonical encoding, a different domain separation, or a new
/// hash family. Because every object stores its own tag, a generation bump is a
/// loud, additive event: new objects get the new generation, old objects keep
/// theirs, and a binary that doesn't know a generation refuses it.
///
/// Design references: DESIGN.md §6.1, §16, Appendix A.7 C-3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HashTag {
    /// The hash algorithm family. v1: always [`HashAlgo::Blake3`].
    pub algo: HashAlgo,
    /// The generation of the hashing scheme. v1: always `1`.
    pub generation: u8,
}

impl HashTag {
    /// The tag every freshly computed Spork object id carries in v1: BLAKE3,
    /// generation 1.
    ///
    /// This is the single source of truth for "what does a new object look
    /// like." New address construction (e.g. [`crate::object_id`]) stamps this
    /// constant; the parser accepts only tags it recognizes and rejects the
    /// rest.
    pub const CURRENT: HashTag = HashTag {
        algo: HashAlgo::Blake3,
        generation: 1,
    };

    /// Construct a tag from its parts without validation.
    ///
    /// This is a plain constructor; whether a tag is *supported* by this build
    /// is a separate question answered by [`HashTag::is_supported`] and enforced
    /// at parse time.
    #[must_use]
    pub const fn new(algo: HashAlgo, generation: u8) -> Self {
        HashTag { algo, generation }
    }

    /// Whether this build understands this exact `(algo, generation)` pair.
    ///
    /// v1 supports only [`HashTag::CURRENT`]. The textual parser uses this to
    /// turn an unsupported generation into [`crate::HashError::UnknownGeneration`].
    #[must_use]
    pub fn is_supported(self) -> bool {
        self == HashTag::CURRENT
    }
}

impl Default for HashTag {
    /// The default tag is [`HashTag::CURRENT`].
    fn default() -> Self {
        HashTag::CURRENT
    }
}

impl std::fmt::Display for HashTag {
    /// Renders as `"<algo-label>.<generation>"`, e.g. `b3.1`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.algo.label(), self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_is_blake3_generation_one() {
        assert_eq!(HashTag::CURRENT.algo, HashAlgo::Blake3);
        assert_eq!(HashTag::CURRENT.generation, 1);
        assert_eq!(HashTag::default(), HashTag::CURRENT);
    }

    #[test]
    fn algo_label_round_trips() {
        assert_eq!(HashAlgo::Blake3.label(), "b3");
        assert_eq!(HashAlgo::from_label("b3"), Some(HashAlgo::Blake3));
        assert_eq!(HashAlgo::from_label("sha256"), None);
        assert_eq!(HashAlgo::from_label(""), None);
    }

    #[test]
    fn tag_display() {
        assert_eq!(HashTag::CURRENT.to_string(), "b3.1");
        assert_eq!(HashTag::new(HashAlgo::Blake3, 99).to_string(), "b3.99");
    }

    #[test]
    fn supported_only_for_current() {
        assert!(HashTag::CURRENT.is_supported());
        assert!(!HashTag::new(HashAlgo::Blake3, 2).is_supported());
        assert!(!HashTag::new(HashAlgo::Blake3, 99).is_supported());
    }

    #[test]
    fn serde_round_trip() {
        let tag = HashTag::CURRENT;
        let json = serde_json::to_string(&tag).unwrap();
        let back: HashTag = serde_json::from_str(&json).unwrap();
        assert_eq!(tag, back);
    }
}
