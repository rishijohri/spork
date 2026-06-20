//! The frozen asset seam: the [`AssetStore`] trait.
//!
//! This trait is the *one place* the rest of Spork talks to the asset cache. It
//! is deliberately small — ensure an asset is present, materialize it read-only
//! into a sandbox, and garbage-collect what no live key references — so that new
//! asset backends (a remote cache, an ecosystem-specific resolver) are added
//! behind the same interface without touching any caller (constraint C3). v1
//! ships exactly one implementation, [`LocalCasAssetStore`](crate::LocalCasAssetStore),
//! complete for the opaque class and deny-by-default for the deps class.
//!
//! # Why this is the right seam (DESIGN.md §10.5, §4.10)
//!
//! Heavy, derived trees never enter the per-node content store; they are
//! reconstructed **read-only** into a sandbox from a project-global,
//! content-addressed, platform-keyed cache. The split between *deciding what an
//! asset is* ([`AssetKey`](crate::AssetKey)) and *how to obtain and place its
//! bytes* (this trait) is what lets the deps-excluded policy be frozen in F0
//! while the ecosystem resolvers that fill the deps class ship additively later
//! — each new resolver is a new code path behind `ensure`, never an edit to this
//! interface (domino D-13).
//!
//! Design references: DESIGN.md §10.5 (deps excluded by default; reconstructed
//! read-only from a content-addressed, platform-keyed cache), §4.10 (the trait
//! is frozen in F0 with one v1 implementation), domino D-13.

use std::path::Path;

use crate::error::Result;
use crate::key::{AssetKey, AssetRef, Reclaimed};

/// The asset-cache seam.
///
/// Implementations obtain, place, and reclaim the heavy/derived trees that are
/// excluded from per-node snapshots. The trait is frozen for v1; capabilities
/// are extended by adding new implementations (or new ecosystem resolvers behind
/// an implementation), never by editing this interface.
///
/// See the [module docs](crate::store) for the full rationale.
pub trait AssetStore {
    /// Ensure the asset named by `key` is present in the cache, ingesting it from
    /// `source` if needed, and return the binding that records where its bytes
    /// live.
    ///
    /// For an [`Opaque`](crate::AssetKind::Opaque) key:
    /// - If `source` is supplied, its content is content-addressed and stored;
    ///   the resulting hash must equal the key's `content_hash`
    ///   ([`ContentMismatch`](crate::AssetError::ContentMismatch) otherwise).
    /// - If `source` is omitted, the asset must already be cached
    ///   ([`SourceRequired`](crate::AssetError::SourceRequired) otherwise);
    ///   `ensure` is then idempotent and simply confirms presence.
    ///
    /// For a [`Deps`](crate::AssetKind::Deps) key, no ecosystem resolver is
    /// registered in v1, so this returns
    /// [`EcosystemNotRegistered`](crate::AssetError::EcosystemNotRegistered) —
    /// the complete deny-by-default behavior, not a stub.
    ///
    /// # Errors
    /// See [`AssetError`](crate::AssetError) for the full set; in particular the
    /// deny-by-default and content-integrity variants above.
    fn ensure(&self, key: &AssetKey, source: Option<&Path>) -> Result<AssetRef>;

    /// Materialize the asset named by `key` into `dest`, **read-only**.
    ///
    /// The asset must already be present (call [`ensure`](AssetStore::ensure)
    /// first). The bytes are placed via copy-on-write reflink where the
    /// filesystem supports it, falling back to a plain copy otherwise, and the
    /// materialized result is marked read-only — assets are reconstructed, never
    /// edited in place (DESIGN.md §10.5).
    ///
    /// # Errors
    /// [`EcosystemNotRegistered`](crate::AssetError::EcosystemNotRegistered) for
    /// a deps key; [`SourceRequired`](crate::AssetError::SourceRequired) if the
    /// opaque asset was never ingested; plus I/O and CAS errors.
    fn materialize(&self, key: &AssetKey, dest: &Path) -> Result<()>;

    /// Reclaim every cached object not reachable from any key in `live`.
    ///
    /// `live` is the set of keys still referenced by some node/branch. Anything
    /// the cache holds that no live key points at is deleted and counted in the
    /// returned [`Reclaimed`]. Deps keys in `live` contribute nothing to reclaim
    /// (no deps bytes are stored in v1) and are not an error here.
    ///
    /// # Errors
    /// Returns an [`AssetError`](crate::AssetError) on a CAS or I/O failure while
    /// enumerating or deleting objects.
    fn gc(&self, live: &[AssetKey]) -> Result<Reclaimed>;
}
