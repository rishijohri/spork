//! The high-level [`ObjectStore`]: capture and materialize codebase state.
//!
//! Where [`crate::StorageBackend`] deals in opaque payload bytes, `ObjectStore`
//! speaks the object model (DESIGN.md §6.1, §10.1): it FastCDC-chunks files into
//! [`crate::Blob`]s, walks directories into [`crate::Tree`]s (honoring an
//! [`spork_ignore::IgnoreMatcher`]), binds a root tree into a
//! [`crate::Snapshot`], reassembles blobs back into bytes, and materializes a
//! tree to disk byte-for-byte. Every mutating call returns [`PutStats`] so the
//! dedup story is *measurable*: a warm re-capture writes zero new objects, and a
//! one-region edit to a large file writes exactly one new chunk.
//!
//! # How identity drives dedup
//!
//! Every object is addressed by the BLAKE3 of its canonical bytes (chunks by the
//! BLAKE3 of their raw bytes). The store checks [`StorageBackend::has`] before
//! every write, so:
//!
//! - **Idempotent capture** — re-putting unchanged content finds every object
//!   already present and writes nothing ([`PutStats::new_objects`] == 0).
//! - **Sub-file dedup** — an edit to one region of a big file re-chunks it, but
//!   only the chunk(s) overlapping the edit differ, so only those are new
//!   ([`PutStats::new_chunks`] counts them).
//! - **Subtree dedup** — two identical subdirectories encode to the same
//!   [`crate::Tree`] bytes and therefore the same id; the second is a pure reuse.
//!
//! Design references: DESIGN.md §6.1 (content identity), §10.1 (object model),
//! §10.4 (FastCDC sub-file dedup, ignore-aware walking), §10.5
//! (`ignore_profile_hash` in snapshot identity).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use spork_hash::{Hash, HashTag};
use spork_ignore::IgnoreMatcher;

use crate::backend::StorageBackend;
use crate::chunk::chunk_ranges;
use crate::error::{CasError, Result};
use crate::object::{Blob, EntryKind, ObjKind, Snapshot, Tree, TreeEntry};

/// Per-operation accounting of what a capture actually stored versus reused.
///
/// This is the evidence behind the dedup definition-of-done (DESIGN.md §10.4,
/// §14.5): a cold capture reports many `new_*`; a warm re-capture of identical
/// content reports all-`reused`, zero `new_*`. "Objects" counts the higher-level
/// objects (blobs, trees, snapshots); "chunks" counts the leaf chunk objects so
/// sub-file dedup is visible separately. `bytes_written` is the total *payload*
/// bytes newly persisted (it excludes object-header overhead and excludes reused
/// objects).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutStats {
    /// Higher-level objects (blobs/trees/snapshots) newly written this operation.
    pub new_objects: u64,
    /// Higher-level objects found already present (reused) this operation.
    pub reused_objects: u64,
    /// Leaf chunk objects newly written this operation.
    pub new_chunks: u64,
    /// Leaf chunk objects found already present (reused) this operation.
    pub reused_chunks: u64,
    /// Total payload bytes newly persisted (new chunks + new objects).
    pub bytes_written: u64,
}

impl PutStats {
    /// Fold another stats record into this one (used to aggregate a tree walk).
    fn merge(&mut self, other: PutStats) {
        self.new_objects += other.new_objects;
        self.reused_objects += other.reused_objects;
        self.new_chunks += other.new_chunks;
        self.reused_chunks += other.reused_chunks;
        self.bytes_written += other.bytes_written;
    }

    /// Total objects touched (new + reused, across both objects and chunks).
    #[must_use]
    pub fn total_touched(&self) -> u64 {
        self.new_objects + self.reused_objects + self.new_chunks + self.reused_chunks
    }
}

/// The high-level content-addressed object store, generic over a backend.
///
/// Wrap any [`StorageBackend`] (v1 ships [`crate::LooseStore`]). All capture
/// methods are idempotent and report [`PutStats`]; all read methods validate and
/// reassemble through the same backend.
#[derive(Debug, Clone)]
pub struct ObjectStore<S: StorageBackend> {
    backend: S,
}

impl<S: StorageBackend> ObjectStore<S> {
    /// Create an object store over `backend`.
    #[must_use]
    pub fn new(backend: S) -> Self {
        ObjectStore { backend }
    }

    /// Borrow the underlying storage backend.
    #[must_use]
    pub fn backend(&self) -> &S {
        &self.backend
    }

    /// Consume the store, returning its backend.
    #[must_use]
    pub fn into_backend(self) -> S {
        self.backend
    }

    /// FastCDC-chunk `bytes`, store the chunks, then store the [`Blob`] binding
    /// them; return the blob id and the stats for everything written.
    ///
    /// A file at or below the single-chunk threshold becomes one chunk; a larger
    /// file is content-defined-chunked so a localized edit reuses untouched
    /// chunks. The blob is itself an object, so identical files dedup at the blob
    /// level and identical regions dedup at the chunk level.
    ///
    /// This is the *incremental* path: a handful of new objects, written loose
    /// (one fsync each) for simplicity. Bulk capture ([`ObjectStore::put_tree`])
    /// uses a single batched durability barrier instead.
    ///
    /// # Errors
    /// Returns a [`CasError`] on backend I/O failure or canonical-encoding
    /// failure.
    pub fn put_blob_bytes(&self, bytes: &[u8]) -> Result<(Hash, PutStats)> {
        let mut stats = PutStats::default();
        let mut sink = LooseSink::new(&self.backend);
        let id = chunk_and_blob(bytes, &mut sink, &mut stats)?;
        Ok((id, stats))
    }

    /// Store a [`Snapshot`] binding a root tree to its ignore profile and an
    /// optional imported Git parent. Returns the snapshot id and stats.
    ///
    /// The snapshot is content-addressed like any object, so re-capturing the
    /// same state under the same ignore profile dedups to one snapshot. A lone
    /// snapshot is a single object, so it is written loose (one fsync).
    ///
    /// # Errors
    /// Returns a [`CasError`] on backend I/O or canonical-encoding failure.
    pub fn put_snapshot(
        &self,
        root_tree: Hash,
        ignore_profile_hash: Hash,
        git_parent: Option<String>,
    ) -> Result<(Hash, PutStats)> {
        let mut stats = PutStats::default();
        let mut sink = LooseSink::new(&self.backend);
        let snapshot = Snapshot::new(root_tree, ignore_profile_hash, git_parent);
        let bytes = snapshot.canonical_bytes()?;
        let id = sink.put_object(ObjKind::Snapshot, bytes, &mut stats)?;
        Ok((id, stats))
    }
}

impl<S: StorageBackend + Sync> ObjectStore<S> {
    /// Walk `dir` into a [`Tree`] (recursively), honoring `matcher`, and store
    /// every blob, chunk, and (sub)tree it contains.
    ///
    /// Two things make a cold capture of a large tree fast here:
    ///
    /// 1. **Parallel hashing.** The per-file work — read, FastCDC-chunk, and
    ///    BLAKE3-hash — is fanned across cores with `rayon`. Parallelism only
    ///    reorders *computation*: object identity is unchanged (a chunk/blob/tree
    ///    id is a pure function of its bytes), and a directory's tree entries are
    ///    re-sorted by name ([`Tree::new`]) so the canonical bytes — and thus the
    ///    snapshot hash — are byte-for-byte identical regardless of which file
    ///    finished hashing first.
    /// 2. **One durability barrier.** Rather than writing each new object as its
    ///    own fsync'd loose file, the walk *buffers* the new objects it produces
    ///    and commits them all with a single [`StorageBackend::put_batch`] at the
    ///    end (one fsync on the v1 [`crate::LooseStore`], which folds them into a
    ///    packfile). When this method returns, every reported new object is
    ///    durable — preserving the write-objects-then-log ordering F1 relies on.
    ///
    /// Dedup is preserved exactly: an object already present (loose, packed, or
    /// produced earlier in *this* capture) is reused and never re-buffered, so a
    /// warm re-capture writes zero new objects. Ignored entries (per `matcher`,
    /// evaluated on the path *relative to `dir`*) are skipped, so
    /// dependency/artifact trees never enter the store (DESIGN.md §10.4, §10.5).
    ///
    /// Returns the root tree id and aggregate [`PutStats`].
    ///
    /// # Errors
    /// Returns [`CasError::Io`] on a directory-read failure,
    /// [`CasError::UnrepresentableEntry`] for a non-UTF-8 name or an
    /// unsupported entry type, plus the errors of the blob/object writers.
    pub fn put_tree(&self, dir: &Path, matcher: &IgnoreMatcher) -> Result<(Hash, PutStats)> {
        let capture = Capture::new(&self.backend);
        let id = self.put_tree_rec(dir, Path::new(""), matcher, &capture)?;
        let stats = capture.commit()?;
        Ok((id, stats))
    }

    /// Recursive, parallel worker for [`ObjectStore::put_tree`].
    ///
    /// `abs` is the absolute (or process-relative) directory being read; `rel` is
    /// its path relative to the snapshot root, used for ignore matching. The
    /// directory's kept children are processed in parallel (files are read +
    /// chunked + hashed concurrently; subdirectories recurse, themselves
    /// parallel), and the resulting entries are gathered into a `BTreeMap` keyed
    /// by name so the tree's entries are deterministically ordered regardless of
    /// completion order — the property that keeps a given input's snapshot hash
    /// stable across runs and across this change.
    fn put_tree_rec(
        &self,
        abs: &Path,
        rel: &Path,
        matcher: &IgnoreMatcher,
        capture: &Capture<'_, S>,
    ) -> Result<Hash> {
        // Collect the directory's kept children first (a cheap, serial pass), so
        // the expensive per-child work can be parallelized below.
        let mut kept: Vec<KeptEntry> = Vec::new();
        let rd = std::fs::read_dir(abs).map_err(|e| CasError::io(abs, e))?;
        for entry in rd {
            let entry = entry.map_err(|e| CasError::io(abs, e))?;
            let name_os = entry.file_name();
            let name = name_os.to_str().ok_or_else(|| {
                CasError::UnrepresentableEntry(format!(
                    "non-UTF-8 file name in {abs:?}: {name_os:?}"
                ))
            })?;
            let child_abs = entry.path();
            let child_rel = rel.join(name);

            // `symlink_metadata` does not follow links, so a symlink is reported
            // as a symlink (not its target's kind).
            let meta =
                std::fs::symlink_metadata(&child_abs).map_err(|e| CasError::io(&child_abs, e))?;
            let is_dir = meta.is_dir();

            if matcher.is_ignored(&child_rel, is_dir) {
                continue;
            }
            kept.push(KeptEntry {
                name: name.to_string(),
                rel: child_rel,
                abs: child_abs,
                meta,
            });
        }

        // Process the kept children in parallel. Each produces a `(name,
        // TreeEntry)`; subdirectory recursion runs inside this same parallel
        // region, so the whole tree is walked across all cores. Errors propagate;
        // ordering of completion is irrelevant because we re-key by name.
        use rayon::prelude::*;
        let built: Vec<Result<(String, TreeEntry)>> = kept
            .par_iter()
            .map(|k| {
                let entry = self.build_entry(k, matcher, capture)?;
                Ok((k.name.clone(), entry))
            })
            .collect();

        let mut entries: BTreeMap<String, TreeEntry> = BTreeMap::new();
        for item in built {
            let (name, entry) = item?;
            entries.insert(name, entry);
        }

        let tree = Tree::new(entries.into_values().collect());
        let tree_bytes = tree.canonical_bytes()?;
        capture.put_object(ObjKind::Tree, tree_bytes)
    }

    /// Build the [`TreeEntry`] for one kept child: a symlink, a recursed
    /// subdirectory, or a chunked + stored file.
    fn build_entry(
        &self,
        k: &KeptEntry,
        matcher: &IgnoreMatcher,
        capture: &Capture<'_, S>,
    ) -> Result<TreeEntry> {
        if k.meta.file_type().is_symlink() {
            symlink_entry(&k.name, &k.abs, capture)
        } else if k.meta.is_dir() {
            let child_tree = self.put_tree_rec(&k.abs, &k.rel, matcher, capture)?;
            Ok(TreeEntry {
                name: k.name.clone(),
                kind: EntryKind::Dir,
                mode: mode_of(&k.meta),
                target: child_tree,
            })
        } else if k.meta.is_file() {
            let bytes = std::fs::read(&k.abs).map_err(|e| CasError::io(&k.abs, e))?;
            let blob = capture.put_file_bytes(&bytes)?;
            Ok(TreeEntry {
                name: k.name.clone(),
                kind: EntryKind::File,
                mode: mode_of(&k.meta),
                target: blob,
            })
        } else {
            // Sockets, fifos, block/char devices have no place in a code
            // snapshot; refuse rather than silently dropping them.
            Err(CasError::UnrepresentableEntry(format!(
                "unsupported file type at {:?}",
                k.abs
            )))
        }
    }

    /// Convenience: capture a directory into a full snapshot in one call.
    ///
    /// Walks `dir` with `matcher` (whose profile hash is `ignore_profile_hash`),
    /// stores the tree under a single batched durability barrier, then stores a
    /// snapshot over it. The returned stats aggregate the whole capture (tree +
    /// snapshot). This is what the CLI's `put-tree` builds on.
    ///
    /// # Errors
    /// Returns the union of [`ObjectStore::put_tree`] and
    /// [`ObjectStore::put_snapshot`] errors.
    pub fn capture_snapshot(
        &self,
        dir: &Path,
        matcher: &IgnoreMatcher,
        ignore_profile_hash: Hash,
        git_parent: Option<String>,
    ) -> Result<(Hash, Hash, PutStats)> {
        let (root_tree, mut stats) = self.put_tree(dir, matcher)?;
        let (snap, snap_stats) = self.put_snapshot(root_tree, ignore_profile_hash, git_parent)?;
        stats.merge(snap_stats);
        Ok((snap, root_tree, stats))
    }

    /// Fetch and decode a stored payload of an expected kind.
    fn read_payload(&self, id: &Hash) -> Result<Vec<u8>> {
        self.backend.get(id)?.ok_or(CasError::NotFound(*id))
    }

    /// Reassemble a [`Blob`] back into the original file bytes.
    ///
    /// Reads the blob object, then concatenates its chunks in order. The result
    /// is byte-identical to the input that produced the blob.
    ///
    /// # Errors
    /// - [`CasError::NotFound`] — the blob or one of its chunks is absent.
    /// - [`CasError::Decode`] — the blob payload does not parse.
    pub fn read_blob(&self, id: &Hash) -> Result<Vec<u8>> {
        let bytes = self.read_payload(id)?;
        let blob: Blob = serde_json::from_slice(&bytes)
            .map_err(|e| CasError::Decode(format!("blob {id}: {e}")))?;
        if blob.chunks.is_empty() {
            return Err(CasError::Decode(format!("blob {id} has no chunks")));
        }
        let mut out = Vec::new();
        for chunk_id in &blob.chunks {
            let chunk = self
                .backend
                .get(chunk_id)?
                .ok_or(CasError::NotFound(*chunk_id))?;
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// Read and decode a [`Tree`].
    ///
    /// # Errors
    /// [`CasError::NotFound`] if absent; [`CasError::Decode`] if the payload does
    /// not parse as a tree.
    pub fn read_tree(&self, id: &Hash) -> Result<Tree> {
        let bytes = self.read_payload(id)?;
        serde_json::from_slice(&bytes).map_err(|e| CasError::Decode(format!("tree {id}: {e}")))
    }

    /// Read and decode a [`Snapshot`].
    ///
    /// # Errors
    /// [`CasError::NotFound`] if absent; [`CasError::Decode`] if the payload does
    /// not parse as a snapshot.
    pub fn read_snapshot(&self, id: &Hash) -> Result<Snapshot> {
        let bytes = self.read_payload(id)?;
        serde_json::from_slice(&bytes).map_err(|e| CasError::Decode(format!("snapshot {id}: {e}")))
    }

    /// Materialize the tree `id` to `dest`, writing every file byte-identically.
    ///
    /// Creates `dest` (and any subdirectories) as needed and reconstructs each
    /// entry: files from their reassembled blobs, directories recursively, and
    /// symlinks from their stored target text. On Unix, file permission bits from
    /// the tree entry's `mode` are restored. The result reproduces the captured
    /// tree exactly for all non-ignored entries.
    ///
    /// # Errors
    /// Returns [`CasError::Io`] on a write failure and the read errors of the
    /// referenced objects.
    pub fn materialize_tree(&self, id: &Hash, dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest).map_err(|e| CasError::io(dest, e))?;
        let tree = self.read_tree(id)?;
        for entry in &tree.entries {
            let child = dest.join(&entry.name);
            match entry.kind {
                EntryKind::Dir => {
                    self.materialize_tree(&entry.target, &child)?;
                    set_mode(&child, entry.mode);
                }
                EntryKind::File => {
                    let bytes = self.read_blob(&entry.target)?;
                    std::fs::write(&child, &bytes).map_err(|e| CasError::io(&child, e))?;
                    set_mode(&child, entry.mode);
                }
                EntryKind::Symlink => {
                    let target_bytes = self.read_blob(&entry.target)?;
                    materialize_symlink(&target_bytes, &child)?;
                }
            }
        }
        Ok(())
    }

    /// Materialize the root tree of a snapshot to `dest`.
    ///
    /// # Errors
    /// As [`ObjectStore::materialize_tree`], plus snapshot-read errors.
    pub fn materialize_snapshot(&self, id: &Hash, dest: &Path) -> Result<()> {
        let snapshot = self.read_snapshot(id)?;
        self.materialize_tree(&snapshot.root_tree, dest)
    }
}

/// A kept (non-ignored) directory child, collected before the parallel pass so
/// the expensive per-child work (read/chunk/hash, or recursion) can be fanned
/// across cores without holding a `read_dir` iterator open.
struct KeptEntry {
    /// The entry's name (a single path component).
    name: String,
    /// Its path relative to the snapshot root (for ignore matching of children).
    rel: std::path::PathBuf,
    /// Its absolute (or process-relative) path on disk.
    abs: std::path::PathBuf,
    /// Its `symlink_metadata` (does not follow links).
    meta: std::fs::Metadata,
}

/// The buffered, single-fsync capture context shared across the parallel tree
/// walk.
///
/// Every new object the walk produces (chunk, blob, or tree) is *staged* here
/// instead of being written immediately, and the accumulated set is flushed with
/// a single [`StorageBackend::put_batch`] in [`Capture::commit`]. An object
/// already present (loose, packed, or staged earlier in this same capture) is
/// counted as reused and never re-buffered, so dedup — and the warm-recapture
/// "zero new objects" guarantee — is preserved exactly.
///
/// The internal state is behind a `Mutex` so the rayon-parallel walk can stage
/// from many threads at once. Hashing and chunking happen *outside* the lock; the
/// lock is held only for the brief presence-check-and-insert.
struct Capture<'a, S: StorageBackend> {
    backend: &'a S,
    state: std::sync::Mutex<CaptureState>,
}

/// The mutable inner state of a [`Capture`]: the pending batch, the digests
/// already accounted for, and the running stats.
struct CaptureState {
    /// New objects to commit in one batch: `(tag, kind, payload)`.
    pending: Vec<(HashTag, ObjKind, Vec<u8>)>,
    /// Digests already seen this capture (staged or found present), so a repeat
    /// is recognized as reuse and never buffered twice.
    seen: std::collections::HashSet<Hash>,
    /// Running dedup accounting for the whole capture.
    stats: PutStats,
}

impl<'a, S: StorageBackend> Capture<'a, S> {
    /// Create an empty capture context over `backend`.
    fn new(backend: &'a S) -> Self {
        Capture {
            backend,
            state: std::sync::Mutex::new(CaptureState {
                pending: Vec::new(),
                seen: std::collections::HashSet::new(),
                stats: PutStats::default(),
            }),
        }
    }

    /// Stage one already-canonicalized object (its `bytes` is the exact payload
    /// whose BLAKE3 is its id), classifying it as a new or reused chunk/object.
    ///
    /// Returns the object's digest. Idempotent within and across captures: a
    /// digest already on disk or already staged is counted as reused and not
    /// buffered again.
    fn stage(&self, kind: ObjKind, bytes: Vec<u8>, is_chunk: bool) -> Result<Hash> {
        let hash = spork_hash::hash_bytes(&bytes);
        let mut st = self.state.lock().expect("capture mutex poisoned");
        if st.seen.contains(&hash) {
            // Already accounted for earlier in this capture.
            if is_chunk {
                st.stats.reused_chunks += 1;
            } else {
                st.stats.reused_objects += 1;
            }
            return Ok(hash);
        }
        // Presence check against the backend can do I/O; do it under the lock so
        // the seen-set stays authoritative (the cost is dwarfed by hashing).
        if self.backend.has(&hash)? {
            st.seen.insert(hash);
            if is_chunk {
                st.stats.reused_chunks += 1;
            } else {
                st.stats.reused_objects += 1;
            }
            return Ok(hash);
        }
        st.seen.insert(hash);
        if is_chunk {
            st.stats.new_chunks += 1;
        } else {
            st.stats.new_objects += 1;
        }
        st.stats.bytes_written += bytes.len() as u64;
        st.pending.push((HashTag::CURRENT, kind, bytes));
        Ok(hash)
    }

    /// Stage a higher-level (blob/tree/snapshot) object.
    fn put_object(&self, kind: ObjKind, bytes: Vec<u8>) -> Result<Hash> {
        self.stage(kind, bytes, false)
    }

    /// Stage one leaf chunk's raw bytes.
    fn put_chunk(&self, bytes: Vec<u8>) -> Result<Hash> {
        self.stage(ObjKind::Chunk, bytes, true)
    }

    /// Chunk + stage a file's bytes as a [`Blob`], returning the blob id.
    fn put_file_bytes(&self, bytes: &[u8]) -> Result<Hash> {
        let mut chunk_hashes = Vec::new();
        for range in chunk_ranges(bytes) {
            let chunk = bytes[range.start..range.end].to_vec();
            chunk_hashes.push(self.put_chunk(chunk)?);
        }
        let blob = Blob::new(chunk_hashes)?;
        let blob_bytes = blob.canonical_bytes()?;
        self.put_object(ObjKind::Blob, blob_bytes)
    }

    /// Flush all staged objects with a **single durability barrier** and return
    /// the capture's aggregate stats.
    ///
    /// When this returns `Ok`, every new object the capture produced is durable
    /// on disk (DESIGN.md §10.4 write-objects-then-log ordering): the caller may
    /// safely treat the reported ids as committed. An empty batch is a no-op.
    fn commit(self) -> Result<PutStats> {
        let state = self.state.into_inner().expect("capture mutex poisoned");
        if !state.pending.is_empty() {
            self.backend.put_batch(&state.pending)?;
        }
        Ok(state.stats)
    }
}

/// A trait abstracting "stage/store an object" so the chunk+blob logic is shared
/// between the buffered bulk path ([`Capture`]) and the immediate loose path
/// ([`LooseSink`]).
trait ObjectSink {
    /// Store/stage a leaf chunk, returning its digest.
    fn sink_chunk(&mut self, bytes: Vec<u8>, stats: &mut PutStats) -> Result<Hash>;
    /// Store/stage a higher-level object, returning its digest.
    fn sink_object(&mut self, kind: ObjKind, bytes: Vec<u8>, stats: &mut PutStats) -> Result<Hash>;
}

/// The immediate, per-object loose write path used by small/incremental puts
/// ([`ObjectStore::put_blob_bytes`], [`ObjectStore::put_snapshot`]).
///
/// Each new object is written as its own atomic, fsync'd loose file — simple and
/// crash-safe, and cheap enough for the handful of objects an incremental put
/// touches. (Bulk capture uses [`Capture`]'s single batched fsync instead.)
struct LooseSink<'a, S: StorageBackend> {
    backend: &'a S,
}

impl<'a, S: StorageBackend> LooseSink<'a, S> {
    fn new(backend: &'a S) -> Self {
        LooseSink { backend }
    }

    /// Store a higher-level object immediately, counting new vs reused.
    fn put_object(&mut self, kind: ObjKind, bytes: Vec<u8>, stats: &mut PutStats) -> Result<Hash> {
        self.sink_object(kind, bytes, stats)
    }
}

impl<S: StorageBackend> ObjectSink for LooseSink<'_, S> {
    fn sink_chunk(&mut self, bytes: Vec<u8>, stats: &mut PutStats) -> Result<Hash> {
        let hash = spork_hash::hash_bytes(&bytes);
        if self.backend.has(&hash)? {
            stats.reused_chunks += 1;
            return Ok(hash);
        }
        let stored = self.backend.put(HashTag::CURRENT, ObjKind::Chunk, &bytes)?;
        debug_assert_eq!(stored, hash);
        stats.new_chunks += 1;
        stats.bytes_written += bytes.len() as u64;
        Ok(hash)
    }

    fn sink_object(&mut self, kind: ObjKind, bytes: Vec<u8>, stats: &mut PutStats) -> Result<Hash> {
        let hash = spork_hash::hash_bytes(&bytes);
        if self.backend.has(&hash)? {
            stats.reused_objects += 1;
            return Ok(hash);
        }
        let stored = self.backend.put(HashTag::CURRENT, kind, &bytes)?;
        debug_assert_eq!(stored, hash);
        stats.new_objects += 1;
        stats.bytes_written += bytes.len() as u64;
        Ok(hash)
    }
}

/// FastCDC-chunk `bytes` into a [`Blob`] through a [`LooseSink`], returning the
/// blob id. Shared by the incremental blob path.
fn chunk_and_blob<S: StorageBackend>(
    bytes: &[u8],
    sink: &mut LooseSink<'_, S>,
    stats: &mut PutStats,
) -> Result<Hash> {
    let mut chunk_hashes = Vec::new();
    for range in chunk_ranges(bytes) {
        let chunk = bytes[range.start..range.end].to_vec();
        chunk_hashes.push(sink.sink_chunk(chunk, stats)?);
    }
    let blob = Blob::new(chunk_hashes)?;
    let blob_bytes = blob.canonical_bytes()?;
    sink.sink_object(ObjKind::Blob, blob_bytes, stats)
}

/// Build a symlink [`TreeEntry`] by staging the link's target-path text as a
/// blob through the capture context.
#[cfg(unix)]
fn symlink_entry<S: StorageBackend>(
    name: &str,
    child_abs: &Path,
    capture: &Capture<'_, S>,
) -> Result<TreeEntry> {
    let target = std::fs::read_link(child_abs).map_err(|e| CasError::io(child_abs, e))?;
    let bytes = target.as_os_str().as_encoded_bytes().to_vec();
    let blob = capture.put_file_bytes(&bytes)?;
    Ok(TreeEntry {
        name: name.to_string(),
        kind: EntryKind::Symlink,
        mode: 0o120_777,
        target: blob,
    })
}

/// On non-Unix platforms symlinks are not first-class; treat the link's resolved
/// content as a regular file so a tree is still capturable.
#[cfg(not(unix))]
fn symlink_entry<S: StorageBackend>(
    name: &str,
    child_abs: &Path,
    capture: &Capture<'_, S>,
) -> Result<TreeEntry> {
    let bytes = std::fs::read(child_abs).map_err(|e| CasError::io(child_abs, e))?;
    let blob = capture.put_file_bytes(&bytes)?;
    Ok(TreeEntry {
        name: name.to_string(),
        kind: EntryKind::File,
        mode: 0o644,
        target: blob,
    })
}

/// Extract the permission mode from metadata.
#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    meta.mode()
}

/// On non-Unix platforms there are no POSIX mode bits; record a conventional
/// value so identity is stable across captures on the same platform.
#[cfg(not(unix))]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    if meta.is_dir() {
        0o755
    } else {
        0o644
    }
}

/// Restore permission bits on Unix; a no-op elsewhere.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    // Mask to the permission bits; the file-type bits in `mode` are not settable
    // via permissions. Best-effort: a failure to chmod does not corrupt content.
    let perms = std::fs::Permissions::from_mode(mode & 0o7777);
    let _ = std::fs::set_permissions(path, perms);
}

/// Permission restoration is a no-op on non-Unix platforms.
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Recreate a symlink at `path` pointing at the decoded `target_bytes`.
#[cfg(unix)]
fn materialize_symlink(target_bytes: &[u8], path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::OsStr::from_bytes(target_bytes);
    // Remove any pre-existing entry so re-materialization is idempotent.
    let _ = std::fs::remove_file(path);
    std::os::unix::fs::symlink(target, path).map_err(|e| CasError::io(path, e))
}

/// On non-Unix platforms a stored symlink is materialized as a regular file
/// containing the target text (the closest faithful reconstruction).
#[cfg(not(unix))]
fn materialize_symlink(target_bytes: &[u8], path: &Path) -> Result<()> {
    std::fs::write(path, target_bytes).map_err(|e| CasError::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loose::LooseStore;
    use spork_ignore::IgnoreProfile;
    use std::fs;
    use tempfile::TempDir;

    fn store(tmp: &TempDir) -> ObjectStore<LooseStore> {
        ObjectStore::new(LooseStore::open(tmp.path()).unwrap())
    }

    fn default_matcher() -> IgnoreMatcher {
        IgnoreMatcher::new(&IgnoreProfile::default_profile())
    }

    fn pseudo_random(n: usize, seed: u64) -> Vec<u8> {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            out.push((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8);
        }
        out
    }

    #[test]
    fn small_blob_round_trips() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let data = b"the quick brown fox";
        let (id, stats) = s.put_blob_bytes(data).unwrap();
        assert_eq!(s.read_blob(&id).unwrap(), data);
        // One chunk + one blob object.
        assert_eq!(stats.new_chunks, 1);
        assert_eq!(stats.new_objects, 1);
    }

    #[test]
    fn empty_blob_round_trips() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let (id, stats) = s.put_blob_bytes(b"").unwrap();
        assert_eq!(s.read_blob(&id).unwrap(), b"");
        assert_eq!(stats.new_chunks, 1); // a single empty chunk
    }

    #[test]
    fn identical_blob_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let data = pseudo_random(2 * 1024 * 1024, 7);
        let (id1, _s1) = s.put_blob_bytes(&data).unwrap();
        let (id2, s2) = s.put_blob_bytes(&data).unwrap();
        assert_eq!(id1, id2);
        // Re-put writes nothing new.
        assert_eq!(s2.new_objects, 0);
        assert_eq!(s2.new_chunks, 0);
        assert_eq!(s2.bytes_written, 0);
        assert!(s2.reused_objects + s2.reused_chunks > 0);
    }

    #[test]
    fn single_region_edit_restores_one_chunk() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        // 4 MiB so FastCDC produces many chunks.
        let mut data = pseudo_random(4 * 1024 * 1024, 11);
        let (_id, _cold) = s.put_blob_bytes(&data).unwrap();
        // Edit one region (flip several adjacent bytes well inside the buffer).
        let mid = data.len() / 2;
        for b in &mut data[mid..mid + 16] {
            *b ^= 0xFF;
        }
        let (_id2, warm) = s.put_blob_bytes(&data).unwrap();
        // Exactly one new chunk should be stored for a single-region edit.
        assert_eq!(
            warm.new_chunks, 1,
            "expected exactly one new chunk, got stats {warm:?}"
        );
        // The blob object itself changed (its chunk list changed), so one new
        // object too.
        assert_eq!(warm.new_objects, 1);
        assert!(warm.reused_chunks > 0);
    }

    #[test]
    fn put_tree_round_trips_and_honors_ignore() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), b"alpha").unwrap();
        fs::write(src.join("sub/b.txt"), b"beta").unwrap();
        // An ignored directory and file.
        fs::create_dir_all(src.join("node_modules/pkg")).unwrap();
        fs::write(src.join("node_modules/pkg/x.js"), b"junk").unwrap();
        fs::write(src.join("debug.pyc"), b"bytecode").unwrap();

        let s = store(&tmp);
        let (tree_id, _stats) = s.put_tree(&src, &default_matcher()).unwrap();

        // Materialize and check the kept files are byte-identical and the ignored
        // ones are absent.
        let dest = tmp.path().join("out");
        s.materialize_tree(&tree_id, &dest).unwrap();
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"beta");
        assert!(!dest.join("node_modules").exists());
        assert!(!dest.join("debug.pyc").exists());
    }

    #[test]
    fn identical_sibling_subtrees_share_one_hash() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        for d in ["dir_a", "dir_b"] {
            fs::create_dir_all(root.join(d)).unwrap();
            fs::write(root.join(d).join("same.txt"), b"identical contents").unwrap();
        }
        let s = store(&tmp);
        let (_root_id, _stats) = s.put_tree(&root, &default_matcher()).unwrap();

        // Read the root tree and confirm both subdirs point at the SAME tree id.
        let root_tree = s
            .put_tree(&root, &default_matcher())
            .map(|(id, _)| s.read_tree(&id).unwrap())
            .unwrap();
        let targets: Vec<Hash> = root_tree
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir)
            .map(|e| e.target)
            .collect();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0], targets[1], "identical subtrees must dedup");
    }

    #[test]
    fn warm_tree_recapture_writes_zero_new_objects() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/file1.txt"), b"one").unwrap();
        fs::write(root.join("a/b/file2.txt"), b"two").unwrap();
        fs::write(root.join("a/b/c/file3.txt"), b"three").unwrap();

        let s = store(&tmp);
        let m = default_matcher();
        let (_id1, _cold) = s.put_tree(&root, &m).unwrap();
        let (_id2, warm) = s.put_tree(&root, &m).unwrap();
        assert_eq!(warm.new_objects, 0, "warm re-capture must write no objects");
        assert_eq!(warm.new_chunks, 0, "warm re-capture must write no chunks");
        assert_eq!(warm.bytes_written, 0);
    }

    #[test]
    fn snapshot_round_trips() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let root = spork_hash::hash_bytes(b"root");
        let ignore = IgnoreProfile::default_profile().hash();
        let (snap_id, stats) = s
            .put_snapshot(root, ignore, Some("deadbeef".to_string()))
            .unwrap();
        assert_eq!(stats.new_objects, 1);
        let snap = s.read_snapshot(&snap_id).unwrap();
        assert_eq!(snap.root_tree, root);
        assert_eq!(snap.ignore_profile_hash, ignore);
        assert_eq!(snap.git_parent_commit.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn capture_snapshot_then_materialize_is_byte_identical() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(root.join("pkg")).unwrap();
        fs::write(root.join("main.rs"), b"fn main() {}").unwrap();
        fs::write(root.join("pkg/lib.rs"), pseudo_random(3 * 1024 * 1024, 13)).unwrap();

        let s = store(&tmp);
        let profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&profile);
        let (snap, _root_tree, _stats) =
            s.capture_snapshot(&root, &m, profile.hash(), None).unwrap();

        let dest = tmp.path().join("restored");
        s.materialize_snapshot(&snap, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("main.rs")).unwrap(),
            fs::read(root.join("main.rs")).unwrap()
        );
        assert_eq!(
            fs::read(dest.join("pkg/lib.rs")).unwrap(),
            fs::read(root.join("pkg/lib.rs")).unwrap()
        );
    }

    #[test]
    fn read_missing_blob_is_not_found() {
        let tmp = TempDir::new().unwrap();
        let s = store(&tmp);
        let bogus = spork_hash::hash_bytes(b"bogus");
        assert!(matches!(s.read_blob(&bogus), Err(CasError::NotFound(_))));
        assert!(matches!(s.read_tree(&bogus), Err(CasError::NotFound(_))));
        assert!(matches!(
            s.read_snapshot(&bogus),
            Err(CasError::NotFound(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_is_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        let script = root.join("run.sh");
        fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let s = store(&tmp);
        let (tree_id, _stats) = s.put_tree(&root, &default_matcher()).unwrap();
        let dest = tmp.path().join("out");
        s.materialize_tree(&tree_id, &dest).unwrap();
        let mode = fs::metadata(dest.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "executable bits must be restored");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_round_trips() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("target.txt"), b"i am the target").unwrap();
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();

        let s = store(&tmp);
        let (tree_id, _stats) = s.put_tree(&root, &default_matcher()).unwrap();
        let dest = tmp.path().join("out");
        s.materialize_tree(&tree_id, &dest).unwrap();

        let link = dest.join("link.txt");
        let meta = fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());
        assert_eq!(fs::read_link(&link).unwrap(), Path::new("target.txt"));
    }
}
