//! Spork content-addressed store — the byte-identity core (DESIGN.md §6.1, §10).
//!
//! This crate is the heart of Spork's "click a node, see the exact state"
//! promise: it turns codebase bytes into a content-addressed, deduplicated object
//! graph and back again, byte-for-byte. It composes the lower foundation crates —
//! BLAKE3 identity ([`spork_hash`]), the frozen canonical encoder
//! ([`spork_canon`]), and the deps-excluded ignore policy ([`spork_ignore`]) —
//! into the Git-analogue object model and the storage substrate beneath it.
//!
//! # The pieces
//!
//! - **Object model** ([`object`]): [`ObjKind`], [`Blob`], [`Tree`]/[`TreeEntry`]/
//!   [`EntryKind`], [`Snapshot`]/[`CreationMeta`]. Every persisted object carries
//!   a schema version (`v`) that participates in its hash (constraint C5), and is
//!   addressed by `BLAKE3(canonical(object))`. A [`Chunk`](object::ObjKind::Chunk)
//!   is addressed by `BLAKE3(bytes)`.
//! - **Self-describing header** ([`header`], internal): a fixed 16-byte prefix
//!   recording the hash tag, kind, and payload length so a read can **reject an
//!   unknown hash generation** ([`CasError::UnknownGeneration`]) — the no-domino
//!   seam (DESIGN.md A.7 C-3).
//! - **Chunking** ([`chunk`]): FastCDC content-defined chunking so a one-region
//!   edit to a large file re-stores only the changed chunk(s) (DESIGN.md §10.4).
//! - **Storage seam** ([`StorageBackend`]): the frozen trait, with one v1 impl —
//!   [`LooseStore`] (loose objects with a real packfile read-through and
//!   [`LooseStore::repack`]).
//! - **High-level store** ([`ObjectStore`]): `put_blob_bytes` / `put_tree` /
//!   `put_snapshot` / `read_*` / `materialize_tree`, each reporting [`PutStats`]
//!   so dedup is measurable.
//!
//! # The dedup invariants this crate guarantees
//!
//! 1. **Identical content → identical id, zero rewrites.** Capturing the same
//!    bytes/tree twice writes nothing the second time.
//! 2. **Sub-file dedup.** Editing one region of a multi-megabyte file re-stores
//!    exactly the overlapping chunk(s).
//! 3. **Subtree dedup.** Identical directories share a single [`Tree`] object.
//! 4. **Roundtrip fidelity.** `materialize_tree` reproduces a captured tree
//!    byte-for-byte (permissions and symlinks included on Unix).
//! 5. **No-domino rejection.** An object whose stored header names a hash
//!    generation this build does not understand is refused on read.
//!
//! # Example
//! ```
//! use spork_cas::{LooseStore, ObjectStore};
//! use tempfile::TempDir;
//!
//! let dir = TempDir::new().unwrap();
//! let store = ObjectStore::new(LooseStore::open(dir.path()).unwrap());
//!
//! // Capture a file as a blob...
//! let (id, cold) = store.put_blob_bytes(b"hello world").unwrap();
//! assert_eq!(cold.new_objects, 1);
//! assert_eq!(store.read_blob(&id).unwrap(), b"hello world");
//!
//! // ...and re-capturing it writes nothing new.
//! let (id2, warm) = store.put_blob_bytes(b"hello world").unwrap();
//! assert_eq!(id, id2);
//! assert_eq!(warm.new_objects + warm.new_chunks, 0);
//! ```
//!
//! Design references: DESIGN.md §6.1 (the two-layer content/timeline model and
//! BLAKE3-everywhere identity), §10.1 (three-layer architecture / object model /
//! loose objects + packfiles), §10.4 (FastCDC sub-file dedup, ignore-aware
//! walking), §10.5 (`ignore_profile_hash` in snapshot identity).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod chunk;
mod error;
mod header;
mod loose;
pub mod object;
mod pack;
mod store;

pub use backend::StorageBackend;
pub use chunk::{chunk_ranges, ChunkRange, AVG_SIZE, MAX_SIZE, MIN_SIZE, SINGLE_CHUNK_THRESHOLD};
pub use error::{CasError, Result};
pub use loose::LooseStore;
pub use object::{
    Blob, CreationMeta, EntryKind, ObjKind, Snapshot, Tree, TreeEntry, BLOB_VERSION,
    SNAPSHOT_VERSION, TREE_VERSION,
};
pub use store::{ObjectStore, PutStats};

#[cfg(test)]
mod integration_tests {
    //! End-to-end checks across the whole crate (object model + backend +
    //! high-level store), proving the dedup, roundtrip, and no-domino invariants
    //! the definition-of-done enumerates.

    use super::*;
    use spork_ignore::{IgnoreMatcher, IgnoreProfile};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn store(tmp: &TempDir) -> ObjectStore<LooseStore> {
        ObjectStore::new(LooseStore::open(tmp.path()).unwrap())
    }

    fn matcher() -> IgnoreMatcher {
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

    /// Build a small representative project tree under `root`.
    fn build_project(root: &Path) {
        fs::create_dir_all(root.join("src/util")).unwrap();
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("Cargo.toml"), b"[package]\nname=\"x\"\n").unwrap();
        fs::write(root.join("src/main.rs"), b"fn main() { println!(\"hi\"); }").unwrap();
        fs::write(root.join("src/util/mod.rs"), b"pub fn f() -> u32 { 7 }").unwrap();
        // A large binary asset (exercises FastCDC).
        fs::write(
            root.join("assets/data.bin"),
            pseudo_random(5 * 1024 * 1024, 99),
        )
        .unwrap();
        // Things the default profile must exclude.
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/app"), b"compiled").unwrap();
        fs::create_dir_all(root.join("node_modules/dep")).unwrap();
        fs::write(root.join("node_modules/dep/index.js"), b"module").unwrap();
        fs::write(root.join(".DS_Store"), b"junk").unwrap();
    }

    #[test]
    fn capture_roundtrip_is_byte_identical_for_kept_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);

        let s = store(&tmp);
        let profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&profile);
        let (snap, _root_tree, _stats) =
            s.capture_snapshot(&root, &m, profile.hash(), None).unwrap();

        let dest = tmp.path().join("restored");
        s.materialize_snapshot(&snap, &dest).unwrap();

        // Every kept file is byte-identical.
        for rel in [
            "Cargo.toml",
            "src/main.rs",
            "src/util/mod.rs",
            "assets/data.bin",
        ] {
            assert_eq!(
                fs::read(dest.join(rel)).unwrap(),
                fs::read(root.join(rel)).unwrap(),
                "mismatch for {rel}"
            );
        }
        // Ignored entries are absent from the materialized tree.
        assert!(!dest.join("target").exists());
        assert!(!dest.join("node_modules").exists());
        assert!(!dest.join(".DS_Store").exists());

        // Whole-tree comparison: the restored set of (path, bytes) equals the
        // source minus ignored entries — nothing extra, nothing missing.
        let mut original = Vec::new();
        let mut restored = Vec::new();
        super::integration_tests_helpers::collect(&root, &root, &mut original);
        super::integration_tests_helpers::collect(&dest, &dest, &mut restored);
        original.retain(|(rel, _)| {
            !(rel.starts_with("target/") || rel.starts_with("node_modules/") || rel == ".DS_Store")
        });
        original.sort();
        restored.sort();
        assert_eq!(original, restored);
    }

    #[test]
    fn warm_recapture_writes_zero_new_objects() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);

        let s = store(&tmp);
        let m = matcher();
        let (id1, cold) = s.put_tree(&root, &m).unwrap();
        assert!(cold.new_objects > 0 && cold.new_chunks > 0);

        let (id2, warm) = s.put_tree(&root, &m).unwrap();
        assert_eq!(id1, id2, "identical tree must dedup to one id");
        assert_eq!(warm.new_objects, 0);
        assert_eq!(warm.new_chunks, 0);
        assert_eq!(warm.bytes_written, 0);
        assert!(warm.reused_objects + warm.reused_chunks > 0);
    }

    #[test]
    fn editing_one_file_restores_only_that_files_path() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);

        let s = store(&tmp);
        let m = matcher();
        let (_id, _cold) = s.put_tree(&root, &m).unwrap();

        // Edit one region of the big binary asset.
        let mut data = fs::read(root.join("assets/data.bin")).unwrap();
        let mid = data.len() / 2;
        for b in &mut data[mid..mid + 8] {
            *b ^= 0xFF;
        }
        fs::write(root.join("assets/data.bin"), &data).unwrap();

        let (_id2, warm) = s.put_tree(&root, &m).unwrap();
        // Exactly one chunk changed in the edited file.
        assert_eq!(
            warm.new_chunks, 1,
            "single-region edit should re-store one chunk, got {warm:?}"
        );
        // New objects: the edited blob + the two trees on its path-to-root
        // (assets/ and the root) whose entries now reference new ids. The other
        // subtree (src/, src/util/) and its blobs are untouched.
        assert!(
            warm.new_objects <= 3,
            "only the edited file's blob and its ancestor trees should change, got {warm:?}"
        );
        assert!(warm.reused_chunks > 0 && warm.reused_objects > 0);
    }

    #[test]
    fn objects_survive_a_repack() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);

        let s = store(&tmp);
        let profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&profile);
        let (snap, _root, _stats) = s.capture_snapshot(&root, &m, profile.hash(), None).unwrap();

        // Pack everything, then read + materialize entirely through the pack.
        let packed = s.backend().repack().unwrap();
        assert!(packed > 0);

        let dest = tmp.path().join("restored_from_pack");
        s.materialize_snapshot(&snap, &dest).unwrap();
        for rel in ["Cargo.toml", "src/main.rs", "assets/data.bin"] {
            assert_eq!(
                fs::read(dest.join(rel)).unwrap(),
                fs::read(root.join(rel)).unwrap()
            );
        }

        // And a warm re-capture after repacking still writes nothing new.
        let (_id, warm) = s.put_tree(&root, &m).unwrap();
        assert_eq!(warm.new_objects, 0);
        assert_eq!(warm.new_chunks, 0);
    }

    #[test]
    fn snapshot_identity_depends_on_ignore_profile() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);
        let s = store(&tmp);

        let default_profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&default_profile);
        let (root_tree, _) = s.put_tree(&root, &m).unwrap();

        let (snap_a, _) = s
            .put_snapshot(root_tree, default_profile.hash(), None)
            .unwrap();
        // Same tree, different ignore-profile hash => different snapshot id.
        let other = IgnoreProfile::from_patterns(["custom/"]).unwrap();
        let (snap_b, _) = s.put_snapshot(root_tree, other.hash(), None).unwrap();
        assert_ne!(snap_a, snap_b);
    }

    #[test]
    fn unknown_generation_object_is_rejected_by_get() {
        use spork_hash::HashTag;

        let tmp = TempDir::new().unwrap();
        let s = LooseStore::open(tmp.path()).unwrap();
        let h = s.put(HashTag::CURRENT, ObjKind::Chunk, b"payload").unwrap();

        // Tamper the on-disk header's generation byte to a future generation.
        let hex = h.to_hex();
        let path = tmp.path().join("objects").join(&hex[..2]).join(&hex[2..]);
        let mut bytes = fs::read(&path).unwrap();
        bytes[6] = 42; // generation byte (see header layout)
        fs::write(&path, &bytes).unwrap();

        let err = s.get(&h).unwrap_err();
        assert!(matches!(err, CasError::UnknownGeneration(42)));
    }

    #[test]
    fn snapshot_hash_is_deterministic_across_captures_and_stores() {
        // Parallel hashing + batched fsync must not change content identity: a
        // given input always yields the same snapshot id, on repeated captures
        // and across independent stores. This is the determinism guard for the
        // parallel tree walk (entries are sorted by name regardless of which
        // file finishes hashing first).
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);
        let profile = IgnoreProfile::default_profile();

        // Capture the same tree many times into a fresh store each time; the
        // snapshot id (and root tree id) must be byte-stable every time despite
        // nondeterministic parallel completion order.
        let mut ids = Vec::new();
        for _ in 0..6 {
            let store_dir = TempDir::new().unwrap();
            let s = store(&store_dir);
            let m = IgnoreMatcher::new(&profile);
            let (snap, root_tree, _stats) =
                s.capture_snapshot(&root, &m, profile.hash(), None).unwrap();
            ids.push((snap, root_tree));
        }
        let first = ids[0];
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(*id, first, "capture {i} produced a different id: {id:?}");
        }

        // And re-capturing into the *same* store still yields the same id and
        // writes nothing new (warm dedup holds through the batched path).
        let s = store(&tmp);
        let m = IgnoreMatcher::new(&profile);
        let (snap1, root1, _cold) = s.capture_snapshot(&root, &m, profile.hash(), None).unwrap();
        let (snap2, root2, warm) = s.capture_snapshot(&root, &m, profile.hash(), None).unwrap();
        assert_eq!((snap1, root1), (snap2, root2));
        assert_eq!((snap1, root1), first);
        assert_eq!(warm.new_objects, 0);
        assert_eq!(warm.new_chunks, 0);
        assert_eq!(warm.bytes_written, 0);
    }

    #[test]
    fn put_tree_commits_new_objects_durably_and_readably() {
        // After a bulk put_tree returns, every object it reported as new must be
        // durable and readable through the backend — the write-objects-then-log
        // invariant F1 relies on. With the batched path the new objects live in a
        // packfile; get/has must find them transparently.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);
        let s = store(&tmp);
        let m = matcher();

        let (tree_id, cold) = s.put_tree(&root, &m).unwrap();
        assert!(cold.new_objects > 0 && cold.new_chunks > 0);

        // The whole graph reachable from the root tree is present and the tree
        // round-trips byte-for-byte through the (now packed) objects.
        let dest = tmp.path().join("restored");
        s.materialize_tree(&tree_id, &dest).unwrap();
        for rel in ["Cargo.toml", "src/main.rs", "assets/data.bin"] {
            assert_eq!(
                fs::read(dest.join(rel)).unwrap(),
                fs::read(root.join(rel)).unwrap()
            );
        }
        assert!(s.backend().has(&tree_id).unwrap());
    }

    #[test]
    fn discarded_bulk_batch_leaves_a_consistent_store() {
        // Crash mid-bulk safety: a batch that never reaches the durability
        // barrier just leaves the store missing objects, never corrupt or
        // dangling. We simulate "the batch was discarded" by computing the ids a
        // capture *would* produce against a fresh sibling store, then asserting
        // the original (un-committed) store reports them absent and is otherwise
        // perfectly usable — a retry re-creates them idempotently.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("proj");
        build_project(&root);

        // The crashed store: empty, never captured.
        let crashed = store(&tmp);

        // A sibling store that *did* capture, to learn the ids involved.
        let sib_dir = TempDir::new().unwrap();
        let sib = store(&sib_dir);
        let (snap, root_tree, _stats) = {
            let profile = IgnoreProfile::default_profile();
            let m = IgnoreMatcher::new(&profile);
            sib.capture_snapshot(&root, &m, profile.hash(), None)
                .unwrap()
        };

        // The discarded batch means those objects are simply absent in the
        // crashed store — not half-written, not dangling.
        assert!(!crashed.backend().has(&snap).unwrap());
        assert!(!crashed.backend().has(&root_tree).unwrap());
        assert!(matches!(
            crashed.read_snapshot(&snap),
            Err(CasError::NotFound(_))
        ));

        // Retry is idempotent: re-capturing yields the SAME ids and a fully
        // readable store.
        let profile = IgnoreProfile::default_profile();
        let m = IgnoreMatcher::new(&profile);
        let (snap2, root2, _stats) = crashed
            .capture_snapshot(&root, &m, profile.hash(), None)
            .unwrap();
        assert_eq!((snap2, root2), (snap, root_tree));
        let dest = tmp.path().join("out");
        crashed.materialize_snapshot(&snap2, &dest).unwrap();
        assert_eq!(
            fs::read(dest.join("Cargo.toml")).unwrap(),
            fs::read(root.join("Cargo.toml")).unwrap()
        );
    }

    #[test]
    fn identical_content_two_stores_share_the_same_blake3() {
        // The dedup-by-hash invariant holds across independent stores: the id is a
        // pure function of content, not of the store instance.
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let s1 = store(&tmp1);
        let s2 = store(&tmp2);
        let data = pseudo_random(2 * 1024 * 1024, 31);
        let (id1, _) = s1.put_blob_bytes(&data).unwrap();
        let (id2, _) = s2.put_blob_bytes(&data).unwrap();
        assert_eq!(id1, id2);
    }
}

#[cfg(test)]
mod property_tests {
    //! Property tests (proptest): random directory trees round-trip byte-for-byte
    //! through capture + materialize, and capture is idempotent.

    use super::*;
    use proptest::prelude::*;
    use spork_ignore::{IgnoreMatcher, IgnoreProfile};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// A generated filesystem entry: either a file with bytes, or a directory of
    /// named children. Names are constrained to safe, non-ignored identifiers so
    /// the generator never collides with the default ignore profile.
    #[derive(Debug, Clone)]
    enum Entry {
        File(Vec<u8>),
        Dir(BTreeMap<String, Entry>),
    }

    /// A safe component name: lowercase letters/digits, never matching the default
    /// ignore profile (no dotfiles, no `node_modules`, etc.).
    fn name_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-z][a-z0-9_]{0,7}").unwrap()
    }

    /// File bytes: occasionally large enough to exercise FastCDC multi-chunking.
    fn file_bytes_strategy() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            // Small files (the common case).
            proptest::collection::vec(any::<u8>(), 0..512),
            // Occasionally a multi-megabyte file to drive chunking.
            (1usize..3).prop_map(|mb| {
                let n = mb * 1024 * 1024 + 12345;
                let mut state = (mb as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) + 1;
                (0..n)
                    .map(|_| {
                        state ^= state >> 12;
                        state ^= state << 25;
                        state ^= state >> 27;
                        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
                    })
                    .collect::<Vec<u8>>()
            }),
        ]
    }

    /// A recursive tree generator with bounded depth and breadth.
    fn entry_strategy() -> impl Strategy<Value = Entry> {
        let leaf = file_bytes_strategy().prop_map(Entry::File);
        leaf.prop_recursive(3, 24, 5, |inner| {
            proptest::collection::btree_map(name_strategy(), inner, 0..5).prop_map(Entry::Dir)
        })
    }

    /// A top-level directory: a map of named entries (so the root is a directory).
    fn dir_strategy() -> impl Strategy<Value = BTreeMap<String, Entry>> {
        proptest::collection::btree_map(name_strategy(), entry_strategy(), 0..5)
    }

    /// Write a generated directory map to disk under `root`.
    fn write_dir(root: &Path, entries: &BTreeMap<String, Entry>) {
        fs::create_dir_all(root).unwrap();
        for (name, entry) in entries {
            let path = root.join(name);
            match entry {
                Entry::File(bytes) => fs::write(&path, bytes).unwrap(),
                Entry::Dir(children) => write_dir(&path, children),
            }
        }
    }

    proptest! {
        // Property runs are I/O-heavy (they write multi-MiB files), so keep the
        // case count modest but meaningful.
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn random_tree_round_trips_and_is_idempotent(entries in dir_strategy()) {
            let tmp = TempDir::new().unwrap();
            let src = tmp.path().join("src");
            write_dir(&src, &entries);

            let store = ObjectStore::new(LooseStore::open(tmp.path()).unwrap());
            let matcher = IgnoreMatcher::new(&IgnoreProfile::default_profile());

            // Cold capture.
            let (tree_id, _cold) = store.put_tree(&src, &matcher).unwrap();

            // Idempotent: a second capture of the same tree writes nothing new.
            let (tree_id2, warm) = store.put_tree(&src, &matcher).unwrap();
            prop_assert_eq!(tree_id, tree_id2);
            prop_assert_eq!(warm.new_objects, 0);
            prop_assert_eq!(warm.new_chunks, 0);
            prop_assert_eq!(warm.bytes_written, 0);

            // Materialize and compare byte-for-byte.
            let dest = tmp.path().join("dest");
            store.materialize_tree(&tree_id, &dest).unwrap();

            let mut original = Vec::new();
            let mut restored = Vec::new();
            super::integration_tests_helpers::collect(&src, &src, &mut original);
            super::integration_tests_helpers::collect(&dest, &dest, &mut restored);
            original.sort();
            restored.sort();
            prop_assert_eq!(original, restored);
        }
    }
}

#[cfg(test)]
mod integration_tests_helpers {
    //! Shared helper used by both the integration and property test modules so
    //! file-collection logic lives in exactly one place.

    use std::fs;
    use std::path::Path;

    /// Recursively collect `(relative_path, bytes)` for every regular file under
    /// `dir`, relative to `base`.
    pub(super) fn collect(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                collect(base, &path, out);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
}
