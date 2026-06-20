//! The self-describing on-disk object header.
//!
//! Every object the store persists — loose or packed — is prefixed with a small,
//! fixed-layout header that describes the bytes that follow *without consulting
//! anything else*. This is what lets the store reject an object minted under a
//! hash generation it does not understand (DESIGN.md §6.1, Appendix A.7 C-3): the
//! generation travels *with* the bytes, so a read can refuse it before
//! interpreting the payload, never silently rehashing or reinterpreting in place.
//!
//! # Frozen layout (v1)
//!
//! ```text
//! offset size field
//! 0      4    magic            = b"SPK0"
//! 4      1    header_version   = 1
//! 5      1    algo_code        (HashAlgo: Blake3 = 1)
//! 6      1    generation       (HashTag generation, e.g. 1)
//! 7      1    kind_code        (ObjKind: Chunk=1, Blob=2, Tree=3, Snapshot=4)
//! 8      8    payload_len      (u64, big-endian)
//! 16     ..   payload          (payload_len bytes)
//! ```
//!
//! The header length is therefore a constant [`HEADER_LEN`] = 16 bytes. The
//! `payload_len` is redundant with the framing in loose files (the file is
//! header + payload) but is *load-bearing* in packfiles, where many objects share
//! one file and each must be self-delimiting. Recording it in both keeps a single
//! header format across storage backends.
//!
//! Design references: DESIGN.md §6.1 (content identity / self-describing
//! addresses), §10.1 (object model), Appendix A.7 C-3 (the generation tag is the
//! no-domino seam).

use spork_hash::{HashAlgo, HashTag};

use crate::error::{CasError, Result};
use crate::object::ObjKind;

/// The 4-byte magic that opens every stored object. Spelled "SPK0".
pub(crate) const MAGIC: [u8; 4] = *b"SPK0";

/// The current header layout version.
pub(crate) const HEADER_VERSION: u8 = 1;

/// The fixed size, in bytes, of a v1 object header.
pub(crate) const HEADER_LEN: usize = 16;

/// The on-disk algorithm code for [`HashAlgo::Blake3`].
const ALGO_BLAKE3: u8 = 1;

/// A decoded object header: the hash tag, the kind, and the payload length.
///
/// Constructed either freshly (when writing — [`ObjectHeader::new`]) or by
/// parsing the leading [`HEADER_LEN`] bytes of a stored object
/// ([`ObjectHeader::parse`]). Parsing is where unknown generations/algorithms are
/// rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjectHeader {
    /// The hash algorithm + generation the object was written under.
    pub tag: HashTag,
    /// The kind of object that follows.
    pub kind: ObjKind,
    /// The length of the payload following the header, in bytes.
    pub payload_len: u64,
}

impl ObjectHeader {
    /// Build a header for a freshly written object.
    #[must_use]
    pub fn new(tag: HashTag, kind: ObjKind, payload_len: u64) -> Self {
        ObjectHeader {
            tag,
            kind,
            payload_len,
        }
    }

    /// Map a [`HashAlgo`] to its on-disk code.
    const fn algo_code(algo: HashAlgo) -> u8 {
        match algo {
            HashAlgo::Blake3 => ALGO_BLAKE3,
        }
    }

    /// Map an on-disk algorithm code back to a [`HashAlgo`].
    fn algo_from_code(code: u8) -> Option<HashAlgo> {
        match code {
            ALGO_BLAKE3 => Some(HashAlgo::Blake3),
            _ => None,
        }
    }

    /// Serialize this header into its fixed [`HEADER_LEN`]-byte form.
    #[must_use]
    pub fn to_bytes(self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = HEADER_VERSION;
        out[5] = Self::algo_code(self.tag.algo);
        out[6] = self.tag.generation;
        out[7] = self.kind.code();
        out[8..16].copy_from_slice(&self.payload_len.to_be_bytes());
        out
    }

    /// Prepend this header to `payload`, returning the full stored object bytes.
    #[must_use]
    pub fn frame(self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
        out.extend_from_slice(&self.to_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Parse a header from the leading bytes of `buf`.
    ///
    /// Validates the magic and header version, then the algorithm and generation.
    /// This is the no-domino gate: an unknown algorithm yields
    /// [`CasError::UnknownAlgo`] and an unknown *generation* yields
    /// [`CasError::UnknownGeneration`] — the object is refused, never reread under
    /// a different scheme. Does **not** consult `payload_len` against the buffer
    /// length; callers that have the full object validate that separately.
    ///
    /// # Errors
    /// - [`CasError::MalformedHeader`] — buffer too short, bad magic, unknown
    ///   header version, or unrecognized object kind.
    /// - [`CasError::UnknownAlgo`] — unrecognized algorithm code.
    /// - [`CasError::UnknownGeneration`] — known algorithm, unsupported
    ///   generation.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN {
            return Err(CasError::MalformedHeader(format!(
                "object is shorter than the {HEADER_LEN}-byte header (got {} bytes)",
                buf.len()
            )));
        }
        if buf[0..4] != MAGIC {
            return Err(CasError::MalformedHeader(format!(
                "bad magic: expected {MAGIC:?}, got {:?}",
                &buf[0..4]
            )));
        }
        let header_version = buf[4];
        if header_version != HEADER_VERSION {
            return Err(CasError::MalformedHeader(format!(
                "unsupported header version {header_version} (this build writes v{HEADER_VERSION})"
            )));
        }

        let algo_code = buf[5];
        let algo = Self::algo_from_code(algo_code).ok_or(CasError::UnknownAlgo(algo_code))?;
        let generation = buf[6];
        let tag = HashTag::new(algo, generation);

        // The crucial gate: refuse any tag this build does not support.
        if !tag.is_supported() {
            return Err(CasError::UnknownGeneration(generation));
        }

        let kind_code = buf[7];
        let kind = ObjKind::from_code(kind_code).ok_or_else(|| {
            CasError::MalformedHeader(format!("unrecognized object kind code {kind_code}"))
        })?;

        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&buf[8..16]);
        let payload_len = u64::from_be_bytes(len_bytes);

        Ok(ObjectHeader {
            tag,
            kind,
            payload_len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let h = ObjectHeader::new(HashTag::CURRENT, ObjKind::Blob, 1234);
        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), HEADER_LEN);
        let parsed = ObjectHeader::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
    }

    #[test]
    fn frame_prepends_header() {
        let h = ObjectHeader::new(HashTag::CURRENT, ObjKind::Chunk, 3);
        let framed = h.frame(b"abc");
        assert_eq!(framed.len(), HEADER_LEN + 3);
        assert_eq!(&framed[HEADER_LEN..], b"abc");
        let parsed = ObjectHeader::parse(&framed).unwrap();
        assert_eq!(parsed.payload_len, 3);
        assert_eq!(parsed.kind, ObjKind::Chunk);
    }

    #[test]
    fn parse_rejects_short_buffer() {
        assert!(matches!(
            ObjectHeader::parse(&[0u8; 4]),
            Err(CasError::MalformedHeader(_))
        ));
    }

    #[test]
    fn parse_rejects_bad_magic() {
        let mut bytes = ObjectHeader::new(HashTag::CURRENT, ObjKind::Tree, 0).to_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            ObjectHeader::parse(&bytes),
            Err(CasError::MalformedHeader(_))
        ));
    }

    #[test]
    fn parse_rejects_unknown_header_version() {
        let mut bytes = ObjectHeader::new(HashTag::CURRENT, ObjKind::Tree, 0).to_bytes();
        bytes[4] = 99;
        assert!(matches!(
            ObjectHeader::parse(&bytes),
            Err(CasError::MalformedHeader(_))
        ));
    }

    #[test]
    fn parse_rejects_unknown_generation() {
        // Build a header by hand with a future generation.
        let mut bytes = ObjectHeader::new(HashTag::CURRENT, ObjKind::Blob, 0).to_bytes();
        bytes[6] = 99; // generation
        let err = ObjectHeader::parse(&bytes).unwrap_err();
        assert!(matches!(err, CasError::UnknownGeneration(99)));
    }

    #[test]
    fn parse_rejects_unknown_algo() {
        let mut bytes = ObjectHeader::new(HashTag::CURRENT, ObjKind::Blob, 0).to_bytes();
        bytes[5] = 7; // algo code
        let err = ObjectHeader::parse(&bytes).unwrap_err();
        assert!(matches!(err, CasError::UnknownAlgo(7)));
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        let mut bytes = ObjectHeader::new(HashTag::CURRENT, ObjKind::Blob, 0).to_bytes();
        bytes[7] = 0; // kind code
        assert!(matches!(
            ObjectHeader::parse(&bytes),
            Err(CasError::MalformedHeader(_))
        ));
    }
}
