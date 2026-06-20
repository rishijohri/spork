//! The content-addressed environment descriptor: [`EnvManifest`].
//!
//! Cross-branch result comparison is only meaningful if the two branches ran in
//! the *same* environment. Spork makes that checkable by pinning the environment
//! declaratively and **hashing it into node identity**: an [`EnvManifest`]
//! records the toolchain versions, the lockfile hashes, and (for the
//! container/microVM tiers) a base-image digest, is canonically serialized
//! ([`spork_canon`]), and BLAKE3-hashed into a single
//! [`env_manifest_hash`](EnvManifest::env_manifest_hash). **Two nodes with the
//! same manifest hash are guaranteed environment-identical** (DESIGN.md §11.3,
//! "Reproducible environments").
//!
//! Because the hash is derived from the *canonical* encoding, it is
//! order-insensitive in the maps (toolchain, lockfile hashes are
//! [`BTreeMap`](std::collections::BTreeMap)s and the canonical encoder sorts
//! object keys by raw UTF-8 bytes) and byte-stable across platforms — the same
//! manifest always yields the same hash, on any machine.
//!
//! The struct carries its own [`EnvManifest::schema_version`] (constraint C5) so
//! the manifest's shape can evolve via a loud version bump and a registered
//! forward-migration rather than a silent reinterpretation. The version
//! participates in the hash, so a schema change opens a new identity generation
//! and never collides an old manifest with a re-shaped new one.
//!
//! Design references: DESIGN.md §11.3 (reproducible, content-addressed
//! `EnvManifest` hashed into node identity), §5.3 (execution & isolation), §6.1
//! (BLAKE3-everywhere content identity).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use spork_canon::canonicalize;
use spork_hash::{hash_bytes, Hash};

use crate::error::Result;

/// The current schema version stamped on a freshly constructed [`EnvManifest`].
///
/// Persisted/hashed manifests carry this so a future change to the manifest's
/// shape is a loud, additive version bump rather than a silent reinterpretation
/// (constraint C5). The version participates in
/// [`env_manifest_hash`](EnvManifest::env_manifest_hash).
pub const ENV_MANIFEST_VERSION: u16 = 1;

/// A declarative, content-addressed description of the execution environment.
///
/// An `EnvManifest` is **hashed into node identity**: its canonical encoding is
/// BLAKE3-hashed into [`env_manifest_hash`](EnvManifest::env_manifest_hash), and
/// two nodes with the same hash are guaranteed environment-identical, which is
/// what makes cross-branch result comparison meaningful (DESIGN.md §11.3).
///
/// All identity-bearing fields are deterministically ordered: the maps are
/// [`BTreeMap`]s and the canonical encoder sorts object keys, so the hash does
/// not depend on insertion order. The optional
/// [`base_image_digest`](EnvManifest::base_image_digest) applies to the
/// container/microVM tiers (P8); the F4 worktree tier serves a "best-effort
/// inherit-host" mode where the field is typically `None` (DESIGN.md §11.3).
///
/// # Example
/// ```
/// use spork_exec::EnvManifest;
///
/// let mut a = EnvManifest::new();
/// a.toolchain.insert("rustc".into(), "1.83.0".into());
/// let mut b = EnvManifest::new();
/// b.toolchain.insert("rustc".into(), "1.83.0".into());
///
/// // Same declared environment => same identity hash.
/// assert_eq!(a.env_manifest_hash().unwrap(), b.env_manifest_hash().unwrap());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvManifest {
    /// The schema version of this manifest's wire form ([`ENV_MANIFEST_VERSION`]
    /// for freshly constructed manifests). Participates in the identity hash.
    pub schema_version: u16,

    /// Pinned toolchain versions, keyed by tool name (e.g. `"rustc" =>
    /// "1.83.0"`, `"node" => "20.11.1"`). Sorted deterministically by the
    /// canonical encoder, so insertion order does not affect identity.
    pub toolchain: BTreeMap<String, String>,

    /// The BLAKE3 hashes of the lockfiles/manifests that pin dependency
    /// resolution, keyed by ecosystem or lockfile name (e.g.
    /// `"Cargo.lock" => <hash>`). These are what bind a manifest to the exact
    /// resolved dependency set the [`AssetStore`](spork_asset::AssetStore)
    /// reconstructs (DESIGN.md §10.5).
    pub lockfile_hashes: BTreeMap<String, Hash>,

    /// An optional base-image digest for the container/microVM tiers (e.g. an
    /// OCI image digest). The F4 worktree tier inherits the host and usually
    /// leaves this `None` (DESIGN.md §11.3); it is part of identity so a strict
    /// manifest on a heavier tier hashes distinctly.
    pub base_image_digest: Option<String>,
}

impl Default for EnvManifest {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvManifest {
    /// Construct an empty manifest stamped with the current
    /// [`ENV_MANIFEST_VERSION`].
    ///
    /// An empty manifest is a legitimate value — it describes the
    /// "inherit-host, nothing pinned" environment of the worktree tier and has a
    /// stable, well-defined identity hash.
    #[must_use]
    pub fn new() -> Self {
        EnvManifest {
            schema_version: ENV_MANIFEST_VERSION,
            toolchain: BTreeMap::new(),
            lockfile_hashes: BTreeMap::new(),
            base_image_digest: None,
        }
    }

    /// Compute the content-addressed identity hash of this manifest.
    ///
    /// The manifest is canonically serialized ([`spork_canon::canonicalize`])
    /// and BLAKE3-hashed. The result is byte-stable across platforms and
    /// insertion orders, so two manifests describing the same environment always
    /// produce the same hash — the invariant that makes cross-branch comparison
    /// meaningful (DESIGN.md §11.3). The [`schema_version`](Self::schema_version)
    /// participates in the hash, so a schema change opens a new identity
    /// generation.
    ///
    /// # Errors
    /// Returns [`ExecError::Canon`](crate::ExecError::Canon) if canonicalization
    /// fails (only possible if a float ever entered the manifest, which the
    /// integer-only shape prevents).
    pub fn env_manifest_hash(&self) -> Result<Hash> {
        let bytes = canonicalize(self)?;
        Ok(hash_bytes(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> Hash {
        Hash::from_bytes([b; 32])
    }

    #[test]
    fn new_stamps_current_version() {
        let m = EnvManifest::new();
        assert_eq!(m.schema_version, ENV_MANIFEST_VERSION);
        assert!(m.toolchain.is_empty());
        assert!(m.lockfile_hashes.is_empty());
        assert_eq!(m.base_image_digest, None);
        assert_eq!(EnvManifest::default(), m);
    }

    #[test]
    fn identical_manifests_hash_identically() {
        let mut a = EnvManifest::new();
        a.toolchain.insert("rustc".into(), "1.83.0".into());
        a.toolchain.insert("node".into(), "20.11.1".into());
        a.lockfile_hashes.insert("Cargo.lock".into(), h(1));

        let mut b = EnvManifest::new();
        // Insert in a *different* order to prove order-insensitivity.
        b.lockfile_hashes.insert("Cargo.lock".into(), h(1));
        b.toolchain.insert("node".into(), "20.11.1".into());
        b.toolchain.insert("rustc".into(), "1.83.0".into());

        assert_eq!(a, b);
        assert_eq!(
            a.env_manifest_hash().unwrap(),
            b.env_manifest_hash().unwrap()
        );
    }

    #[test]
    fn different_environments_hash_differently() {
        let mut a = EnvManifest::new();
        a.toolchain.insert("rustc".into(), "1.83.0".into());

        let mut b = EnvManifest::new();
        b.toolchain.insert("rustc".into(), "1.84.0".into());

        assert_ne!(
            a.env_manifest_hash().unwrap(),
            b.env_manifest_hash().unwrap()
        );
    }

    #[test]
    fn base_image_digest_is_part_of_identity() {
        let a = EnvManifest::new();
        let mut b = EnvManifest::new();
        b.base_image_digest = Some("sha256:deadbeef".into());
        assert_ne!(
            a.env_manifest_hash().unwrap(),
            b.env_manifest_hash().unwrap()
        );
    }

    #[test]
    fn lockfile_hash_is_part_of_identity() {
        let mut a = EnvManifest::new();
        a.lockfile_hashes.insert("Cargo.lock".into(), h(1));
        let mut b = EnvManifest::new();
        b.lockfile_hashes.insert("Cargo.lock".into(), h(2));
        assert_ne!(
            a.env_manifest_hash().unwrap(),
            b.env_manifest_hash().unwrap()
        );
    }

    #[test]
    fn empty_manifest_has_stable_hash() {
        let a = EnvManifest::new();
        let b = EnvManifest::new();
        assert_eq!(
            a.env_manifest_hash().unwrap(),
            b.env_manifest_hash().unwrap()
        );
    }

    #[test]
    fn manifest_round_trips_through_serde() {
        let mut m = EnvManifest::new();
        m.toolchain.insert("python".into(), "3.12.1".into());
        m.lockfile_hashes.insert("poetry.lock".into(), h(7));
        m.base_image_digest = Some("sha256:abc123".into());

        let json = serde_json::to_string(&m).unwrap();
        let back: EnvManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // Identity survives the round-trip.
        assert_eq!(
            m.env_manifest_hash().unwrap(),
            back.env_manifest_hash().unwrap()
        );
    }

    #[test]
    fn schema_version_participates_in_hash() {
        // Two otherwise-equal manifests with different schema versions must hash
        // differently — a schema change opens a new identity generation.
        let a = EnvManifest::new();
        let mut b = EnvManifest::new();
        b.schema_version = ENV_MANIFEST_VERSION + 1;
        assert_ne!(
            a.env_manifest_hash().unwrap(),
            b.env_manifest_hash().unwrap()
        );
    }
}
