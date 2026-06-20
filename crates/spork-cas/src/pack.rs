//! A real, minimal packfile: [`PackStore`] and [`PackIndex`].
//!
//! Loose objects are simple and crash-safe but cost one inode and one `open` per
//! object; once a repository accumulates many objects, bundling them into a
//! handful of *packfiles* (mirroring `.git`'s loose-then-pack model, DESIGN.md
//! §10.1, §16) keeps reads cheap. This module implements a complete — if
//! deliberately simple — pack format and the read/repack machinery, not a stub.
//!
//! # Layout
//!
//! A pack is a pair of files under `objects/pack/`:
//!
//! - `<name>.pack` — a header (`magic b"SPKP" + version u8`) followed by the
//!   concatenation of full stored-object byte sequences (each is a
//!   [`crate::header::ObjectHeader`] + payload, exactly as a loose object on
//!   disk). Each object is self-delimiting via its header's `payload_len`.
//! - `<name>.idx` — a header (`magic b"SPKI" + version u8 + count u32`) followed
//!   by `count` fixed-width entries `{hash[32], offset u64, length u64}` sorted
//!   ascending by hash, where `offset`/`length` locate the object's full bytes
//!   inside the `.pack`. Sorting enables a binary search for O(log n) lookup
//!   without loading the whole index into a map (though small indices are simply
//!   read whole).
//!
//! `<name>` is the BLAKE3 digest (hex) of the `.pack` body, so a pack's name is
//! itself content-derived and two identical packs collide harmlessly.
//!
//! # What "minimal" means
//!
//! There is no zlib/zstd compression of the pack body and no delta-encoding
//! between objects — those are optimizations layered on later without changing
//! the read contract. What *is* complete: writing a consistent pack+index pair
//! atomically, looking objects up through the index with header validation and
//! integrity checking, and enumerating a pack's contents so a repack can fold
//! loose objects in. Reads through a pack reject an unknown generation exactly
//! like loose reads do (the same [`crate::header::ObjectHeader::parse`] gate).
//!
//! Design references: DESIGN.md §10.1 (loose objects + periodic packfiles), §16
//! (CAS: loose objects + packfiles).

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use spork_hash::Hash;

use crate::error::{CasError, Result};
use crate::header::{ObjectHeader, HEADER_LEN};

/// Magic opening a `.pack` body file ("SPKP").
const PACK_MAGIC: [u8; 4] = *b"SPKP";
/// Magic opening a `.idx` file ("SPKI").
const IDX_MAGIC: [u8; 4] = *b"SPKI";
/// The current pack/index format version.
const PACK_VERSION: u8 = 1;
/// Size of the `.pack` file header (magic + version).
const PACK_HEADER_LEN: usize = 5;
/// Size of the `.idx` file header (magic + version + count).
const IDX_HEADER_LEN: usize = 4 + 1 + 4;
/// Size of one index entry: 32-byte hash + u64 offset + u64 length.
const IDX_ENTRY_LEN: usize = 32 + 8 + 8;

/// One located object within a pack: its digest and where its bytes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IndexEntry {
    hash: Hash,
    offset: u64,
    length: u64,
}

/// An in-memory pack index: digest → (offset, length) within a `.pack`.
///
/// Held sorted by hash so lookups are a binary search. Built either while
/// writing a pack ([`PackStore::write`]) or by reading an `.idx` file
/// ([`PackIndex::read`]).
#[derive(Debug, Clone)]
pub(crate) struct PackIndex {
    entries: Vec<IndexEntry>,
}

impl PackIndex {
    /// Build an index from `(hash, offset, length)` triples, sorting by hash.
    fn from_entries(mut entries: Vec<IndexEntry>) -> Self {
        entries.sort_unstable_by(|a, b| a.hash.as_bytes().cmp(b.hash.as_bytes()));
        PackIndex { entries }
    }

    /// Serialize the index to its on-disk bytes.
    fn to_bytes(&self) -> Vec<u8> {
        let count = self.entries.len() as u32;
        let mut out = Vec::with_capacity(IDX_HEADER_LEN + self.entries.len() * IDX_ENTRY_LEN);
        out.extend_from_slice(&IDX_MAGIC);
        out.push(PACK_VERSION);
        out.extend_from_slice(&count.to_be_bytes());
        for e in &self.entries {
            out.extend_from_slice(e.hash.as_bytes());
            out.extend_from_slice(&e.offset.to_be_bytes());
            out.extend_from_slice(&e.length.to_be_bytes());
        }
        out
    }

    /// Parse an `.idx` file's bytes into an index.
    fn parse(buf: &[u8], path: &Path) -> Result<Self> {
        if buf.len() < IDX_HEADER_LEN {
            return Err(CasError::MalformedHeader(format!(
                "pack index {path:?} is shorter than its header"
            )));
        }
        if buf[0..4] != IDX_MAGIC {
            return Err(CasError::MalformedHeader(format!(
                "pack index {path:?} has bad magic"
            )));
        }
        if buf[4] != PACK_VERSION {
            return Err(CasError::MalformedHeader(format!(
                "pack index {path:?} has unsupported version {}",
                buf[4]
            )));
        }
        let mut count_bytes = [0u8; 4];
        count_bytes.copy_from_slice(&buf[5..9]);
        let count = u32::from_be_bytes(count_bytes) as usize;

        let expected = IDX_HEADER_LEN + count * IDX_ENTRY_LEN;
        if buf.len() != expected {
            return Err(CasError::MalformedHeader(format!(
                "pack index {path:?} length {} disagrees with declared count {count} (expected {expected})",
                buf.len()
            )));
        }

        let mut entries = Vec::with_capacity(count);
        let mut pos = IDX_HEADER_LEN;
        for _ in 0..count {
            let mut hbytes = [0u8; 32];
            hbytes.copy_from_slice(&buf[pos..pos + 32]);
            pos += 32;
            let mut obytes = [0u8; 8];
            obytes.copy_from_slice(&buf[pos..pos + 8]);
            pos += 8;
            let mut lbytes = [0u8; 8];
            lbytes.copy_from_slice(&buf[pos..pos + 8]);
            pos += 8;
            entries.push(IndexEntry {
                hash: Hash::from_bytes(hbytes),
                offset: u64::from_be_bytes(obytes),
                length: u64::from_be_bytes(lbytes),
            });
        }
        // Trust-but-don't-assume: the on-disk order should already be sorted, but
        // re-establish the invariant so binary search is always valid.
        Ok(PackIndex::from_entries(entries))
    }

    /// Read and parse an `.idx` file from disk.
    fn read(path: &Path) -> Result<Self> {
        let buf = fs::read(path).map_err(|e| CasError::io(path, e))?;
        PackIndex::parse(&buf, path)
    }

    /// Look up an entry by digest via binary search.
    fn find(&self, hash: &Hash) -> Option<IndexEntry> {
        self.entries
            .binary_search_by(|e| e.hash.as_bytes().cmp(hash.as_bytes()))
            .ok()
            .map(|i| self.entries[i])
    }

    /// The digests this index covers (for repack reconciliation).
    fn hashes(&self) -> impl Iterator<Item = &Hash> {
        self.entries.iter().map(|e| &e.hash)
    }
}

/// A read/write handle to the pack directory of a store.
///
/// A `PackStore` is *not* itself a [`crate::StorageBackend`]; it is the pack-aware
/// half of [`crate::LooseStore`], which composes loose-object reads with
/// pack-through reads and exposes [`crate::LooseStore::repack`]. This type owns
/// the format: writing a `pack`/`idx` pair from a set of objects, reading an
/// object out of a pack, and enumerating packs.
#[derive(Debug, Clone)]
pub(crate) struct PackStore {
    /// The `objects/pack/` directory.
    dir: PathBuf,
}

impl PackStore {
    /// Open (creating the directory) the pack store rooted at `pack_dir`.
    pub(crate) fn open(pack_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&pack_dir).map_err(|e| CasError::io(&pack_dir, e))?;
        Ok(PackStore { dir: pack_dir })
    }

    /// List the `.pack` files currently in the pack directory.
    fn pack_files(&self) -> Result<Vec<PathBuf>> {
        let mut packs = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            // A missing pack dir simply means no packs yet.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(packs),
            Err(e) => return Err(CasError::io(&self.dir, e)),
        };
        for entry in rd {
            let entry = entry.map_err(|e| CasError::io(&self.dir, e))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("pack") {
                packs.push(path);
            }
        }
        // Deterministic order so behavior is reproducible.
        packs.sort();
        Ok(packs)
    }

    /// The `.idx` path paired with a `.pack` path.
    fn idx_path_for(pack: &Path) -> PathBuf {
        pack.with_extension("idx")
    }

    /// Whether any pack in the directory contains `hash`.
    pub(crate) fn has(&self, hash: &Hash) -> Result<bool> {
        for pack in self.pack_files()? {
            let idx_path = Self::idx_path_for(&pack);
            if !idx_path.exists() {
                continue;
            }
            let idx = PackIndex::read(&idx_path)?;
            if idx.find(hash).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Read the *payload* of `hash` from whichever pack holds it, or `None`.
    ///
    /// Parses and validates the object header (rejecting an unknown generation)
    /// and verifies the payload's integrity against `hash`.
    pub(crate) fn get(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        for pack in self.pack_files()? {
            let idx_path = Self::idx_path_for(&pack);
            if !idx_path.exists() {
                continue;
            }
            let idx = PackIndex::read(&idx_path)?;
            if let Some(entry) = idx.find(hash) {
                let full = self.read_object_bytes(&pack, &entry)?;
                let payload = parse_and_verify(&full, hash)?;
                return Ok(Some(payload));
            }
        }
        Ok(None)
    }

    /// Read the full stored-object bytes (header + payload) for an index entry.
    fn read_object_bytes(&self, pack: &Path, entry: &IndexEntry) -> Result<Vec<u8>> {
        let mut f = fs::File::open(pack).map_err(|e| CasError::io(pack, e))?;
        f.seek(SeekFrom::Start(entry.offset))
            .map_err(|e| CasError::io(pack, e))?;
        let mut buf = vec![0u8; entry.length as usize];
        f.read_exact(&mut buf).map_err(|e| CasError::io(pack, e))?;
        Ok(buf)
    }

    /// Enumerate every `(hash, full_object_bytes)` across all packs.
    ///
    /// Used by repack to carry already-packed objects forward into a new
    /// consolidated pack. Bytes are the complete framed objects (header +
    /// payload), ready to re-concatenate.
    pub(crate) fn enumerate_objects(&self) -> Result<Vec<(Hash, Vec<u8>)>> {
        let mut out = Vec::new();
        for pack in self.pack_files()? {
            let idx_path = Self::idx_path_for(&pack);
            if !idx_path.exists() {
                continue;
            }
            let idx = PackIndex::read(&idx_path)?;
            for hash in idx.hashes() {
                if let Some(entry) = idx.find(hash) {
                    let full = self.read_object_bytes(&pack, &entry)?;
                    out.push((*hash, full));
                }
            }
        }
        Ok(out)
    }

    /// Write a new pack from a set of `(hash, full_object_bytes)` objects.
    ///
    /// `objects` are complete framed objects (header + payload). Duplicate hashes
    /// are de-duplicated (first wins). The pack body and its index are written to
    /// temporary files and atomically renamed into place so a reader never
    /// observes a half-written pack. The pack is named by the BLAKE3 of its body.
    ///
    /// Returns the digests actually written. Writing an empty set is a no-op that
    /// returns an empty list (no files created).
    pub(crate) fn write(&self, objects: &[(Hash, Vec<u8>)]) -> Result<Vec<Hash>> {
        // De-duplicate by hash, preserving first occurrence, and order
        // deterministically by hash so the resulting pack body is reproducible.
        let mut seen = std::collections::BTreeMap::new();
        for (hash, bytes) in objects {
            seen.entry(*hash).or_insert_with(|| bytes.clone());
        }
        if seen.is_empty() {
            return Ok(Vec::new());
        }

        // Build the pack body and the index together.
        let mut body = Vec::with_capacity(PACK_HEADER_LEN);
        body.extend_from_slice(&PACK_MAGIC);
        body.push(PACK_VERSION);

        let mut entries = Vec::with_capacity(seen.len());
        let mut written = Vec::with_capacity(seen.len());
        for (hash, bytes) in &seen {
            let offset = body.len() as u64;
            body.extend_from_slice(bytes);
            entries.push(IndexEntry {
                hash: *hash,
                offset,
                length: bytes.len() as u64,
            });
            written.push(*hash);
        }

        // Name the pack by the digest of its body (content-derived, collision-safe).
        let name = spork_hash::hash_bytes(&body).to_hex();
        let pack_path = self.dir.join(format!("{name}.pack"));
        let idx_path = self.dir.join(format!("{name}.idx"));

        let index = PackIndex::from_entries(entries);
        let idx_bytes = index.to_bytes();

        // Write body and index to temp files, fsync, then atomically rename. The
        // index is renamed *after* the body so a reader that sees an `.idx` is
        // guaranteed to find the corresponding `.pack`.
        write_atomic(&pack_path, &body)?;
        write_atomic(&idx_path, &idx_bytes)?;

        Ok(written)
    }

    /// Delete a pack and its index (used by repack to remove superseded packs).
    fn remove_pack(&self, pack: &Path) -> Result<()> {
        let idx = Self::idx_path_for(pack);
        // Remove the index first so a concurrent reader never finds an index that
        // points at a deleted pack.
        if idx.exists() {
            fs::remove_file(&idx).map_err(|e| CasError::io(&idx, e))?;
        }
        if pack.exists() {
            fs::remove_file(pack).map_err(|e| CasError::io(pack, e))?;
        }
        Ok(())
    }

    /// Replace all existing packs with a single consolidated pack containing
    /// `objects` plus whatever the old packs already held.
    ///
    /// Returns the digests in the consolidated pack. The new pack is written
    /// first; only after it is durably in place are the old packs removed, so a
    /// crash mid-repack leaves a readable (if briefly redundant) store.
    pub(crate) fn consolidate(&self, mut objects: Vec<(Hash, Vec<u8>)>) -> Result<Vec<Hash>> {
        let old_packs = self.pack_files()?;
        // Fold in already-packed objects so consolidation never loses data.
        objects.extend(self.enumerate_objects()?);

        let written = self.write(&objects)?;

        // Identify the freshly written pack so we don't delete it as "old".
        let new_name = {
            // Recompute the body digest the same way `write` did, to find the new
            // pack's filename. Cheap relative to I/O and avoids threading the
            // name out of `write`.
            let mut seen = std::collections::BTreeMap::new();
            for (hash, bytes) in &objects {
                seen.entry(*hash).or_insert_with(|| bytes.clone());
            }
            let mut body = Vec::with_capacity(PACK_HEADER_LEN);
            body.extend_from_slice(&PACK_MAGIC);
            body.push(PACK_VERSION);
            for bytes in seen.values() {
                body.extend_from_slice(bytes);
            }
            spork_hash::hash_bytes(&body).to_hex()
        };
        let new_pack = self.dir.join(format!("{new_name}.pack"));

        for pack in old_packs {
            if pack != new_pack {
                self.remove_pack(&pack)?;
            }
        }
        Ok(written)
    }
}

/// Parse the header of a full stored object, reject unknown generations, slice
/// out the payload, and verify it hashes to `expected`.
fn parse_and_verify(full: &[u8], expected: &Hash) -> Result<Vec<u8>> {
    let header = ObjectHeader::parse(full)?;
    let want = header.payload_len as usize;
    if full.len() < HEADER_LEN + want {
        return Err(CasError::MalformedHeader(format!(
            "packed object truncated: header declares {want} payload bytes but only {} follow",
            full.len().saturating_sub(HEADER_LEN)
        )));
    }
    let payload = &full[HEADER_LEN..HEADER_LEN + want];
    let actual = spork_hash::hash_bytes(payload);
    if &actual != expected {
        return Err(CasError::IntegrityMismatch {
            addressed: *expected,
            actual,
        });
    }
    Ok(payload.to_vec())
}

/// Write `bytes` to `path` atomically: write to a sibling temp file, fsync it,
/// then rename over the destination. The directory is fsynced after the rename so
/// the new name is durable.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| CasError::MalformedHeader(format!("pack path {path:?} has no parent")))?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CasError::MalformedHeader(format!("pack path {path:?} has no file name")))?;
    let tmp = parent.join(format!(".{file_name}.tmp"));

    {
        let mut f = fs::File::create(&tmp).map_err(|e| CasError::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| CasError::io(&tmp, e))?;
        f.sync_all().map_err(|e| CasError::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| CasError::io(path, e))?;

    // Best-effort directory fsync so the rename is durable. A platform that
    // cannot open a directory for syncing simply skips this (the rename is still
    // ordered after the file's own fsync).
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::ObjectHeader;
    use crate::object::ObjKind;
    use spork_hash::{hash_bytes, HashTag};
    use tempfile::TempDir;

    /// Frame a payload as a full stored object and return `(hash, full_bytes)`.
    fn obj(kind: ObjKind, payload: &[u8]) -> (Hash, Vec<u8>) {
        let hash = hash_bytes(payload);
        let full = ObjectHeader::new(HashTag::CURRENT, kind, payload.len() as u64).frame(payload);
        (hash, full)
    }

    #[test]
    fn write_then_get_round_trips() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();

        let (h1, o1) = obj(ObjKind::Chunk, b"hello");
        let (h2, o2) = obj(ObjKind::Blob, b"world!!");
        let written = store.write(&[(h1, o1), (h2, o2)]).unwrap();
        assert_eq!(written.len(), 2);

        assert_eq!(store.get(&h1).unwrap().as_deref(), Some(&b"hello"[..]));
        assert_eq!(store.get(&h2).unwrap().as_deref(), Some(&b"world!!"[..]));
        assert!(store.has(&h1).unwrap());
        assert!(store.has(&h2).unwrap());

        let absent = hash_bytes(b"absent");
        assert_eq!(store.get(&absent).unwrap(), None);
        assert!(!store.has(&absent).unwrap());
    }

    #[test]
    fn empty_write_is_noop() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        assert!(store.write(&[]).unwrap().is_empty());
        assert!(store.pack_files().unwrap().is_empty());
    }

    #[test]
    fn duplicate_hashes_are_deduped() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        let (h, o) = obj(ObjKind::Chunk, b"dup");
        let written = store.write(&[(h, o.clone()), (h, o)]).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(store.get(&h).unwrap().as_deref(), Some(&b"dup"[..]));
    }

    #[test]
    fn enumerate_returns_all_objects() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        let (h1, o1) = obj(ObjKind::Chunk, b"a");
        let (h2, o2) = obj(ObjKind::Chunk, b"b");
        store.write(&[(h1, o1), (h2, o2)]).unwrap();
        let all = store.enumerate_objects().unwrap();
        let hashes: std::collections::HashSet<Hash> = all.iter().map(|(h, _)| *h).collect();
        assert!(hashes.contains(&h1));
        assert!(hashes.contains(&h2));
    }

    #[test]
    fn consolidate_merges_packs_and_removes_old() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        let (h1, o1) = obj(ObjKind::Chunk, b"first");
        store.write(&[(h1, o1)]).unwrap();
        let (h2, o2) = obj(ObjKind::Chunk, b"second");
        store.write(&[(h2, o2)]).unwrap();
        assert_eq!(store.pack_files().unwrap().len(), 2);

        let (h3, o3) = obj(ObjKind::Chunk, b"third");
        store.consolidate(vec![(h3, o3)]).unwrap();
        // Exactly one pack remains and all three objects are readable.
        assert_eq!(store.pack_files().unwrap().len(), 1);
        for h in [h1, h2, h3] {
            assert!(store.get(&h).unwrap().is_some());
        }
    }

    #[test]
    fn get_rejects_unknown_generation_in_pack() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        // Build an object whose header carries a future generation, but address
        // it by the digest of its payload so the index points at it.
        let payload = b"future";
        let hash = hash_bytes(payload);
        let mut full = ObjectHeader::new(HashTag::CURRENT, ObjKind::Chunk, payload.len() as u64)
            .frame(payload);
        full[6] = 99; // tamper the generation byte
        store.write(&[(hash, full)]).unwrap();

        let err = store.get(&hash).unwrap_err();
        assert!(matches!(err, CasError::UnknownGeneration(99)));
    }

    #[test]
    fn get_detects_integrity_mismatch() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        // Store bytes under the *wrong* digest.
        let payload = b"real";
        let wrong = hash_bytes(b"claimed");
        let full = ObjectHeader::new(HashTag::CURRENT, ObjKind::Chunk, payload.len() as u64)
            .frame(payload);
        store.write(&[(wrong, full)]).unwrap();
        let err = store.get(&wrong).unwrap_err();
        assert!(matches!(err, CasError::IntegrityMismatch { .. }));
    }

    #[test]
    fn index_parse_rejects_corruption() {
        let tmp = TempDir::new().unwrap();
        let store = PackStore::open(tmp.path().join("pack")).unwrap();
        let (h, o) = obj(ObjKind::Chunk, b"x");
        store.write(&[(h, o)]).unwrap();
        let idx_path = store.pack_files().unwrap()[0].with_extension("idx");
        // Truncate the index file to corrupt it.
        fs::write(&idx_path, b"SPKI").unwrap();
        assert!(matches!(
            PackIndex::read(&idx_path),
            Err(CasError::MalformedHeader(_))
        ));
    }
}
