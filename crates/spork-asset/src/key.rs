//! The frozen asset vocabulary: [`AssetKey`], [`AssetKind`], [`AssetRef`],
//! [`Reclaimed`].
//!
//! These types name *what* an asset is, independently of *how* it is stored.
//! They are the stable identity surface of the asset layer (DESIGN.md §10.5):
//! heavy, derived trees — `node_modules`, `venv`, `target/`, model weights,
//! datasets, media — are excluded from per-node snapshots and reconstructed from
//! a project-global, content-addressed, platform-keyed cache. An asset falls
//! into exactly one of two classes:
//!
//! - **Reconstructable deps** ([`AssetKind::Deps`]) — keyed by
//!   `(ecosystem, lockfile-hash, platform)`. The cache stores the *resolved*
//!   package artifacts content-addressed, so reconstruction is offline-safe and
//!   immune to registry yanks. No ecosystem resolver ships in F0, so this class
//!   denies by default (see [`crate::AssetError::EcosystemNotRegistered`]).
//! - **Opaque artifacts** ([`AssetKind::Opaque`]) — keyed by `content_hash`.
//!   Large blobs (ML weights, datasets, media) deduped once globally and
//!   materialized read-only. This is the class the v1 `LocalCasAssetStore`
//!   implements completely.
//!
//! # Identity is content-addressed and evolution-safe (C5)
//!
//! [`AssetKey`] derives serde `Serialize`/`Deserialize` and carries a
//! [`AssetKey::schema_version`] so the key's wire form can evolve without an
//! in-place reinterpretation. Two keys are equal iff their `(kind,
//! schema_version)` are equal — the key is a value, not a handle.
//!
//! Design references: DESIGN.md §10.5 (the two asset classes and their keys),
//! §4.10 (the `AssetStore` trait is frozen in F0), domino D-13.

use serde::{Deserialize, Serialize};

use spork_hash::Hash;

/// The current schema version stamped on a freshly constructed [`AssetKey`].
///
/// Persisted/hashed asset keys carry this so a future change to the key's shape
/// is a loud, additive version bump rather than a silent reinterpretation
/// (constraint C5).
pub const ASSET_KEY_VERSION: u16 = 1;

/// Which class an asset belongs to, and the fields that identify it within that
/// class.
///
/// This is a frozen v1 contract: new classes are *added* as variants (the
/// compiler then flags every exhaustive `match` that must consider them), never
/// reinterpreted. `#[non_exhaustive]` is deliberately omitted so downstream
/// matches are forced to handle a new class when one is added.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssetKind {
    /// A reconstructable dependency set, keyed by the triple that makes its
    /// reconstruction deterministic and offline-safe (DESIGN.md §10.5).
    ///
    /// The cache stores the resolved package artifacts content-addressed, so the
    /// same `(ecosystem, lockfile_hash, platform)` re-materializes the same bytes
    /// without a network round-trip. No resolver ships in F0, so an
    /// [`AssetStore`](crate::AssetStore) refuses this class with
    /// [`AssetError::EcosystemNotRegistered`](crate::AssetError::EcosystemNotRegistered).
    Deps {
        /// The package ecosystem, e.g. `"npm"`, `"pip"`, `"cargo"`.
        ecosystem: String,
        /// The BLAKE3 hash of the lockfile/manifest the deps were resolved from.
        lockfile_hash: Hash,
        /// The platform/arch the artifacts are resolved for, e.g.
        /// `"aarch64-apple-darwin"`. Deps are platform-keyed because resolved
        /// artifacts are not portable across platforms.
        platform: String,
    },

    /// An opaque, content-addressed artifact (ML weights, datasets, media),
    /// keyed solely by the BLAKE3 hash of its content.
    ///
    /// Deduped once globally and materialized read-only. This is the class the
    /// v1 [`LocalCasAssetStore`](crate::LocalCasAssetStore) implements end to
    /// end.
    Opaque {
        /// The BLAKE3 content address of the artifact — the cache id under which
        /// its bytes are stored and deduped.
        content_hash: Hash,
    },
}

impl AssetKind {
    /// A short, stable label for diagnostics and logging.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            AssetKind::Deps { .. } => "deps",
            AssetKind::Opaque { .. } => "opaque",
        }
    }
}

/// The stable identity of an asset: its [`AssetKind`] plus a schema version.
///
/// An `AssetKey` is a *value*, not a handle to stored bytes; it is what callers
/// pass to [`AssetStore::ensure`](crate::AssetStore::ensure),
/// [`materialize`](crate::AssetStore::materialize), and
/// [`gc`](crate::AssetStore::gc). It derives serde so it can be persisted into a
/// node's manifest (the snapshot records *which* assets it needs, not their
/// bytes), and carries [`AssetKey::schema_version`] for evolution safety (C5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AssetKey {
    /// The class and identifying fields of the asset.
    pub kind: AssetKind,
    /// The schema version of this key's wire form ([`ASSET_KEY_VERSION`] for
    /// freshly constructed keys).
    pub schema_version: u16,
}

impl AssetKey {
    /// Construct a key for an opaque, content-addressed artifact.
    ///
    /// Stamps the current [`ASSET_KEY_VERSION`].
    #[must_use]
    pub fn opaque(content_hash: Hash) -> Self {
        AssetKey {
            kind: AssetKind::Opaque { content_hash },
            schema_version: ASSET_KEY_VERSION,
        }
    }

    /// Construct a key for a reconstructable dependency set.
    ///
    /// Stamps the current [`ASSET_KEY_VERSION`]. Such a key is refused by the v1
    /// store ([`EcosystemNotRegistered`](crate::AssetError::EcosystemNotRegistered))
    /// until a resolver for `ecosystem` is registered in a later phase.
    #[must_use]
    pub fn deps(
        ecosystem: impl Into<String>,
        lockfile_hash: Hash,
        platform: impl Into<String>,
    ) -> Self {
        AssetKey {
            kind: AssetKind::Deps {
                ecosystem: ecosystem.into(),
                lockfile_hash,
                platform: platform.into(),
            },
            schema_version: ASSET_KEY_VERSION,
        }
    }
}

/// The result of an [`AssetStore::ensure`](crate::AssetStore::ensure): the key
/// that was ensured and the content address its bytes are stored under.
///
/// For the opaque class, `stored` equals the key's `content_hash` (the asset is
/// addressed by its content). The struct is returned so a caller can record the
/// binding without re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    /// The key that was ensured present.
    pub key: AssetKey,
    /// The content address under which the asset's bytes are stored.
    pub stored: Hash,
}

/// What a [`gc`](crate::AssetStore::gc) reclaimed.
///
/// Reports the number of stored objects deleted and the number of payload bytes
/// freed, so the dedup/reclaim story is measurable (mirroring
/// [`spork_cas::PutStats`] on the capture side).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reclaimed {
    /// The number of stored objects (asset records, blobs, trees, chunks)
    /// deleted.
    pub objects: u64,
    /// The number of on-disk bytes freed.
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> Hash {
        Hash::from_bytes([byte; 32])
    }

    #[test]
    fn opaque_constructor_stamps_version() {
        let k = AssetKey::opaque(h(7));
        assert_eq!(k.schema_version, ASSET_KEY_VERSION);
        assert_eq!(k.kind, AssetKind::Opaque { content_hash: h(7) });
        assert_eq!(k.kind.label(), "opaque");
    }

    #[test]
    fn deps_constructor_stamps_version_and_fields() {
        let k = AssetKey::deps("npm", h(3), "aarch64-apple-darwin");
        assert_eq!(k.schema_version, ASSET_KEY_VERSION);
        match &k.kind {
            AssetKind::Deps {
                ecosystem,
                lockfile_hash,
                platform,
            } => {
                assert_eq!(ecosystem, "npm");
                assert_eq!(*lockfile_hash, h(3));
                assert_eq!(platform, "aarch64-apple-darwin");
            }
            other => panic!("expected Deps, got {other:?}"),
        }
        assert_eq!(k.kind.label(), "deps");
    }

    #[test]
    fn key_round_trips_through_serde() {
        for k in [
            AssetKey::opaque(h(1)),
            AssetKey::deps("pip", h(2), "x86_64-unknown-linux-gnu"),
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: AssetKey = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
    }

    #[test]
    fn asset_ref_round_trips_through_serde() {
        let r = AssetRef {
            key: AssetKey::opaque(h(9)),
            stored: h(9),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: AssetRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn deps_platform_is_part_of_identity() {
        // Two deps keys identical but for platform must be distinct keys.
        let a = AssetKey::deps("npm", h(5), "aarch64-apple-darwin");
        let b = AssetKey::deps("npm", h(5), "x86_64-unknown-linux-gnu");
        assert_ne!(a, b);
    }
}
