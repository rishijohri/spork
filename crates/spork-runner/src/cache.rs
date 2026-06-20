//! The content-addressed derivation cache: [`ResultCache`] and its v1 impls.
//!
//! Re-running an identical deterministic check is wasted work: the result is a
//! replayable derivation, so an identical `(spec, input_tree, runner_version)`
//! must be a **cache hit** that returns the stored
//! [`ResultEnvelope`](crate::ResultEnvelope) without re-executing the runner
//! (DESIGN.md §8.1, §9.2). This module is that cache, keyed by the
//! [`DerivationKey`](crate::DerivationKey) (an [`input_digest`](crate::input_digest)
//! paired with a formula generation) so two branches that share a subtree share
//! a cached sanity result, and a formula change opens a fresh generation rather
//! than poisoning the old one.
//!
//! Two cache properties are enforced *outside* this trait, by the
//! [`CachingRunner`](crate::CachingRunner) that wraps a runner:
//!
//! - **Impure never caches.** A [`CheckSpec`](crate::CheckSpec) with `impure =
//!   true` is never looked up and never stored — it always re-runs (DESIGN.md
//!   §9.2). This module only ever sees deterministic derivations.
//! - **Append-only history.** A cache *miss* produces a fresh run with a new
//!   `run_id`; the cache stores exactly that envelope so a later hit returns the
//!   *same* run, preserving the append-only `(nodeId, runId, inputDigest)`
//!   history (DESIGN.md §7, §8.2).
//!
//! The [`ResultCache`] trait is the seam; F4 ships two real implementations
//! behind it: an [`InMemoryResultCache`] (a process-lifetime cache) and a
//! [`CasResultCache`] that persists envelopes as content-addressed blobs in a
//! [`spork_cas`] object store with a small durable key→blob index, so the cache
//! survives a restart and dedups identical envelopes by content.
//!
//! Design references: DESIGN.md §8.1 (`(spec, inputHash, runnerImage)` dedup
//! against a content-addressed result cache), §9.2 (replayable derivations;
//! impure opts out), §7 (append-only results), §4.4 (`inputDigest` /
//! `derivationKey`).

use std::collections::BTreeMap;
use std::sync::Mutex;

use spork_cas::{ObjectStore, StorageBackend};
use spork_hash::Hash;

use crate::digest::DerivationKey;
use crate::envelope::ResultEnvelope;
use crate::error::{Result, RunnerError};

/// A content-addressed cache of normalized results, keyed by
/// [`DerivationKey`](crate::DerivationKey).
///
/// The seam every cache implementation satisfies: look a derivation up, and
/// store one. Implementations must be `Sync` so a runner behind `&self` can use
/// them. Storing an entry for a key that already exists is idempotent (the
/// derivation is deterministic, so the value is identical by construction).
pub trait ResultCache {
    /// Look up the stored envelope for `key`, if any.
    ///
    /// # Errors
    /// [`RunnerError::Cache`](crate::RunnerError::Cache) if a stored entry
    /// exists but cannot be read or decoded.
    fn get(&self, key: &DerivationKey) -> Result<Option<ResultEnvelope>>;

    /// Store `envelope` under `key`.
    ///
    /// # Errors
    /// [`RunnerError::Cache`](crate::RunnerError::Cache) if the entry cannot be
    /// written.
    fn put(&self, key: &DerivationKey, envelope: &ResultEnvelope) -> Result<()>;

    /// Whether an entry exists for `key`.
    ///
    /// # Errors
    /// [`RunnerError::Cache`](crate::RunnerError::Cache) on a backend failure.
    fn contains(&self, key: &DerivationKey) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }
}

/// A process-lifetime, in-memory [`ResultCache`].
///
/// Keys are the derivation's [`storage_key`](DerivationKey::storage_key) (which
/// folds in the generation, so generations never collide). The map is behind a
/// [`Mutex`] so the cache is `Sync` and usable behind `&self`.
#[derive(Debug, Default)]
pub struct InMemoryResultCache {
    entries: Mutex<BTreeMap<Hash, ResultEnvelope>>,
}

impl InMemoryResultCache {
    /// Construct an empty in-memory cache.
    #[must_use]
    pub fn new() -> Self {
        InMemoryResultCache::default()
    }

    /// The number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.lock().expect("cache mutex poisoned").len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries
            .lock()
            .expect("cache mutex poisoned")
            .is_empty()
    }
}

impl ResultCache for InMemoryResultCache {
    fn get(&self, key: &DerivationKey) -> Result<Option<ResultEnvelope>> {
        let storage_key = key.storage_key()?;
        let entries = self.entries.lock().expect("cache mutex poisoned");
        Ok(entries.get(&storage_key).cloned())
    }

    fn put(&self, key: &DerivationKey, envelope: &ResultEnvelope) -> Result<()> {
        let storage_key = key.storage_key()?;
        let mut entries = self.entries.lock().expect("cache mutex poisoned");
        entries.insert(storage_key, envelope.clone());
        Ok(())
    }
}

/// A [`ResultCache`] that persists envelopes as content-addressed blobs in a
/// [`spork_cas`] object store.
///
/// The (small, queryable) envelope is serialized to JSON, stored as a CAS blob
/// (so two identical envelopes dedup by content — the cross-branch dedup of
/// DESIGN.md §8.2), and a durable index maps each derivation
/// [`storage_key`](DerivationKey::storage_key) to the blob's content hash. The
/// index is held in memory and persisted lazily; on construction it is loaded
/// from the backing store if present. This is the persistence-backed v1 of the
/// cache seam — the bulky-artifact side (logs, traces) is already content-
/// addressed via the [`ArtifactManifest`](crate::ArtifactManifest), so the cache
/// only needs to hold the envelope.
///
/// The index is behind a [`Mutex`] so the cache is `Sync`.
#[derive(Debug)]
pub struct CasResultCache<S: StorageBackend + Sync> {
    objects: ObjectStore<S>,
    /// Maps a derivation storage key to the content hash of its serialized
    /// envelope blob.
    index: Mutex<BTreeMap<Hash, Hash>>,
}

impl<S: StorageBackend + Sync> CasResultCache<S> {
    /// Construct a CAS-backed cache over `objects` with an empty index.
    #[must_use]
    pub fn new(objects: ObjectStore<S>) -> Self {
        CasResultCache {
            objects,
            index: Mutex::new(BTreeMap::new()),
        }
    }

    /// Construct a CAS-backed cache with a pre-loaded key→blob index.
    ///
    /// This is how a restart restores the cache: load the durable index (e.g.
    /// from a sidecar file the daemon owns) and hand it here; the envelope blobs
    /// themselves live in the object store and survive on their own.
    #[must_use]
    pub fn with_index(objects: ObjectStore<S>, index: BTreeMap<Hash, Hash>) -> Self {
        CasResultCache {
            objects,
            index: Mutex::new(index),
        }
    }

    /// Borrow the underlying object store.
    #[must_use]
    pub fn objects(&self) -> &ObjectStore<S> {
        &self.objects
    }

    /// Snapshot the current key→blob index, e.g. to persist it across a restart.
    #[must_use]
    pub fn index_snapshot(&self) -> BTreeMap<Hash, Hash> {
        self.index
            .lock()
            .expect("cache index mutex poisoned")
            .clone()
    }

    /// The number of indexed entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.lock().expect("cache index mutex poisoned").len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index
            .lock()
            .expect("cache index mutex poisoned")
            .is_empty()
    }
}

impl<S: StorageBackend + Sync> ResultCache for CasResultCache<S> {
    fn get(&self, key: &DerivationKey) -> Result<Option<ResultEnvelope>> {
        let storage_key = key.storage_key()?;
        let content_ref = {
            let index = self.index.lock().expect("cache index mutex poisoned");
            match index.get(&storage_key) {
                Some(h) => *h,
                None => return Ok(None),
            }
        };
        let bytes = self
            .objects
            .read_blob(&content_ref)
            .map_err(|e| RunnerError::Cache(format!("reading cached envelope blob: {e}")))?;
        let envelope: ResultEnvelope = serde_json::from_slice(&bytes)
            .map_err(|e| RunnerError::Cache(format!("decoding cached envelope: {e}")))?;
        Ok(Some(envelope))
    }

    fn put(&self, key: &DerivationKey, envelope: &ResultEnvelope) -> Result<()> {
        let storage_key = key.storage_key()?;
        let bytes = serde_json::to_vec(envelope)
            .map_err(|e| RunnerError::Cache(format!("encoding envelope for cache: {e}")))?;
        let (content_ref, _stats) = self
            .objects
            .put_blob_bytes(&bytes)
            .map_err(|e| RunnerError::Cache(format!("writing cached envelope blob: {e}")))?;
        let mut index = self.index.lock().expect("cache index mutex poisoned");
        index.insert(storage_key, content_ref);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Outcome;
    use spork_cas::LooseStore;
    use tempfile::TempDir;
    use ulid::Ulid;

    fn env(outcome: Outcome) -> ResultEnvelope {
        ResultEnvelope::new(outcome, Ulid::new(), Hash::from_bytes([1; 32]))
    }

    fn key(digest: u8, generation: u32) -> DerivationKey {
        DerivationKey::new(Hash::from_bytes([digest; 32]), generation)
    }

    #[test]
    fn in_memory_miss_then_hit() {
        let cache = InMemoryResultCache::new();
        let k = key(1, 1);
        assert!(cache.get(&k).unwrap().is_none());
        let stored = env(Outcome::Passed);
        cache.put(&k, &stored).unwrap();
        let hit = cache.get(&k).unwrap().unwrap();
        // The exact stored run is returned (same run_id) — append-only history.
        assert_eq!(hit.run_id, stored.run_id);
        assert_eq!(cache.len(), 1);
        assert!(!cache.is_empty());
    }

    #[test]
    fn in_memory_generation_isolation() {
        // A formula change (new generation) must not read or overwrite the old
        // generation's entry.
        let cache = InMemoryResultCache::new();
        let gen1 = key(1, 1);
        let gen2 = key(1, 2);
        let e1 = env(Outcome::Passed);
        cache.put(&gen1, &e1).unwrap();
        // Same input digest, new generation => a miss, not a stale hit.
        assert!(cache.get(&gen2).unwrap().is_none());
        let e2 = env(Outcome::Failed);
        cache.put(&gen2, &e2).unwrap();
        // The old generation is untouched.
        assert_eq!(cache.get(&gen1).unwrap().unwrap().run_id, e1.run_id);
        assert_eq!(cache.get(&gen2).unwrap().unwrap().run_id, e2.run_id);
    }

    #[test]
    fn cas_backed_miss_then_hit() {
        let tmp = TempDir::new().unwrap();
        let objects = ObjectStore::new(LooseStore::open(tmp.path().join("cas")).unwrap());
        let cache = CasResultCache::new(objects);
        let k = key(2, 1);
        assert!(cache.get(&k).unwrap().is_none());
        let stored = env(Outcome::Passed);
        cache.put(&k, &stored).unwrap();
        let hit = cache.get(&k).unwrap().unwrap();
        assert_eq!(hit, stored);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cas_backed_survives_index_reload() {
        // Simulate a restart: the envelope blobs persist in the object store,
        // and the durable index is reloaded — a hit still resolves.
        let tmp = TempDir::new().unwrap();
        let cas_dir = tmp.path().join("cas");
        let k = key(3, 1);
        let stored = env(Outcome::Failed);

        let index = {
            let objects = ObjectStore::new(LooseStore::open(&cas_dir).unwrap());
            let cache = CasResultCache::new(objects);
            cache.put(&k, &stored).unwrap();
            cache.index_snapshot()
        };

        // Reopen the store and rehydrate the cache from the persisted index.
        let objects = ObjectStore::new(LooseStore::open(&cas_dir).unwrap());
        let reopened = CasResultCache::with_index(objects, index);
        let hit = reopened.get(&k).unwrap().unwrap();
        assert_eq!(hit, stored);
    }

    #[test]
    fn cas_backed_dedups_identical_envelopes_by_content() {
        let tmp = TempDir::new().unwrap();
        let objects = ObjectStore::new(LooseStore::open(tmp.path().join("cas")).unwrap());
        let cache = CasResultCache::new(objects);
        // Two derivations, but byte-identical envelopes (same content hash).
        let shared = env(Outcome::Passed);
        cache.put(&key(4, 1), &shared).unwrap();
        cache.put(&key(5, 1), &shared).unwrap();
        let snapshot = cache.index_snapshot();
        // Two index entries...
        assert_eq!(snapshot.len(), 2);
        // ...pointing at the *same* blob (content dedup).
        let blobs: std::collections::BTreeSet<_> = snapshot.values().collect();
        assert_eq!(blobs.len(), 1);
    }
}
