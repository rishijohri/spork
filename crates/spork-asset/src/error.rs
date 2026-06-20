//! The crate's error type, [`AssetError`].
//!
//! Every fallible [`AssetStore`](crate::AssetStore) operation funnels its
//! failures into [`AssetError`]. The variants are deliberately discriminable so
//! callers can react precisely. Two carry the load of the design's
//! deny-by-default policy:
//!
//! - [`AssetError::EcosystemNotRegistered`] is the *complete* deny-by-default
//!   behavior for the reconstructable-deps class (DESIGN.md §10.5). No ecosystem
//!   resolver (npm, pip, …) ships in F0, so every [`AssetKind::Deps`] key is
//!   refused with the offending ecosystem named. This is a finished capability,
//!   not a stub: a later phase adds resolvers *additively* behind the same
//!   [`AssetStore`](crate::AssetStore) trait, and until one is registered the
//!   answer is a deterministic, typed refusal.
//! - [`AssetError::ContentMismatch`] enforces the content-addressed contract of
//!   the opaque class: an `ensure` whose source does not hash to the key's
//!   declared `content_hash` is rejected rather than silently re-pointing the
//!   key at different bytes.
//!
//! Design references: DESIGN.md §10.5 (deps excluded by default, reconstructed
//! read-only from a content-addressed, platform-keyed cache), §4.10 (the
//! `AssetStore` trait is frozen in F0 with one v1 implementation; ecosystem
//! resolvers ship later), domino D-13.

use thiserror::Error;

use spork_hash::Hash;

/// Errors produced by an [`AssetStore`](crate::AssetStore).
///
/// The set spans the three failure surfaces of the asset layer: the
/// deny-by-default policy for unregistered ecosystems, content-address integrity
/// for the opaque class, and the I/O / CAS plumbing beneath both.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AssetError {
    /// A [`AssetKind::Deps`](crate::AssetKind::Deps) key named an ecosystem this
    /// build has no resolver for.
    ///
    /// This is the *complete* deny-by-default behavior of the reconstructable
    /// deps class (DESIGN.md §10.5): in F0 no ecosystem is registered, so every
    /// such key is refused with the offending ecosystem string. Resolvers
    /// (npm/pip/…) are added additively behind the same
    /// [`AssetStore`](crate::AssetStore) trait in a later phase; this is not a
    /// placeholder.
    #[error("no resolver is registered for ecosystem {0:?} (deps are reconstructed, not stored; deny-by-default)")]
    EcosystemNotRegistered(String),

    /// An `ensure` of an [`AssetKind::Opaque`](crate::AssetKind::Opaque) key was
    /// given a source whose content does not hash to the key's declared
    /// `content_hash`.
    ///
    /// The opaque class is content-addressed: the key *is* the identity of the
    /// bytes. Rather than silently re-point the key at different bytes, the store
    /// refuses the mismatch, naming the expected and actual content hashes.
    #[error(
        "content mismatch for opaque asset: key declares {expected} but source hashes to {actual}"
    )]
    ContentMismatch {
        /// The `content_hash` declared by the [`AssetKey`](crate::AssetKey).
        expected: Hash,
        /// The content hash the supplied source actually produced.
        actual: Hash,
    },

    /// `ensure` was called for an opaque key with no source, and the asset is not
    /// already present in the cache.
    ///
    /// An opaque asset cannot be conjured from its hash alone — it must have been
    /// ingested from a source at least once. This is raised when a first-time
    /// `ensure` omits the `source`.
    #[error("opaque asset {0} is not cached and no source was supplied to ingest it")]
    SourceRequired(Hash),

    /// The on-disk source path supplied to `ensure` was of an unsupported type.
    ///
    /// Opaque assets are ingested as a single regular file or a directory tree;
    /// anything else (a FIFO, socket, device node, or a dangling symlink at the
    /// root) cannot be content-addressed faithfully and is refused.
    #[error("unsupported source for opaque asset at {path:?}: {reason}")]
    UnsupportedSource {
        /// The offending source path.
        path: std::path::PathBuf,
        /// Why the path could not be ingested.
        reason: String,
    },

    /// A stored asset record could not be decoded.
    ///
    /// The asset index keeps a small self-describing record per ingested opaque
    /// asset; this variant covers a record whose bytes do not parse (e.g. a
    /// schema version this build does not understand, or on-disk corruption).
    #[error("failed to decode asset record: {0}")]
    Decode(String),

    /// An underlying I/O operation failed, tagged with the path it targeted.
    #[error("i/o error at {path:?}: {source}")]
    Io {
        /// The filesystem path the operation was targeting, if applicable.
        path: std::path::PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// An error surfaced from the content-addressed store beneath the asset
    /// store.
    ///
    /// Carries the [`spork_cas::CasError`] message so a CAS failure (a missing
    /// object, an unknown hash generation, a malformed header) is not lost in
    /// translation.
    #[error("content-addressed store error: {0}")]
    Cas(String),
}

impl AssetError {
    /// Build an [`AssetError::Io`] tagged with the path it occurred at.
    pub(crate) fn io(path: impl Into<std::path::PathBuf>, source: std::io::Error) -> Self {
        AssetError::Io {
            path: path.into(),
            source,
        }
    }

    /// Build an [`AssetError::UnsupportedSource`] for `path` with a reason.
    pub(crate) fn unsupported(
        path: impl Into<std::path::PathBuf>,
        reason: impl Into<String>,
    ) -> Self {
        AssetError::UnsupportedSource {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

impl From<spork_cas::CasError> for AssetError {
    fn from(e: spork_cas::CasError) -> Self {
        AssetError::Cas(e.to_string())
    }
}

/// Convenience result alias for the crate's fallible operations.
pub type Result<T> = std::result::Result<T, AssetError>;
