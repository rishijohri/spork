//! Spork asset store — the deps-excluded, content-addressed asset cache seam.
//!
//! Per-node snapshots stay small and exact, but real projects carry heavy,
//! *derived* trees — `node_modules`, `venv`/`site-packages`, `target/`, build
//! outputs, ML model weights, datasets, media. Snapshotting these per node, per
//! branch would re-incur the large-repo cliff and balloon the object store.
//! Spork's policy is therefore **deps-excluded-by-default** (DESIGN.md §10.5):
//! dependency and artifact directories are excluded from per-node snapshots via
//! the F0-frozen `ignore_profile`, and the heavy/derived trees are reconstructed
//! **read-only** into a sandbox from a project-global, **content-addressed,
//! platform-keyed, offline-safe** asset cache, materialized via
//! reflink/clonefile — never copied per branch, never per-node-stored.
//!
//! This crate freezes the seam that makes that policy real:
//!
//! - [`AssetKey`] / [`AssetKind`] — the stable vocabulary naming an asset. An
//!   asset is either a **reconstructable dep** keyed by
//!   `(ecosystem, lockfile-hash, platform)` ([`AssetKind::Deps`]) or an **opaque
//!   artifact** keyed by `content_hash` ([`AssetKind::Opaque`]).
//! - [`AssetStore`] — the frozen trait: [`ensure`](AssetStore::ensure) an asset
//!   is cached, [`materialize`](AssetStore::materialize) it read-only into a
//!   sandbox, and [`gc`](AssetStore::gc) what no live key references.
//! - [`LocalCasAssetStore`] — the **one v1 implementation**, backed by a
//!   [`spork_cas`] object store. It is **complete for the opaque class**
//!   (content-address, reflink-materialize read-only, gc) and **deny-by-default
//!   for the deps class**: a [`AssetKind::Deps`] key is refused with
//!   [`AssetError::EcosystemNotRegistered`] because no ecosystem resolver ships
//!   in F0. That is a finished capability, not a stub — resolvers (npm/pip/…) are
//!   added additively behind this same trait later (domino D-13).
//!
//! # Why a frozen trait now and resolvers later (C3, C4, D-13)
//!
//! Whether deps are excluded-and-reconstructed *changes snapshot identity* via
//! the `ignore_profile_hash` baked into the [`spork_cas::Snapshot`] object, so
//! that decision is frozen at Foundation (F0). The `AssetStore` trait is frozen
//! alongside it with exactly one v1 implementation. New ecosystem resolvers are
//! a *new code path behind [`AssetStore::ensure`]*, never an edit to the trait
//! its users depend on — the no-domino seam D-13.
//!
//! # Example
//! ```
//! use spork_asset::{AssetKey, AssetStore, LocalCasAssetStore};
//! use tempfile::TempDir;
//!
//! let tmp = TempDir::new().unwrap();
//! let store = LocalCasAssetStore::open(tmp.path().join("assets")).unwrap();
//!
//! // Ingest an opaque artifact and key it by its content hash.
//! let src = tmp.path().join("weights.bin");
//! std::fs::write(&src, b"a large opaque artifact").unwrap();
//! let content_hash = store.content_hash_for_source(&src).unwrap();
//! let key = AssetKey::opaque(content_hash);
//! let asset = store.ensure(&key, Some(&src)).unwrap();
//! assert_eq!(asset.stored, content_hash);
//!
//! // Materialize it read-only into a sandbox.
//! let dest = tmp.path().join("sandbox/weights.bin");
//! store.materialize(&key, &dest).unwrap();
//! assert_eq!(std::fs::read(&dest).unwrap(), b"a large opaque artifact");
//! assert!(std::fs::metadata(&dest).unwrap().permissions().readonly());
//!
//! // A reconstructable-dep key denies by default in F0.
//! let deps = AssetKey::deps("npm", content_hash, "aarch64-apple-darwin");
//! assert!(store.ensure(&deps, None).is_err());
//! ```
//!
//! Design references: DESIGN.md §10.5 (deps excluded by default; reconstructed
//! read-only from a content-addressed, platform-keyed cache), §4.10 (the
//! `AssetStore` trait is frozen in F0 with one v1 implementation shipping later),
//! domino D-13.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod key;
mod local;
mod store;

pub use error::{AssetError, Result};
pub use key::{AssetKey, AssetKind, AssetRef, Reclaimed, ASSET_KEY_VERSION};
pub use local::LocalCasAssetStore;
pub use store::AssetStore;
