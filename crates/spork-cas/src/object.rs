//! The content-layer object model: [`Chunk`], [`Blob`], [`Tree`], [`Snapshot`].
//!
//! These are Spork's Git-analogue objects (DESIGN.md §6.1, §10.1), hashed with
//! BLAKE3 rather than SHA-1. The graph is:
//!
//! ```text
//! Snapshot ──root_tree──▶ Tree ──entries──▶ Tree | Blob
//!                                            Blob ──chunks──▶ Chunk (raw bytes)
//! ```
//!
//! - A **[`Chunk`]** is the raw bytes of a content-defined chunk; it is stored
//!   as-is and addressed by the BLAKE3 digest of those bytes. Chunks are the only
//!   objects that are *not* canonical-JSON encoded — they are opaque payloads.
//! - A **[`Blob`]** is a file: an ordered, non-empty list of chunk digests.
//!   Identical files produce identical chunk lists and therefore an identical
//!   blob id; a one-region edit in a large file changes exactly one chunk digest
//!   and reuses the rest (sub-file dedup, DESIGN.md §10.4).
//! - A **[`Tree`]** is a directory manifest: entries sorted by name so identical
//!   subtrees dedup to a single hash (DESIGN.md §10.1).
//! - A **[`Snapshot`]** is the commit analogue: it binds a `root_tree` to the
//!   `ignore_profile_hash` that produced it and an optional imported
//!   `git_parent_commit`, plus minimal deterministic [`CreationMeta`].
//!
//! # Identity (frozen)
//!
//! Every JSON-encoded object's id is `BLAKE3(canonical(object))`, where
//! `canonical` is the frozen [`spork_canon`] encoder. Each persisted struct
//! carries a `v` schema-version field (constraint C5) that participates in the
//! hash, so a schema change is a loud identity change, never an in-place rehash.
//! The id of a [`Chunk`] is simply `BLAKE3(bytes)`.
//!
//! Design references: DESIGN.md §6.1 (the two-layer content/timeline model),
//! §10.1 (three-layer architecture / object model), §10.4 (FastCDC sub-file
//! dedup), §10.5 (`ignore_profile_hash` baked into snapshot identity).

use serde::{Deserialize, Serialize};

use spork_canon::SERIALIZATION_VERSION;
use spork_hash::Hash;

use crate::error::{CasError, Result};

/// The schema version stamped on a freshly created [`Blob`].
pub const BLOB_VERSION: u16 = 1;
/// The schema version stamped on a freshly created [`Tree`].
pub const TREE_VERSION: u16 = 1;
/// The schema version stamped on a freshly created [`Snapshot`].
pub const SNAPSHOT_VERSION: u16 = 1;

/// The kind of a stored object.
///
/// This discriminator is recorded in every stored object's self-describing
/// header (see [`crate::header`]) so the store can tell, from the bytes alone,
/// how to interpret a payload (raw chunk bytes vs. canonical-JSON object). It is
/// a frozen v1 contract: new kinds are *added* as variants (the compiler then
/// flags every `match` that must consider them), never reinterpreted.
///
/// `#[non_exhaustive]` is deliberately omitted: a new kind is an additive change
/// that downstream exhaustive matches should be forced to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ObjKind {
    /// Raw bytes of a content-defined chunk (opaque, not JSON-encoded).
    Chunk,
    /// A file: an ordered list of chunk digests ([`Blob`]).
    Blob,
    /// A directory manifest ([`Tree`]).
    Tree,
    /// A codebase-state snapshot, the commit analogue ([`Snapshot`]).
    Snapshot,
}

impl ObjKind {
    /// The compact, stable on-disk code for this kind, used in object headers.
    ///
    /// These codes are part of the frozen header wire format; the mapping is
    /// fixed here once so a header written today reads back forever.
    #[must_use]
    pub(crate) const fn code(self) -> u8 {
        match self {
            ObjKind::Chunk => 1,
            ObjKind::Blob => 2,
            ObjKind::Tree => 3,
            ObjKind::Snapshot => 4,
        }
    }

    /// Parse an on-disk kind code back into an [`ObjKind`].
    ///
    /// Returns `None` for an unrecognized code; the header reader maps that to
    /// [`CasError::MalformedHeader`].
    #[must_use]
    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(ObjKind::Chunk),
            2 => Some(ObjKind::Blob),
            3 => Some(ObjKind::Tree),
            4 => Some(ObjKind::Snapshot),
            _ => None,
        }
    }

    /// A short human label for diagnostics and the CLI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ObjKind::Chunk => "chunk",
            ObjKind::Blob => "blob",
            ObjKind::Tree => "tree",
            ObjKind::Snapshot => "snapshot",
        }
    }
}

/// A file as an ordered list of content-defined chunk digests.
///
/// A blob has **at least one** chunk (an empty file is a single empty chunk),
/// so its identity is well-defined and re-assembly is unambiguous. Two files
/// with identical bytes chunk identically and thus share a blob id; a localized
/// edit to a large file rewrites exactly the chunk(s) it touched, leaving the
/// rest reusable (DESIGN.md §10.4).
///
/// The id is `BLAKE3(canonical(Blob))`. The `v` field participates in that hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    /// Schema version (hashed). New blobs use [`BLOB_VERSION`].
    pub v: u16,
    /// The ordered chunk digests that, concatenated, reconstruct the file.
    /// Always non-empty.
    pub chunks: Vec<Hash>,
}

impl Blob {
    /// Construct a blob from its ordered chunk digests.
    ///
    /// # Errors
    /// Returns [`CasError::Decode`] if `chunks` is empty — a blob must reference
    /// at least one chunk (an empty file is represented by a single empty chunk,
    /// produced by the high-level store).
    pub fn new(chunks: Vec<Hash>) -> Result<Self> {
        if chunks.is_empty() {
            return Err(CasError::Decode(
                "a blob must have at least one chunk".to_string(),
            ));
        }
        Ok(Blob {
            v: BLOB_VERSION,
            chunks,
        })
    }

    /// The canonical bytes whose BLAKE3 digest is this blob's id.
    ///
    /// # Errors
    /// Returns [`CasError::Canon`] if canonical encoding fails (impossible for
    /// the v1 schema; present for evolution safety).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        spork_canon::canonicalize(self).map_err(|e| CasError::Canon(e.to_string()))
    }
}

/// The type of a [`TreeEntry`].
///
/// Mirrors the filesystem entry kinds Spork captures. A frozen v1 set; new kinds
/// are added as variants behind this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EntryKind {
    /// A regular file. `target` is a [`Blob`] id.
    File,
    /// A subdirectory. `target` is a [`Tree`] id.
    Dir,
    /// A symbolic link. `target` is a [`Blob`] id holding the link's target path
    /// bytes (so the link text is itself content-addressed and dedupable).
    Symlink,
}

impl EntryKind {
    /// A short human label for diagnostics and the CLI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            EntryKind::File => "file",
            EntryKind::Dir => "dir",
            EntryKind::Symlink => "symlink",
        }
    }
}

/// One named child of a [`Tree`].
///
/// The `target` digest points at a [`Blob`] (for files and symlinks) or a
/// [`Tree`] (for directories), per `kind`. The `mode` carries the POSIX
/// permission bits so a materialized tree restores executable bits and the like;
/// it participates in tree identity (a chmod is a real content change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    /// The entry's name (a single path component, valid UTF-8).
    pub name: String,
    /// Whether this entry is a file, directory, or symlink.
    pub kind: EntryKind,
    /// POSIX mode bits (permissions). Part of identity.
    pub mode: u32,
    /// The digest of the child object ([`Blob`] or [`Tree`]).
    pub target: Hash,
}

/// A directory manifest: a sorted list of named children.
///
/// Entries are held **sorted ascending by `name` (UTF-8 bytes)** so that two
/// directories with the same contents — regardless of the order the walk
/// produced them — encode to identical canonical bytes and share a tree id. This
/// is what makes identical subtrees dedup to a single object (DESIGN.md §10.1).
///
/// The id is `BLAKE3(canonical(Tree))`; the `v` field participates in that hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    /// Schema version (hashed). New trees use [`TREE_VERSION`].
    pub v: u16,
    /// The directory's children, sorted ascending by `name`.
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// Construct a tree from entries, sorting them by name for canonical identity.
    ///
    /// The input order is irrelevant: entries are sorted ascending by their UTF-8
    /// byte sequence so the resulting object is identity-stable. (Duplicate names
    /// are a caller error and are not silently merged; the high-level store never
    /// produces them because a directory cannot hold two entries with the same
    /// name.)
    #[must_use]
    pub fn new(mut entries: Vec<TreeEntry>) -> Self {
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        Tree {
            v: TREE_VERSION,
            entries,
        }
    }

    /// The canonical bytes whose BLAKE3 digest is this tree's id.
    ///
    /// # Errors
    /// Returns [`CasError::Canon`] if canonical encoding fails (impossible for
    /// the v1 schema; present for evolution safety).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        spork_canon::canonicalize(self).map_err(|e| CasError::Canon(e.to_string()))
    }
}

/// Minimal, deterministic creation metadata recorded on a [`Snapshot`].
///
/// Deliberately tiny and free of wall-clock or environment inputs: a snapshot's
/// identity must depend only on *what was captured*, not *when or where*. The
/// only field is the `serialization_version` in force when the snapshot was made,
/// which ties the snapshot to the frozen canonical encoding (DESIGN.md A.1).
/// Mutable, non-identity bookkeeping (timestamps, author, origin) belongs to the
/// event-log node envelope (F1), not to the content-addressed snapshot object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreationMeta {
    /// The canonical [`spork_canon::SERIALIZATION_VERSION`] used to encode this
    /// snapshot. Part of identity.
    pub serialization_version: u16,
}

impl CreationMeta {
    /// The current, deterministic metadata for a freshly created snapshot.
    #[must_use]
    pub const fn current() -> Self {
        CreationMeta {
            serialization_version: SERIALIZATION_VERSION,
        }
    }
}

impl Default for CreationMeta {
    fn default() -> Self {
        CreationMeta::current()
    }
}

/// The commit analogue: a whole codebase state bound to a root tree.
///
/// A snapshot records the `root_tree` digest, the `ignore_profile_hash` that
/// produced the capture (so two snapshots taken under different exclusion
/// policies are distinct objects — DESIGN.md §6.1, §10.5), an optional
/// `git_parent_commit` (a lone SHA-1 *imported, never computed* by Spork, for
/// Git import/export lineage), and minimal [`CreationMeta`].
///
/// The id is `BLAKE3(canonical(Snapshot))`; the `v` field participates in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Schema version (hashed). New snapshots use [`SNAPSHOT_VERSION`].
    pub v: u16,
    /// The digest of the root [`Tree`] this snapshot captures.
    pub root_tree: Hash,
    /// An optional Git parent commit SHA-1, present only for imported state.
    ///
    /// This is a *lone, imported* value — Spork never computes a SHA-1. It is a
    /// 40-character lowercase hex string when present, but is intentionally typed
    /// as an opaque `String` because it belongs to a foreign hash space.
    pub git_parent_commit: Option<String>,
    /// The `ignore_profile_hash` (BLAKE3 of the canonical ignore profile) under
    /// which this snapshot was captured. Part of identity.
    pub ignore_profile_hash: Hash,
    /// Minimal, deterministic creation metadata.
    pub meta: CreationMeta,
}

impl Snapshot {
    /// Construct a snapshot binding a root tree, ignore profile, and optional
    /// imported Git parent. Stamps the current schema version and metadata.
    #[must_use]
    pub fn new(
        root_tree: Hash,
        ignore_profile_hash: Hash,
        git_parent_commit: Option<String>,
    ) -> Self {
        Snapshot {
            v: SNAPSHOT_VERSION,
            root_tree,
            git_parent_commit,
            ignore_profile_hash,
            meta: CreationMeta::current(),
        }
    }

    /// The canonical bytes whose BLAKE3 digest is this snapshot's id.
    ///
    /// # Errors
    /// Returns [`CasError::Canon`] if canonical encoding fails (impossible for
    /// the v1 schema; present for evolution safety).
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        spork_canon::canonicalize(self).map_err(|e| CasError::Canon(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;

    fn h(seed: &[u8]) -> Hash {
        hash_bytes(seed)
    }

    #[test]
    fn objkind_code_round_trips() {
        for k in [
            ObjKind::Chunk,
            ObjKind::Blob,
            ObjKind::Tree,
            ObjKind::Snapshot,
        ] {
            assert_eq!(ObjKind::from_code(k.code()), Some(k));
        }
        assert_eq!(ObjKind::from_code(0), None);
        assert_eq!(ObjKind::from_code(99), None);
    }

    #[test]
    fn blob_rejects_empty_chunk_list() {
        assert!(matches!(Blob::new(vec![]), Err(CasError::Decode(_))));
        assert!(Blob::new(vec![h(b"c")]).is_ok());
    }

    #[test]
    fn blob_id_is_stable_and_order_sensitive() {
        let a = Blob::new(vec![h(b"1"), h(b"2")]).unwrap();
        let b = Blob::new(vec![h(b"1"), h(b"2")]).unwrap();
        assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
        // Chunk order matters (file content order matters).
        let c = Blob::new(vec![h(b"2"), h(b"1")]).unwrap();
        assert_ne!(a.canonical_bytes().unwrap(), c.canonical_bytes().unwrap());
    }

    #[test]
    fn tree_sorts_entries_by_name() {
        let mk = |name: &str| TreeEntry {
            name: name.to_string(),
            kind: EntryKind::File,
            mode: 0o644,
            target: h(name.as_bytes()),
        };
        let unsorted = Tree::new(vec![mk("b"), mk("a"), mk("c")]);
        let sorted = Tree::new(vec![mk("a"), mk("b"), mk("c")]);
        // Construction order is irrelevant to identity.
        assert_eq!(
            unsorted.canonical_bytes().unwrap(),
            sorted.canonical_bytes().unwrap()
        );
        assert_eq!(
            unsorted.entries.iter().map(|e| &e.name).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn identical_subtrees_share_bytes() {
        // Two trees with identical contents must encode identically (dedup).
        let mk = || {
            Tree::new(vec![TreeEntry {
                name: "x.txt".to_string(),
                kind: EntryKind::File,
                mode: 0o644,
                target: h(b"x"),
            }])
        };
        assert_eq!(
            mk().canonical_bytes().unwrap(),
            mk().canonical_bytes().unwrap()
        );
    }

    #[test]
    fn snapshot_identity_includes_ignore_profile_and_root() {
        let s1 = Snapshot::new(h(b"root"), h(b"ignoreA"), None);
        let s2 = Snapshot::new(h(b"root"), h(b"ignoreB"), None);
        // Different ignore profile => different snapshot identity.
        assert_ne!(s1.canonical_bytes().unwrap(), s2.canonical_bytes().unwrap());

        let s3 = Snapshot::new(h(b"root"), h(b"ignoreA"), None);
        assert_eq!(s1.canonical_bytes().unwrap(), s3.canonical_bytes().unwrap());

        // git_parent participates in identity too.
        let s4 = Snapshot::new(h(b"root"), h(b"ignoreA"), Some("abc".to_string()));
        assert_ne!(s1.canonical_bytes().unwrap(), s4.canonical_bytes().unwrap());
    }

    #[test]
    fn creation_meta_is_deterministic() {
        assert_eq!(CreationMeta::current(), CreationMeta::default());
        assert_eq!(
            CreationMeta::current().serialization_version,
            SERIALIZATION_VERSION
        );
    }

    #[test]
    fn objects_serde_round_trip() {
        let blob = Blob::new(vec![h(b"a"), h(b"b")]).unwrap();
        let back: Blob = serde_json::from_slice(&serde_json::to_vec(&blob).unwrap()).unwrap();
        assert_eq!(blob, back);

        let tree = Tree::new(vec![TreeEntry {
            name: "f".to_string(),
            kind: EntryKind::Dir,
            mode: 0o755,
            target: h(b"t"),
        }]);
        let back: Tree = serde_json::from_slice(&serde_json::to_vec(&tree).unwrap()).unwrap();
        assert_eq!(tree, back);

        let snap = Snapshot::new(h(b"r"), h(b"i"), Some("deadbeef".to_string()));
        let back: Snapshot = serde_json::from_slice(&serde_json::to_vec(&snap).unwrap()).unwrap();
        assert_eq!(snap, back);
    }
}
