//! The v1 production storage backend: [`LooseStore`].
//!
//! Loose objects are the simple, crash-safe write path (mirroring `.git/objects`,
//! DESIGN.md §10.1, §16): each object is one file at
//! `objects/<aa>/<rest-of-hex>`, where `<aa>` is the first byte of the digest in
//! hex (a 256-way fan-out so no single directory holds every object). The file
//! is the self-describing header ([`crate::header`]) followed by the payload.
//!
//! `LooseStore` is also the *composition point* with packfiles: a write always
//! produces a loose object, but a read first checks loose storage and then falls
//! through to any packfiles in `objects/pack/` (so objects folded into a pack by
//! [`LooseStore::repack`] remain readable). This is what satisfies the contract
//! "LooseStore::get must also find objects in packs."
//!
//! # Durability
//!
//! Writes are atomic: the framed bytes go to a temp file, are fsynced, then
//! renamed into place. A concurrent reader therefore never sees a partial object,
//! and an interrupted write leaves at most a stray temp file (reclaimable). Since
//! the file name is the content digest, a re-put of identical bytes is a no-op.
//!
//! # Reads reject the future
//!
//! Every read parses the stored header and **rejects an unknown hash generation**
//! ([`crate::CasError::UnknownGeneration`]) before returning any bytes — the
//! no-domino guarantee (DESIGN.md A.7 C-3) — and verifies the payload's integrity
//! against the digest it was stored under.
//!
//! Design references: DESIGN.md §10.1 (loose objects + packfiles), §6.1 (content
//! identity), Appendix A.7 C-3 (no-domino generation seam).

use std::fs;
use std::path::{Path, PathBuf};

use spork_hash::{Hash, HashTag};

use crate::backend::StorageBackend;
use crate::error::{CasError, Result};
use crate::header::{ObjectHeader, HEADER_LEN};
use crate::object::ObjKind;
use crate::pack::PackStore;

/// The on-disk content-addressed store: loose objects with pack read-through.
///
/// Construct with [`LooseStore::open`], pointing at the directory that will hold
/// `objects/` (typically `.spork/objects`). Implements [`StorageBackend`].
#[derive(Debug, Clone)]
pub struct LooseStore {
    /// The `objects/` root holding the `<aa>/<rest>` loose tree and `pack/`.
    objects_dir: PathBuf,
    /// The pack-aware sibling used for read-through and repack.
    packs: PackStore,
}

impl LooseStore {
    /// Open (creating directories as needed) a store whose objects live under
    /// `root/objects`.
    ///
    /// `root` is typically a `.spork` directory; the loose tree is created at
    /// `root/objects/<aa>/...` and packs at `root/objects/pack/`.
    ///
    /// # Errors
    /// Returns [`CasError::Io`] if the object directories cannot be created.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let objects_dir = root.as_ref().join("objects");
        fs::create_dir_all(&objects_dir).map_err(|e| CasError::io(&objects_dir, e))?;
        let packs = PackStore::open(objects_dir.join("pack"))?;
        Ok(LooseStore { objects_dir, packs })
    }

    /// The `objects/` directory this store manages.
    #[must_use]
    pub fn objects_dir(&self) -> &Path {
        &self.objects_dir
    }

    /// The loose-object path for a digest: `objects/<aa>/<rest-of-hex>`.
    fn loose_path(&self, hash: &Hash) -> PathBuf {
        let hex = hash.to_hex();
        // `to_hex` always yields 64 chars, so [..2] / [2..] are safe.
        let (prefix, rest) = hex.split_at(2);
        self.objects_dir.join(prefix).join(rest)
    }

    /// Read and validate a loose object file, returning its payload, or `None` if
    /// the file is absent.
    fn read_loose(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        let path = self.loose_path(hash);
        let full = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CasError::io(&path, e)),
        };
        Ok(Some(parse_and_verify(&full, hash, &path)?))
    }

    /// Fold every loose object into a single consolidated packfile, then remove
    /// the now-packed loose files.
    ///
    /// This is the real (minimal) repack: it gathers all loose objects, hands
    /// them to the [`PackStore`] to merge with any existing packs into one pack,
    /// and only after the pack is durably written deletes the loose copies. A
    /// crash mid-repack therefore leaves the objects readable (loose and/or
    /// packed) — never lost. Returns the number of loose objects packed.
    ///
    /// Reads continue to work transparently afterwards because
    /// [`LooseStore::get`] falls through to packs.
    ///
    /// # Errors
    /// Returns [`CasError::Io`] on any filesystem failure while gathering,
    /// packing, or removing objects.
    pub fn repack(&self) -> Result<usize> {
        let loose = self.gather_loose_objects()?;
        if loose.is_empty() {
            return Ok(0);
        }
        let count = loose.len();
        // Consolidate loose + existing packs into one pack.
        self.packs.consolidate(loose.clone())?;
        // Now that everything is in the pack, remove the loose copies.
        for (hash, _) in &loose {
            let path = self.loose_path(hash);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(CasError::io(&path, e)),
            }
        }
        Ok(count)
    }

    /// Collect `(hash, full_object_bytes)` for every loose object on disk.
    ///
    /// Walks the two-level `objects/<aa>/<rest>` fan-out, skipping the `pack/`
    /// directory and any temp files. The digest is reconstructed from the
    /// `<aa>` + `<rest>` filename and the object's header is validated, so a
    /// corrupt or unknown-generation loose file surfaces an error rather than
    /// being silently packed.
    fn gather_loose_objects(&self) -> Result<Vec<(Hash, Vec<u8>)>> {
        let mut out = Vec::new();
        let top = match fs::read_dir(&self.objects_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(CasError::io(&self.objects_dir, e)),
        };
        for entry in top {
            let entry = entry.map_err(|e| CasError::io(&self.objects_dir, e))?;
            let prefix_path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| CasError::io(&prefix_path, e))?;
            if !file_type.is_dir() {
                continue;
            }
            let prefix = match entry.file_name().to_str() {
                // The fan-out dirs are exactly two lowercase hex chars; anything
                // else (notably `pack`) is skipped.
                Some(name) if name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    name.to_string()
                }
                _ => continue,
            };

            let sub = fs::read_dir(&prefix_path).map_err(|e| CasError::io(&prefix_path, e))?;
            for file in sub {
                let file = file.map_err(|e| CasError::io(&prefix_path, e))?;
                let fpath = file.path();
                let ftype = file.file_type().map_err(|e| CasError::io(&fpath, e))?;
                if !ftype.is_file() {
                    continue;
                }
                let rest = match file.file_name().to_str() {
                    Some(name)
                        if name.len() == 62 && name.bytes().all(|b| b.is_ascii_hexdigit()) =>
                    {
                        name.to_string()
                    }
                    // Skip temp files and anything that is not a hex object name.
                    _ => continue,
                };
                let hex = format!("{prefix}{rest}");
                let hash = Hash::from_hex(&hex).map_err(|e| {
                    CasError::MalformedHeader(format!("bad loose object name: {e}"))
                })?;
                let full = fs::read(&fpath).map_err(|e| CasError::io(&fpath, e))?;
                // Validate (header + integrity) before packing.
                parse_and_verify(&full, &hash, &fpath)?;
                out.push((hash, full));
            }
        }
        Ok(out)
    }
}

impl StorageBackend for LooseStore {
    fn put(&self, tag: HashTag, kind: ObjKind, bytes: &[u8]) -> Result<Hash> {
        let hash = spork_hash::hash_bytes(bytes);
        let path = self.loose_path(&hash);

        // Idempotent: if the object already exists (loose or packed), do nothing.
        if path.exists() || self.packs.has(&hash)? {
            return Ok(hash);
        }

        let parent = path.parent().expect("loose path always has a parent dir");
        fs::create_dir_all(parent).map_err(|e| CasError::io(parent, e))?;

        let framed = ObjectHeader::new(tag, kind, bytes.len() as u64).frame(bytes);
        write_atomic(&path, &framed)?;
        Ok(hash)
    }

    fn get(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        // Loose first (the freshest write path), then packs.
        if let Some(payload) = self.read_loose(hash)? {
            return Ok(Some(payload));
        }
        self.packs.get(hash)
    }

    fn has(&self, hash: &Hash) -> Result<bool> {
        if self.loose_path(hash).exists() {
            return Ok(true);
        }
        self.packs.has(hash)
    }

    /// Durably commit a batch of new objects with a **single fsync** by folding
    /// the not-yet-present ones into one freshly written packfile.
    ///
    /// This is the cold-capture fast path. The per-object loose write
    /// ([`LooseStore::put`]) is crash-safe but pays one fsync *per object*; a cold
    /// `put_tree` of a large repo writes tens of thousands of objects and is
    /// therefore fsync-bound. Here we instead:
    ///
    /// 1. Frame each *new* object (skipping any already present loose or packed,
    ///    so dedup is preserved and a re-put writes nothing) into its full
    ///    header+payload bytes.
    /// 2. Hand the whole set to [`PackStore::write`], which writes the `.pack` and
    ///    `.idx` to temp files, fsyncs each, and renames them into place — so the
    ///    entire batch becomes durable behind **one** pair of fsyncs regardless of
    ///    object count.
    ///
    /// Reads are unaffected: [`LooseStore::get`]/[`LooseStore::has`] already fall
    /// through to packs, so a packed-via-batch object is found transparently.
    ///
    /// Crash safety is preserved: the pack is only ever observed whole (atomic
    /// rename after fsync), so a crash mid-batch leaves the store with either the
    /// new pack present or absent — never a half object and never a dangling
    /// reference, because every object is content-addressed and immutable.
    ///
    /// Digests are returned in input order (a present object still yields its
    /// digest), matching [`StorageBackend::put`].
    fn put_batch(&self, objects: &[(HashTag, ObjKind, Vec<u8>)]) -> Result<Vec<Hash>> {
        let mut digests = Vec::with_capacity(objects.len());
        // Collect the objects we actually need to persist, de-duplicating against
        // what is already on disk (loose or packed) and against earlier elements
        // of this same batch, so the pack never stores a redundant copy.
        let mut to_write: Vec<(Hash, Vec<u8>)> = Vec::new();
        let mut staged: std::collections::HashSet<Hash> = std::collections::HashSet::new();
        for (tag, kind, bytes) in objects {
            let hash = spork_hash::hash_bytes(bytes);
            digests.push(hash);
            if staged.contains(&hash) || self.has(&hash)? {
                continue;
            }
            let framed = ObjectHeader::new(*tag, *kind, bytes.len() as u64).frame(bytes);
            to_write.push((hash, framed));
            staged.insert(hash);
        }
        // One packfile, one durability barrier for the whole batch.
        self.packs.write(&to_write)?;
        Ok(digests)
    }
}

/// Parse a full stored object, reject unknown generations, and verify integrity,
/// returning the payload.
fn parse_and_verify(full: &[u8], expected: &Hash, path: &Path) -> Result<Vec<u8>> {
    let header = ObjectHeader::parse(full)?;
    let want = header.payload_len as usize;
    if full.len() != HEADER_LEN + want {
        return Err(CasError::MalformedHeader(format!(
            "loose object {path:?}: header declares {want} payload bytes but file body is {}",
            full.len().saturating_sub(HEADER_LEN)
        )));
    }
    let payload = &full[HEADER_LEN..];
    let actual = spork_hash::hash_bytes(payload);
    if &actual != expected {
        return Err(CasError::IntegrityMismatch {
            addressed: *expected,
            actual,
        });
    }
    Ok(payload.to_vec())
}

/// Atomically write `bytes` to `path` (temp file + fsync + rename).
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path.parent().expect("loose path always has a parent dir");
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .expect("loose object name is valid hex");
    let tmp = parent.join(format!(".{file_name}.tmp"));

    {
        let mut f = fs::File::create(&tmp).map_err(|e| CasError::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| CasError::io(&tmp, e))?;
        f.sync_all().map_err(|e| CasError::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| CasError::io(path, e))?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;
    use tempfile::TempDir;

    fn store() -> (TempDir, LooseStore) {
        let tmp = TempDir::new().unwrap();
        let store = LooseStore::open(tmp.path()).unwrap();
        (tmp, store)
    }

    #[test]
    fn put_get_round_trips() {
        let (_t, s) = store();
        let h = s.put(HashTag::CURRENT, ObjKind::Chunk, b"hello").unwrap();
        assert_eq!(h, hash_bytes(b"hello"));
        assert_eq!(s.get(&h).unwrap().as_deref(), Some(&b"hello"[..]));
        assert!(s.has(&h).unwrap());
    }

    #[test]
    fn get_missing_is_none() {
        let (_t, s) = store();
        let h = hash_bytes(b"nope");
        assert_eq!(s.get(&h).unwrap(), None);
        assert!(!s.has(&h).unwrap());
    }

    #[test]
    fn put_is_idempotent() {
        let (_t, s) = store();
        let h1 = s.put(HashTag::CURRENT, ObjKind::Blob, b"x").unwrap();
        let h2 = s.put(HashTag::CURRENT, ObjKind::Blob, b"x").unwrap();
        assert_eq!(h1, h2);
        // Exactly one loose file exists for it.
        assert!(s.loose_path(&h1).exists());
    }

    #[test]
    fn fanout_layout_is_used() {
        let (_t, s) = store();
        let h = s.put(HashTag::CURRENT, ObjKind::Chunk, b"layout").unwrap();
        let hex = h.to_hex();
        let expected = s.objects_dir().join(&hex[..2]).join(&hex[2..]);
        assert!(expected.exists());
    }

    #[test]
    fn read_rejects_unknown_generation() {
        let (_t, s) = store();
        let h = s.put(HashTag::CURRENT, ObjKind::Chunk, b"future").unwrap();
        // Tamper the generation byte (index 6) of the on-disk file.
        let path = s.loose_path(&h);
        let mut bytes = fs::read(&path).unwrap();
        bytes[6] = 99;
        fs::write(&path, &bytes).unwrap();
        let err = s.get(&h).unwrap_err();
        assert!(matches!(err, CasError::UnknownGeneration(99)));
    }

    #[test]
    fn read_detects_corruption() {
        let (_t, s) = store();
        let h = s.put(HashTag::CURRENT, ObjKind::Chunk, b"intact").unwrap();
        let path = s.loose_path(&h);
        let mut bytes = fs::read(&path).unwrap();
        // Corrupt a payload byte.
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(s.get(&h), Err(CasError::IntegrityMismatch { .. })));
    }

    #[test]
    fn repack_moves_loose_into_pack_and_reads_through() {
        let (_t, s) = store();
        let payloads: Vec<&[u8]> = vec![b"a", b"bb", b"ccc", b"dddd"];
        let mut hashes = Vec::new();
        for p in &payloads {
            hashes.push(s.put(HashTag::CURRENT, ObjKind::Chunk, p).unwrap());
        }
        let packed = s.repack().unwrap();
        assert_eq!(packed, payloads.len());

        // Loose files are gone...
        for h in &hashes {
            assert!(!s.loose_path(h).exists());
        }
        // ...but reads still succeed (through the pack) and `has` is true.
        for (h, p) in hashes.iter().zip(&payloads) {
            assert_eq!(s.get(h).unwrap().as_deref(), Some(&p[..]));
            assert!(s.has(h).unwrap());
        }
    }

    #[test]
    fn put_after_repack_is_still_idempotent() {
        let (_t, s) = store();
        let h = s.put(HashTag::CURRENT, ObjKind::Chunk, b"packme").unwrap();
        s.repack().unwrap();
        assert!(!s.loose_path(&h).exists());
        // Re-putting the same bytes must NOT create a new loose file: it is
        // already present in the pack.
        let h2 = s.put(HashTag::CURRENT, ObjKind::Chunk, b"packme").unwrap();
        assert_eq!(h, h2);
        assert!(!s.loose_path(&h).exists());
    }

    #[test]
    fn repack_with_no_loose_is_noop() {
        let (_t, s) = store();
        assert_eq!(s.repack().unwrap(), 0);
    }

    #[test]
    fn put_batch_packs_new_objects_and_reads_through() {
        let (_t, s) = store();
        let payloads: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma"];
        let batch: Vec<(HashTag, ObjKind, Vec<u8>)> = payloads
            .iter()
            .map(|p| (HashTag::CURRENT, ObjKind::Chunk, p.to_vec()))
            .collect();
        let digests = s.put_batch(&batch).unwrap();
        assert_eq!(digests.len(), payloads.len());

        // The batch went to a single packfile (no loose files created)...
        for (h, p) in digests.iter().zip(&payloads) {
            assert_eq!(*h, hash_bytes(p));
            assert!(
                !s.loose_path(h).exists(),
                "batch must not write loose files"
            );
            // ...and every object is found transparently through the pack.
            assert_eq!(s.get(h).unwrap().as_deref(), Some(&p[..]));
            assert!(s.has(h).unwrap());
        }
        // Exactly one pack pair exists for the whole batch (one durability barrier).
        let pack_dir = s.objects_dir().join("pack");
        let packs: Vec<_> = fs::read_dir(&pack_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pack"))
            .collect();
        assert_eq!(packs.len(), 1, "the batch must commit as a single pack");
    }

    #[test]
    fn put_batch_dedups_against_present_and_within_batch() {
        let (_t, s) = store();
        // Pre-existing object (loose).
        let present = s.put(HashTag::CURRENT, ObjKind::Chunk, b"present").unwrap();

        // Batch with: the already-present object, plus a duplicate pair, plus a
        // genuinely new one. All digests are returned in order; storage holds one
        // copy of each distinct payload.
        let batch: Vec<(HashTag, ObjKind, Vec<u8>)> = vec![
            (HashTag::CURRENT, ObjKind::Chunk, b"present".to_vec()),
            (HashTag::CURRENT, ObjKind::Chunk, b"dup".to_vec()),
            (HashTag::CURRENT, ObjKind::Chunk, b"dup".to_vec()),
            (HashTag::CURRENT, ObjKind::Chunk, b"fresh".to_vec()),
        ];
        let digests = s.put_batch(&batch).unwrap();
        assert_eq!(digests[0], present);
        assert_eq!(digests[1], digests[2], "duplicate payloads share a digest");
        for (i, p) in [&b"present"[..], b"dup", b"dup", b"fresh"]
            .iter()
            .enumerate()
        {
            assert_eq!(digests[i], hash_bytes(p));
            assert_eq!(s.get(&digests[i]).unwrap().as_deref(), Some(&p[..]));
        }
    }

    #[test]
    fn empty_put_batch_is_noop() {
        let (_t, s) = store();
        assert!(s.put_batch(&[]).unwrap().is_empty());
        // No pack files created for an empty batch.
        let pack_dir = s.objects_dir().join("pack");
        let any_pack = fs::read_dir(&pack_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("pack"));
        assert!(!any_pack);
    }
}
