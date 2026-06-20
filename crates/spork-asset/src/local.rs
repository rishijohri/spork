//! The one v1 [`AssetStore`] implementation: [`LocalCasAssetStore`].
//!
//! A local, on-disk asset cache backed by a [`spork_cas`] object store. It is
//! **complete for the opaque class** and **deny-by-default for the deps class**
//! (DESIGN.md §10.5): opaque artifacts (ML weights, datasets, media) are
//! content-addressed, deduped once, and materialized read-only via
//! reflink/clonefile with a copy fallback; reconstructable deps are refused with
//! [`AssetError::EcosystemNotRegistered`] because no ecosystem resolver ships in
//! F0 — that capability is added additively behind the same
//! [`AssetStore`](crate::AssetStore) trait later (domino D-13).
//!
//! # On-disk layout
//!
//! The store owns a single root directory and three areas beneath it:
//!
//! ```text
//! <root>/
//!   objects/                 a spork-cas LooseStore the asset store fully owns
//!     <aa>/<rest>            content-addressed chunks/blobs/trees (dedup + integrity)
//!     pack/                  (managed by spork-cas)
//!   payloads/<content_hash>/ the read-only canonical materialization, the reflink source
//!   records/<content_hash>   one self-describing AssetRecord per ingested opaque asset
//! ```
//!
//! The CAS `objects/` area is the source of truth for content identity, dedup,
//! and integrity. Because the asset store owns that directory exclusively, it can
//! enumerate and delete loose objects during [`gc`](AssetStore::gc) — something a
//! shared, append-only CAS could not safely do. The `payloads/` area holds a
//! materialized read-only copy per asset that [`materialize`](AssetStore::materialize)
//! reflinks into a sandbox, so per-branch checkout is O(changed bytes), never a
//! per-branch copy (DESIGN.md §10.5, §6.4 worktree-on-CoW).
//!
//! # Content addressing of an opaque asset
//!
//! Ingesting a *file* stores its bytes as a CAS [`Blob`](spork_cas::Blob) and
//! records its permission mode; ingesting a *directory* stores it as a CAS
//! [`Tree`](spork_cas::Tree). The asset's `content_hash` is the BLAKE3 of the
//! canonical identity record `{kind, root, mode}` — a single stable hash that
//! does not depend on how the bytes were chunked (chunking is itself
//! deterministic). An [`ensure`](AssetStore::ensure) whose supplied source hashes
//! to a different value than the key declares is refused
//! ([`ContentMismatch`](crate::AssetError::ContentMismatch)).
//!
//! Design references: DESIGN.md §10.5 (deps excluded by default; opaque artifacts
//! content-addressed, deduped once globally, materialized read-only), §4.10 (the
//! `AssetStore` trait is frozen in F0 with one v1 implementation), domino D-13.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use spork_cas::{Blob, EntryKind, LooseStore, ObjectStore, StorageBackend, Tree};
use spork_hash::Hash;

use crate::error::{AssetError, Result};
use crate::key::{AssetKey, AssetKind, AssetRef, Reclaimed};
use crate::store::AssetStore;

/// The schema version stamped on a freshly written [`AssetRecord`].
const ASSET_RECORD_VERSION: u16 = 1;

/// What an ingested opaque asset materializes to.
///
/// Recorded explicitly so [`materialize`](LocalCasAssetStore::materialize) and
/// [`gc`](LocalCasAssetStore::gc) never need to introspect a CAS object's header
/// to tell a file from a directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum RecordPayload {
    /// A single regular file: its CAS blob id and the permission mode to restore
    /// (before the read-only marking).
    File {
        /// The CAS [`Blob`](spork_cas::Blob) id holding the file's bytes.
        blob: Hash,
        /// The file's permission mode at ingest (informational; the
        /// materialized copy is forced read-only regardless).
        mode: u32,
    },
    /// A directory tree: its CAS [`Tree`](spork_cas::Tree) root id.
    Dir {
        /// The CAS [`Tree`](spork_cas::Tree) id at the root of the directory.
        tree: Hash,
    },
}

/// The self-describing index entry written per ingested opaque asset.
///
/// Persisted under `records/<content_hash>`; carries a schema `v` (C5) so its
/// wire form can evolve loudly. The `refs` field is the complete transitive
/// closure of CAS object hashes this asset depends on (its root, every
/// sub-tree, every blob, every chunk), captured at ingest so
/// [`gc`](LocalCasAssetStore::gc) computes the live set by simple union — no
/// graph walk and no header introspection at collection time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AssetRecord {
    /// Schema version of this record.
    v: u16,
    /// The asset's content address (matches the key's `content_hash` and the
    /// record's filename).
    content_hash: Hash,
    /// What the asset materializes to.
    payload: RecordPayload,
    /// The full set of CAS object hashes this asset references (root + all
    /// transitively reachable trees, blobs, and chunks). Sorted for a stable,
    /// canonical record.
    refs: Vec<Hash>,
}

/// A local, on-disk asset cache backed by a [`spork_cas`] object store.
///
/// Construct with [`LocalCasAssetStore::open`]. Implements [`AssetStore`]:
/// complete for [`AssetKind::Opaque`], deny-by-default for [`AssetKind::Deps`].
#[derive(Debug, Clone)]
pub struct LocalCasAssetStore {
    /// The owned root directory holding `objects/`, `payloads/`, and `records/`.
    root: PathBuf,
    /// The owned content-addressed object store (dedup, integrity, GC source).
    objects: ObjectStore<LooseStore>,
}

impl LocalCasAssetStore {
    /// Open (creating directories as needed) an asset cache rooted at `root`.
    ///
    /// The CAS objects live at `root/objects`, materialized payloads at
    /// `root/payloads`, and the asset index at `root/records`.
    ///
    /// # Errors
    /// Returns [`AssetError::Io`] if the directories cannot be created, or an
    /// [`AssetError::Cas`] if the backing object store cannot be opened.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let backend = LooseStore::open(&root)?;
        let objects = ObjectStore::new(backend);
        let store = LocalCasAssetStore { root, objects };
        fs::create_dir_all(store.payloads_dir())
            .map_err(|e| AssetError::io(store.payloads_dir(), e))?;
        fs::create_dir_all(store.records_dir())
            .map_err(|e| AssetError::io(store.records_dir(), e))?;
        Ok(store)
    }

    /// The directory holding the read-only materialized payloads (reflink
    /// sources).
    fn payloads_dir(&self) -> PathBuf {
        self.root.join("payloads")
    }

    /// The directory holding the per-asset index records.
    fn records_dir(&self) -> PathBuf {
        self.root.join("records")
    }

    /// The record file path for a content hash.
    fn record_path(&self, content_hash: &Hash) -> PathBuf {
        self.records_dir().join(content_hash.to_hex())
    }

    /// The canonical payload directory for a content hash.
    fn payload_dir(&self, content_hash: &Hash) -> PathBuf {
        self.payloads_dir().join(content_hash.to_hex())
    }

    /// Borrow the backing content-addressed object store (read-only access for
    /// callers that want to inspect dedup or integrity).
    #[must_use]
    pub fn objects(&self) -> &ObjectStore<LooseStore> {
        &self.objects
    }

    /// Ingest `source` into the CAS, returning the record payload and the full
    /// transitive set of referenced CAS object hashes.
    fn ingest(&self, source: &Path) -> Result<(RecordPayload, BTreeSet<Hash>)> {
        let meta = fs::symlink_metadata(source).map_err(|e| AssetError::io(source, e))?;
        if meta.file_type().is_symlink() {
            // Follow the link for the root only; a dangling root link cannot be
            // ingested faithfully.
            let target_meta = fs::metadata(source)
                .map_err(|_| AssetError::unsupported(source, "dangling symlink at asset root"))?;
            return self.ingest_resolved(source, &target_meta);
        }
        self.ingest_resolved(source, &meta)
    }

    /// Ingest a source whose (resolved) metadata is `meta`.
    fn ingest_resolved(
        &self,
        source: &Path,
        meta: &fs::Metadata,
    ) -> Result<(RecordPayload, BTreeSet<Hash>)> {
        let mut refs = BTreeSet::new();
        if meta.is_file() {
            let bytes = fs::read(source).map_err(|e| AssetError::io(source, e))?;
            let (blob, _stats) = self.objects.put_blob_bytes(&bytes)?;
            self.collect_blob_refs(&blob, &mut refs)?;
            Ok((
                RecordPayload::File {
                    blob,
                    mode: mode_of(meta),
                },
                refs,
            ))
        } else if meta.is_dir() {
            // Capture the directory with a permissive (empty) ignore profile:
            // an opaque asset is captured verbatim, not subject to the
            // deps-excluded code policy.
            let matcher = permissive_matcher();
            let (tree, _stats) = self.objects.put_tree(source, &matcher)?;
            self.collect_tree_refs(&tree, &mut refs)?;
            Ok((RecordPayload::Dir { tree }, refs))
        } else {
            Err(AssetError::unsupported(
                source,
                "opaque source must be a regular file or a directory",
            ))
        }
    }

    /// Add a blob id and all of its chunk ids to `refs`.
    fn collect_blob_refs(&self, blob_id: &Hash, refs: &mut BTreeSet<Hash>) -> Result<()> {
        refs.insert(*blob_id);
        let bytes = self
            .objects
            .backend()
            .get(blob_id)?
            .ok_or_else(|| AssetError::Cas(format!("blob {blob_id} vanished after store")))?;
        let blob: Blob = serde_json::from_slice(&bytes)
            .map_err(|e| AssetError::Decode(format!("blob {blob_id}: {e}")))?;
        for chunk in &blob.chunks {
            refs.insert(*chunk);
        }
        Ok(())
    }

    /// Add a tree id and the full transitive closure beneath it to `refs`.
    fn collect_tree_refs(&self, tree_id: &Hash, refs: &mut BTreeSet<Hash>) -> Result<()> {
        if !refs.insert(*tree_id) {
            // Already visited (subtree dedup means a tree can appear twice).
            return Ok(());
        }
        let bytes = self
            .objects
            .backend()
            .get(tree_id)?
            .ok_or_else(|| AssetError::Cas(format!("tree {tree_id} vanished after store")))?;
        let tree: Tree = serde_json::from_slice(&bytes)
            .map_err(|e| AssetError::Decode(format!("tree {tree_id}: {e}")))?;
        for entry in &tree.entries {
            match entry.kind {
                EntryKind::Dir => self.collect_tree_refs(&entry.target, refs)?,
                // Files and symlinks both store their bytes/target text as a blob.
                EntryKind::File | EntryKind::Symlink => {
                    self.collect_blob_refs(&entry.target, refs)?;
                }
            }
        }
        Ok(())
    }

    /// Write the read-only canonical payload for `content_hash` from its record,
    /// reconstructing it from the CAS. Idempotent: an already-materialized
    /// payload is left untouched.
    fn write_canonical_payload(&self, record: &AssetRecord) -> Result<()> {
        let dest = self.payload_dir(&record.content_hash);
        if dest.exists() {
            return Ok(());
        }
        // Reconstruct into a temp dir, mark read-only, then rename into place so a
        // crash never leaves a half-written canonical payload.
        let tmp = self
            .payloads_dir()
            .join(format!(".{}.tmp", record.content_hash.to_hex()));
        if tmp.exists() {
            remove_path(&tmp)?;
        }
        match &record.payload {
            RecordPayload::File { blob, .. } => {
                let bytes = self.objects.read_blob(blob)?;
                if let Some(parent) = tmp.parent() {
                    fs::create_dir_all(parent).map_err(|e| AssetError::io(parent, e))?;
                }
                // The payload for a file is a directory containing one file named
                // `data`, so a file and a dir payload reconstruct uniformly.
                fs::create_dir_all(&tmp).map_err(|e| AssetError::io(&tmp, e))?;
                let file_path = tmp.join("data");
                fs::write(&file_path, &bytes).map_err(|e| AssetError::io(&file_path, e))?;
            }
            RecordPayload::Dir { tree } => {
                self.objects.materialize_tree(tree, &tmp)?;
            }
        }
        set_tree_readonly(&tmp)?;
        fs::rename(&tmp, &dest).map_err(|e| AssetError::io(&dest, e))?;
        Ok(())
    }

    /// Load and decode the record for `content_hash`, or `None` if absent.
    fn load_record(&self, content_hash: &Hash) -> Result<Option<AssetRecord>> {
        let path = self.record_path(content_hash);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AssetError::io(&path, e)),
        };
        let record: AssetRecord = serde_json::from_slice(&bytes)
            .map_err(|e| AssetError::Decode(format!("record {content_hash}: {e}")))?;
        if record.v != ASSET_RECORD_VERSION {
            return Err(AssetError::Decode(format!(
                "record {content_hash} has unsupported schema version {}",
                record.v
            )));
        }
        Ok(Some(record))
    }

    /// Atomically write `record` to its record file.
    fn write_record(&self, record: &AssetRecord) -> Result<()> {
        let path = self.record_path(&record.content_hash);
        let bytes = serde_json::to_vec(record)
            .map_err(|e| AssetError::Decode(format!("encoding record: {e}")))?;
        let tmp = self
            .records_dir()
            .join(format!(".{}.tmp", record.content_hash.to_hex()));
        fs::write(&tmp, &bytes).map_err(|e| AssetError::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| AssetError::io(&path, e))?;
        Ok(())
    }

    /// Handle the opaque-class `ensure`.
    fn ensure_opaque(&self, content_hash: &Hash, source: Option<&Path>) -> Result<Hash> {
        // Already cached? Confirm and (if a source was given) integrity-check it.
        if let Some(existing) = self.load_record(content_hash)? {
            // Make sure the canonical payload exists (it may have been GC'd while
            // the record survived, or this is a fresh open).
            self.write_canonical_payload(&existing)?;
            return Ok(existing.content_hash);
        }

        let source = source.ok_or(AssetError::SourceRequired(*content_hash))?;
        let (payload, refs) = self.ingest(source)?;
        let computed = content_hash_of(&payload);
        if &computed != content_hash {
            return Err(AssetError::ContentMismatch {
                expected: *content_hash,
                actual: computed,
            });
        }
        let record = AssetRecord {
            v: ASSET_RECORD_VERSION,
            content_hash: computed,
            payload,
            refs: refs.into_iter().collect(),
        };
        self.write_canonical_payload(&record)?;
        self.write_record(&record)?;
        Ok(computed)
    }

    /// Compute the `content_hash` a source would produce, without committing it.
    ///
    /// Useful for constructing the [`AssetKey`](crate::AssetKey) before calling
    /// [`ensure`](AssetStore::ensure): ingest is content-addressed, so the hash
    /// returned here is exactly what `ensure` will require the key to declare.
    ///
    /// # Errors
    /// As [`ingest`](LocalCasAssetStore::ingest): I/O, unsupported source, or CAS
    /// errors.
    pub fn content_hash_for_source(&self, source: &Path) -> Result<Hash> {
        let (payload, _refs) = self.ingest(source)?;
        Ok(content_hash_of(&payload))
    }
}

impl AssetStore for LocalCasAssetStore {
    fn ensure(&self, key: &AssetKey, source: Option<&Path>) -> Result<AssetRef> {
        match &key.kind {
            AssetKind::Deps { ecosystem, .. } => {
                Err(AssetError::EcosystemNotRegistered(ecosystem.clone()))
            }
            AssetKind::Opaque { content_hash } => {
                let stored = self.ensure_opaque(content_hash, source)?;
                Ok(AssetRef {
                    key: key.clone(),
                    stored,
                })
            }
        }
    }

    fn materialize(&self, key: &AssetKey, dest: &Path) -> Result<()> {
        let content_hash = match &key.kind {
            AssetKind::Deps { ecosystem, .. } => {
                return Err(AssetError::EcosystemNotRegistered(ecosystem.clone()));
            }
            AssetKind::Opaque { content_hash } => content_hash,
        };

        let record = self
            .load_record(content_hash)?
            .ok_or(AssetError::SourceRequired(*content_hash))?;
        // Defensive: ensure the canonical payload is present before reflinking.
        self.write_canonical_payload(&record)?;
        let src = self.payload_dir(content_hash);

        match &record.payload {
            RecordPayload::File { .. } => {
                // The canonical payload is `<payload_dir>/data`; reflink that file
                // to `dest` directly (dest names the file).
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| AssetError::io(parent, e))?;
                }
                if dest.exists() {
                    remove_path(dest)?;
                }
                reflink_file(&src.join("data"), dest)?;
                set_path_readonly(dest)?;
            }
            RecordPayload::Dir { .. } => {
                reflink_tree(&src, dest)?;
                set_tree_readonly(dest)?;
            }
        }
        Ok(())
    }

    fn gc(&self, live: &[AssetKey]) -> Result<Reclaimed> {
        // Compute the set of live opaque content hashes (deps contribute nothing
        // to reclaim in v1 and are silently ignored here, not an error).
        let mut live_hashes: BTreeSet<Hash> = BTreeSet::new();
        for key in live {
            if let AssetKind::Opaque { content_hash } = &key.kind {
                live_hashes.insert(*content_hash);
            }
        }

        // Union the CAS refs of every live record => the keep set.
        let mut keep_objects: BTreeSet<Hash> = BTreeSet::new();
        for h in &live_hashes {
            if let Some(record) = self.load_record(h)? {
                for r in &record.refs {
                    keep_objects.insert(*r);
                }
            }
        }

        let mut reclaimed = Reclaimed::default();

        // 1) Delete dead records and their canonical payloads.
        let records = self.records_dir();
        if let Ok(rd) = fs::read_dir(&records) {
            for entry in rd {
                let entry = entry.map_err(|e| AssetError::io(&records, e))?;
                let fname = entry.file_name();
                let name = match fname.to_str() {
                    Some(n) if !n.starts_with('.') => n.to_string(),
                    _ => continue,
                };
                let hash = match Hash::from_hex(&name) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                if live_hashes.contains(&hash) {
                    continue;
                }
                // Dead asset: remove its record and its canonical payload.
                let rec_path = entry.path();
                let freed = file_len(&rec_path);
                remove_path(&rec_path)?;
                reclaimed.objects += 1;
                reclaimed.bytes += freed;

                let payload = self.payload_dir(&hash);
                if payload.exists() {
                    reclaimed.bytes += dir_size(&payload);
                    remove_path(&payload)?;
                }
            }
        }

        // 2) Delete CAS loose objects not in the keep set.
        let cas_reclaimed = self.gc_objects(&keep_objects)?;
        reclaimed.objects += cas_reclaimed.objects;
        reclaimed.bytes += cas_reclaimed.bytes;

        Ok(reclaimed)
    }
}

impl LocalCasAssetStore {
    /// Delete every loose CAS object whose hash is not in `keep`, counting what
    /// was reclaimed.
    ///
    /// The asset store owns its `objects/` directory exclusively, so walking the
    /// `<aa>/<rest>` fan-out and deleting unreferenced files is safe. Packed
    /// objects (if any) are not deleted here — the v1 asset store never repacks,
    /// so all objects are loose; this keeps GC simple and correct for the F0
    /// implementation.
    fn gc_objects(&self, keep: &BTreeSet<Hash>) -> Result<Reclaimed> {
        let mut reclaimed = Reclaimed::default();
        let objects_dir = self.objects.backend().objects_dir().to_path_buf();
        let top = match fs::read_dir(&objects_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(reclaimed),
            Err(e) => return Err(AssetError::io(&objects_dir, e)),
        };
        for entry in top {
            let entry = entry.map_err(|e| AssetError::io(&objects_dir, e))?;
            let prefix_path = entry.path();
            let ftype = entry
                .file_type()
                .map_err(|e| AssetError::io(&prefix_path, e))?;
            if !ftype.is_dir() {
                continue;
            }
            let prefix = match entry.file_name().to_str() {
                Some(name) if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    name.to_string()
                }
                // Skip `pack/` and anything that is not a fan-out directory.
                _ => continue,
            };
            let sub = fs::read_dir(&prefix_path).map_err(|e| AssetError::io(&prefix_path, e))?;
            for file in sub {
                let file = file.map_err(|e| AssetError::io(&prefix_path, e))?;
                let fpath = file.path();
                let ft = file.file_type().map_err(|e| AssetError::io(&fpath, e))?;
                if !ft.is_file() {
                    continue;
                }
                let rest = match file.file_name().to_str() {
                    Some(name)
                        if name.len() == 62 && name.bytes().all(|b| b.is_ascii_hexdigit()) =>
                    {
                        name.to_string()
                    }
                    _ => continue,
                };
                let hex = format!("{prefix}{rest}");
                let hash = match Hash::from_hex(&hex) {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                if keep.contains(&hash) {
                    continue;
                }
                let freed = file_len(&fpath);
                remove_path(&fpath)?;
                reclaimed.objects += 1;
                reclaimed.bytes += freed;
            }
        }
        Ok(reclaimed)
    }
}

/// Compute the stable content hash of an opaque asset from its record payload.
///
/// The hash binds the kind discriminator and the root CAS id (and a file's mode)
/// into a single BLAKE3 digest, so it is invariant to chunking details and stable
/// across machines.
///
/// The identity object is reduced to bytes through the **frozen canonical
/// encoder** ([`spork_canon`]) — the same path every other identity-bearing Spork
/// object (blob, tree, snapshot, ignore profile) uses — rather than raw
/// `serde_json`. This keeps a single, byte-stable definition of "how a structured
/// object becomes an address" for the whole workspace: key order, integer
/// formatting, and string escaping are pinned by the encoder rather than left to
/// `serde_json`'s defaults, so this `content_hash` (which is persisted as a
/// record filename and travels in a serialized [`AssetKey`](crate::AssetKey))
/// reproduces bit-for-bit on any machine and any future toolchain.
fn content_hash_of(payload: &RecordPayload) -> Hash {
    let identity = match payload {
        RecordPayload::File { blob, mode } => {
            serde_json::json!({ "kind": "file", "root": blob.to_hex(), "mode": mode })
        }
        RecordPayload::Dir { tree } => {
            serde_json::json!({ "kind": "dir", "root": tree.to_hex() })
        }
    };
    // The identity object is integers/strings only (no floats), so canonicalizing
    // it cannot fail; route it through the frozen encoder so the address is the
    // canonical bytes, not serde_json's incidental output.
    let bytes = spork_canon::canonicalize_value(&identity)
        .expect("asset identity is integer/string-only and always canonicalizes");
    spork_hash::hash_bytes(&bytes)
}

/// Build a matcher that ignores nothing (opaque assets are captured verbatim,
/// not subject to the deps-excluded code policy).
fn permissive_matcher() -> spork_ignore::IgnoreMatcher {
    // An empty pattern set matches nothing; `from_patterns` over no patterns is
    // infallible (there is nothing to compile), so the expect never fires.
    let empty = spork_ignore::IgnoreProfile::from_patterns(std::iter::empty::<String>())
        .expect("an empty ignore profile always compiles");
    spork_ignore::IgnoreMatcher::new(&empty)
}

/// The permission mode of a file (Unix), or a conventional value elsewhere.
#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode()
}

/// Non-Unix conventional mode.
#[cfg(not(unix))]
fn mode_of(_meta: &fs::Metadata) -> u32 {
    0o644
}

/// Reflink a single file from `src` to `dst`, falling back to a copy.
fn reflink_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| AssetError::io(parent, e))?;
    }
    // `reflink_or_copy` returns Ok(None) when it reflinked, Ok(Some(n)) when it
    // had to copy `n` bytes. Either way the destination now holds the bytes.
    reflink_copy::reflink_or_copy(src, dst).map_err(|e| AssetError::io(dst, e))?;
    Ok(())
}

/// Reflink an entire directory tree from `src` to `dst` (recreating structure,
/// reflinking each file, copying where reflink is unsupported).
fn reflink_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(|e| AssetError::io(dst, e))?;
    for entry in fs::read_dir(src).map_err(|e| AssetError::io(src, e))? {
        let entry = entry.map_err(|e| AssetError::io(src, e))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ftype = entry.file_type().map_err(|e| AssetError::io(&from, e))?;
        if ftype.is_dir() {
            reflink_tree(&from, &to)?;
        } else if ftype.is_symlink() {
            // Recreate symlinks rather than reflinking their target bytes.
            copy_symlink(&from, &to)?;
        } else {
            reflink_file(&from, &to)?;
        }
    }
    Ok(())
}

/// Recreate a symlink at `to` pointing where `from` points.
#[cfg(unix)]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    let target = fs::read_link(from).map_err(|e| AssetError::io(from, e))?;
    let _ = fs::remove_file(to);
    std::os::unix::fs::symlink(&target, to).map_err(|e| AssetError::io(to, e))
}

/// On non-Unix platforms, fall back to copying the link's resolved bytes.
#[cfg(not(unix))]
fn copy_symlink(from: &Path, to: &Path) -> Result<()> {
    reflink_file(from, to)
}

/// Mark a single path read-only (best-effort across platforms).
fn set_path_readonly(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|e| AssetError::io(path, e))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let mut perms = meta.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms).map_err(|e| AssetError::io(path, e))
}

/// Recursively mark a tree read-only. Files first, then their containing
/// directories, so write permission is not needed to descend after a parent is
/// frozen.
fn set_tree_readonly(root: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(root).map_err(|e| AssetError::io(root, e))?;
    if meta.is_dir() {
        for entry in fs::read_dir(root).map_err(|e| AssetError::io(root, e))? {
            let entry = entry.map_err(|e| AssetError::io(root, e))?;
            set_tree_readonly(&entry.path())?;
        }
        set_path_readonly(root)
    } else {
        set_path_readonly(root)
    }
}

/// Remove a path (file, symlink, or directory tree), restoring write permission
/// first so a read-only payload can still be reclaimed.
fn remove_path(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AssetError::io(path, e)),
    };
    if meta.file_type().is_symlink() {
        return fs::remove_file(path).map_err(|e| AssetError::io(path, e));
    }
    if meta.is_dir() {
        // Restore write on the dir so we can delete its children, recurse, then
        // remove it.
        clear_readonly(path);
        for entry in fs::read_dir(path).map_err(|e| AssetError::io(path, e))? {
            let entry = entry.map_err(|e| AssetError::io(path, e))?;
            remove_path(&entry.path())?;
        }
        fs::remove_dir(path).map_err(|e| AssetError::io(path, e))
    } else {
        clear_readonly(path);
        fs::remove_file(path).map_err(|e| AssetError::io(path, e))
    }
}

/// Best-effort: restore owner write on a path so a read-only payload can be
/// deleted.
///
/// On Unix this adds only the owner-write bit to the existing mode (rather than
/// `set_readonly(false)`, which would make the file world-writable); elsewhere it
/// clears the read-only flag.
#[cfg(unix)]
fn clear_readonly(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_symlink() {
            let mode = meta.permissions().mode();
            let perms = fs::Permissions::from_mode(mode | 0o200);
            let _ = fs::set_permissions(path, perms);
        }
    }
}

/// Non-Unix: clear the read-only flag so the path can be deleted.
#[cfg(not(unix))]
fn clear_readonly(path: &Path) {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.file_type().is_symlink() {
            let mut perms = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            let _ = fs::set_permissions(path, perms);
        }
    }
}

/// The size of a regular file in bytes, or 0 if it cannot be stat'd.
fn file_len(path: &Path) -> u64 {
    fs::symlink_metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// The total size of all regular files under `dir` (best-effort).
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return 0,
    };
    for entry in rd.flatten() {
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => total += dir_size(&path),
            Ok(m) if m.is_file() => total += m.len(),
            _ => {}
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Open a store under a temp dir; returns the dir guard and the store.
    fn store() -> (TempDir, LocalCasAssetStore) {
        let tmp = TempDir::new().unwrap();
        let s = LocalCasAssetStore::open(tmp.path().join("assets")).unwrap();
        (tmp, s)
    }

    #[test]
    fn opaque_file_ensure_materialize_round_trips_read_only() {
        let (tmp, s) = store();
        let src = tmp.path().join("weights.bin");
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 256) as u8).collect();
        fs::write(&src, &data).unwrap();

        let ch = s.content_hash_for_source(&src).unwrap();
        let key = AssetKey::opaque(ch);
        let aref = s.ensure(&key, Some(&src)).unwrap();
        assert_eq!(aref.stored, ch);
        assert_eq!(aref.key, key);

        let dest = tmp.path().join("out/weights.bin");
        s.materialize(&key, &dest).unwrap();

        // Byte-identical.
        assert_eq!(fs::read(&dest).unwrap(), data);
        // Read-only.
        assert!(fs::metadata(&dest).unwrap().permissions().readonly());
    }

    #[test]
    fn opaque_dir_ensure_materialize_round_trips_read_only() {
        let (tmp, s) = store();
        let src = tmp.path().join("dataset");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("a.txt"), b"alpha").unwrap();
        fs::write(src.join("nested/b.bin"), vec![7u8; 2048]).unwrap();

        let ch = s.content_hash_for_source(&src).unwrap();
        let key = AssetKey::opaque(ch);
        s.ensure(&key, Some(&src)).unwrap();

        let dest = tmp.path().join("restored");
        s.materialize(&key, &dest).unwrap();

        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(
            fs::read(dest.join("nested/b.bin")).unwrap(),
            vec![7u8; 2048]
        );
        // Files and dirs are read-only.
        assert!(fs::metadata(dest.join("a.txt"))
            .unwrap()
            .permissions()
            .readonly());
        assert!(fs::metadata(dest.join("nested/b.bin"))
            .unwrap()
            .permissions()
            .readonly());
    }

    #[test]
    fn ensure_is_idempotent_and_dedups() {
        let (tmp, s) = store();
        let src = tmp.path().join("blob.bin");
        fs::write(&src, vec![3u8; 8192]).unwrap();
        let ch = s.content_hash_for_source(&src).unwrap();
        let key = AssetKey::opaque(ch);

        let r1 = s.ensure(&key, Some(&src)).unwrap();
        let r2 = s.ensure(&key, Some(&src)).unwrap();
        assert_eq!(r1, r2);

        // A second ensure with no source also works (already cached).
        let r3 = s.ensure(&key, None).unwrap();
        assert_eq!(r3.stored, ch);
    }

    #[test]
    fn ensure_without_source_for_uncached_is_error() {
        let (_tmp, s) = store();
        let key = AssetKey::opaque(Hash::from_bytes([1u8; 32]));
        let err = s.ensure(&key, None).unwrap_err();
        assert!(matches!(err, AssetError::SourceRequired(_)));
    }

    #[test]
    fn content_mismatch_is_rejected() {
        let (tmp, s) = store();
        let src = tmp.path().join("x.bin");
        fs::write(&src, b"actual bytes").unwrap();
        // Declare a wrong content hash.
        let wrong = Hash::from_bytes([0xAB; 32]);
        let key = AssetKey::opaque(wrong);
        let err = s.ensure(&key, Some(&src)).unwrap_err();
        match err {
            AssetError::ContentMismatch { expected, .. } => assert_eq!(expected, wrong),
            other => panic!("expected ContentMismatch, got {other:?}"),
        }
    }

    #[test]
    fn deps_key_denies_by_default_on_ensure_and_materialize() {
        let (tmp, s) = store();
        let key = AssetKey::deps("npm", Hash::from_bytes([5u8; 32]), "aarch64-apple-darwin");
        match s.ensure(&key, None).unwrap_err() {
            AssetError::EcosystemNotRegistered(eco) => assert_eq!(eco, "npm"),
            other => panic!("expected EcosystemNotRegistered, got {other:?}"),
        }
        let dest = tmp.path().join("deps_out");
        match s.materialize(&key, &dest).unwrap_err() {
            AssetError::EcosystemNotRegistered(eco) => assert_eq!(eco, "npm"),
            other => panic!("expected EcosystemNotRegistered, got {other:?}"),
        }
        // Different ecosystem name is reported faithfully.
        let pip = AssetKey::deps(
            "pip",
            Hash::from_bytes([6u8; 32]),
            "x86_64-unknown-linux-gnu",
        );
        match s.ensure(&pip, None).unwrap_err() {
            AssetError::EcosystemNotRegistered(eco) => assert_eq!(eco, "pip"),
            other => panic!("expected EcosystemNotRegistered, got {other:?}"),
        }
    }

    #[test]
    fn gc_keeps_live_and_reclaims_dead() {
        let (tmp, s) = store();

        // Two distinct opaque assets.
        let src_a = tmp.path().join("a.bin");
        let src_b = tmp.path().join("b.bin");
        fs::write(&src_a, vec![1u8; 4096]).unwrap();
        fs::write(&src_b, vec![2u8; 4096]).unwrap();
        let ka = AssetKey::opaque(s.content_hash_for_source(&src_a).unwrap());
        let kb = AssetKey::opaque(s.content_hash_for_source(&src_b).unwrap());
        s.ensure(&ka, Some(&src_a)).unwrap();
        s.ensure(&kb, Some(&src_b)).unwrap();

        // GC keeping only A: B's record, payload, and CAS objects are reclaimed.
        let reclaimed = s.gc(std::slice::from_ref(&ka)).unwrap();
        assert!(reclaimed.objects > 0, "expected to reclaim B's objects");
        assert!(reclaimed.bytes > 0);

        // A is still materializable; B is gone.
        let dest_a = tmp.path().join("out_a.bin");
        s.materialize(&ka, &dest_a).unwrap();
        assert_eq!(fs::read(&dest_a).unwrap(), vec![1u8; 4096]);

        let err = s
            .materialize(&kb, &tmp.path().join("out_b.bin"))
            .unwrap_err();
        assert!(matches!(err, AssetError::SourceRequired(_)));
    }

    #[test]
    fn gc_with_all_live_reclaims_nothing() {
        let (tmp, s) = store();
        let src = tmp.path().join("c.bin");
        fs::write(&src, vec![9u8; 2048]).unwrap();
        let k = AssetKey::opaque(s.content_hash_for_source(&src).unwrap());
        s.ensure(&k, Some(&src)).unwrap();

        let reclaimed = s.gc(std::slice::from_ref(&k)).unwrap();
        assert_eq!(reclaimed, Reclaimed::default());
        // Still materializable.
        let dest = tmp.path().join("out_c.bin");
        s.materialize(&k, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), vec![9u8; 2048]);
    }

    #[test]
    fn gc_with_empty_live_reclaims_everything() {
        let (tmp, s) = store();
        let src = tmp.path().join("d.bin");
        fs::write(&src, vec![4u8; 1024]).unwrap();
        let k = AssetKey::opaque(s.content_hash_for_source(&src).unwrap());
        s.ensure(&k, Some(&src)).unwrap();

        let reclaimed = s.gc(&[]).unwrap();
        assert!(reclaimed.objects > 0);
        assert!(matches!(
            s.materialize(&k, &tmp.path().join("nope")),
            Err(AssetError::SourceRequired(_))
        ));
    }

    #[test]
    fn gc_ignores_live_deps_keys() {
        let (tmp, s) = store();
        let src = tmp.path().join("e.bin");
        fs::write(&src, vec![8u8; 512]).unwrap();
        let opaque = AssetKey::opaque(s.content_hash_for_source(&src).unwrap());
        s.ensure(&opaque, Some(&src)).unwrap();

        // A live set containing a deps key (which stores nothing) plus the live
        // opaque key: GC keeps the opaque asset, ignores the deps key, reclaims
        // nothing.
        let deps = AssetKey::deps("cargo", Hash::from_bytes([1u8; 32]), "any");
        let reclaimed = s.gc(&[deps, opaque.clone()]).unwrap();
        assert_eq!(reclaimed, Reclaimed::default());
        let dest = tmp.path().join("out_e.bin");
        s.materialize(&opaque, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), vec![8u8; 512]);
    }

    #[test]
    fn identical_content_dedups_to_same_key() {
        let (tmp, s) = store();
        let a = tmp.path().join("one.bin");
        let b = tmp.path().join("two.bin");
        let data = vec![42u8; 6000];
        fs::write(&a, &data).unwrap();
        fs::write(&b, &data).unwrap();
        let ha = s.content_hash_for_source(&a).unwrap();
        let hb = s.content_hash_for_source(&b).unwrap();
        assert_eq!(ha, hb, "identical bytes must share a content hash");
    }

    #[test]
    fn reopen_store_finds_existing_assets() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("assets");
        let src = tmp.path().join("persist.bin");
        fs::write(&src, vec![5u8; 3000]).unwrap();

        let ch = {
            let s = LocalCasAssetStore::open(&root).unwrap();
            let ch = s.content_hash_for_source(&src).unwrap();
            s.ensure(&AssetKey::opaque(ch), Some(&src)).unwrap();
            ch
        };

        // A fresh handle over the same root materializes without re-ingesting.
        let s2 = LocalCasAssetStore::open(&root).unwrap();
        let key = AssetKey::opaque(ch);
        let dest = tmp.path().join("out_persist.bin");
        s2.materialize(&key, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), vec![5u8; 3000]);
    }
}
