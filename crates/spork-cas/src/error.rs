//! The crate's error type, [`CasError`].
//!
//! Every fallible operation in `spork-cas` — storing/reading an object, walking
//! a directory, parsing a self-describing object header — funnels its failures
//! into [`CasError`]. The variants are deliberately discriminable so callers can
//! react precisely; in particular [`CasError::UnknownGeneration`] is the
//! load-bearing *no-domino* signal (DESIGN.md §6.1, Appendix A.7 C-3): an object
//! whose stored header carries a hash generation this build does not understand
//! is *rejected on read*, never silently reinterpreted.
//!
//! Design references: DESIGN.md §6.1 (content identity), §10.1 (object model).

use thiserror::Error;

use spork_hash::Hash;

/// Errors produced by the content-addressed store.
///
/// The set spans the three failure surfaces of the crate: I/O against the
/// on-disk store, structural problems with a stored object's bytes (a bad header,
/// a digest mismatch, an unknown generation), and logical problems building or
/// reading the object graph (a missing object, a malformed payload, a path that
/// cannot be represented in a tree).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CasError {
    /// An underlying I/O operation failed.
    ///
    /// Carries the offending path (when known) alongside the source error so the
    /// failure is actionable (which file could not be read/written/fsynced).
    #[error("i/o error at {path:?}: {source}")]
    Io {
        /// The filesystem path the operation was targeting, if applicable.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A stored object's self-describing header was malformed.
    ///
    /// The header encodes `{magic, header_version, HashTag, ObjKind,
    /// payload_len}`; this variant covers a truncated header, a bad magic, an
    /// unknown header version, an unrecognized object kind, or a payload length
    /// that disagrees with the bytes on disk.
    #[error("malformed object header: {0}")]
    MalformedHeader(String),

    /// A stored object carried a hash generation this build does not support.
    ///
    /// This is the no-domino rejection (DESIGN.md A.7 C-3). An object written by
    /// a newer Spork (a future canonical encoding or hash family, signalled by a
    /// higher [`spork_hash::HashTag::generation`]) is refused rather than
    /// rehashed or reinterpreted. Carries the offending generation.
    #[error("object has unknown hash generation: {0}")]
    UnknownGeneration(u8),

    /// A stored object's hash generation's algorithm label is unknown.
    ///
    /// Distinct from [`CasError::UnknownGeneration`]: here the *algorithm* code
    /// in the header is one this build has never heard of.
    #[error("object has unknown hash algorithm code: {0}")]
    UnknownAlgo(u8),

    /// The bytes read back for a digest did not hash to that digest.
    ///
    /// A content-addressed store's core invariant is that an object's address is
    /// the BLAKE3 digest of its payload. A mismatch means on-disk corruption (or
    /// a programming error) and is surfaced rather than returned as if valid.
    #[error("integrity check failed: object addressed as {addressed} hashes to {actual}")]
    IntegrityMismatch {
        /// The digest the object was looked up / stored under.
        addressed: Hash,
        /// The digest the bytes actually produce.
        actual: Hash,
    },

    /// An object required to satisfy a read was not present in the store.
    ///
    /// Raised when reassembling a blob, reading a tree/snapshot, or materializing
    /// a tree references a child object that the backend cannot find.
    #[error("object not found: {0}")]
    NotFound(Hash),

    /// A stored object's payload could not be decoded into the expected type.
    ///
    /// For example, bytes stored as a `Tree` did not parse as canonical
    /// `Tree` JSON, or a blob's chunk list was empty (a blob always has at least
    /// one chunk).
    #[error("failed to decode object payload: {0}")]
    Decode(String),

    /// A value could not be reduced to canonical bytes for hashing.
    ///
    /// Wraps a [`spork_canon::CanonError`] message. With the v1 object schemas
    /// (integers, strings, and arrays only) this cannot occur, but the fallible
    /// surface is kept honest for future schema evolution.
    #[error("failed to canonicalize object: {0}")]
    Canon(String),

    /// A filesystem entry could not be represented in the tree object model.
    ///
    /// For example, a file name that is not valid UTF-8 (tree entry names are
    /// UTF-8 strings for canonical, machine-portable identity), or an entry whose
    /// type is neither a regular file, directory, nor symlink.
    #[error("unrepresentable filesystem entry: {0}")]
    UnrepresentableEntry(String),
}

impl CasError {
    /// Build a [`CasError::Io`] tagged with the path it occurred at.
    pub(crate) fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        CasError::Io {
            path: path.into(),
            source,
        }
    }
}

/// Convenience result alias for the crate's fallible operations.
pub type Result<T> = std::result::Result<T, CasError>;
