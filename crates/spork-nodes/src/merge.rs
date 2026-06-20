//! The **Merge** built-in node type and its 3-way reconciliation logic
//! (DESIGN.md §6.5, §7.1, A.4).
//!
//! A Merge is an *explicit, recorded* operation — branches are never auto-merged
//! (DESIGN.md §6.5). It is computed as a standard three-way file merge against
//! the **nearest common ancestor** snapshot, and its outcome is one of:
//!
//! - a clean, materializable Merge node (a [`Family::Mutating`] node that
//!   `owns_snapshot`) carrying a merged root-tree `contentRef` and an empty
//!   [`ConflictResolution`](spork_merge::ConflictResolution); or
//! - a **conflict set** ([`MergeOutcome::Conflicts`]) returned as *data* for the
//!   F3-UI three-way resolver — **no half-node is built** (DESIGN.md A.4: "a
//!   `branch.merge` that hits unresolvable hunks returns a `conflictSet` rather
//!   than creating a node").
//!
//! A Merge node is therefore *always* in a clean, materializable state once
//! created, preserving "click a node, see exact state" (DESIGN.md A.4).
//!
//! # The reconciliation (file-level three-way)
//!
//! Reconciliation is over the content-addressed [`Tree`](spork_cas::Tree)
//! objects, keyed by repository-relative path:
//!
//! | base | ours | theirs | result |
//! |------|------|--------|--------|
//! | B    | B    | T      | take theirs (only they changed) |
//! | B    | O    | B      | take ours (only we changed) |
//! | B    | O    | O      | take either (both made the same change) |
//! | B    | O    | T (O≠T)| **conflict** (both changed differently) |
//! | —    | O    | T (O≠T)| **conflict** (both added different content) |
//! | B    | —    | B      | delete (we deleted, they didn't change) |
//! | B    | —    | T (T≠B)| **conflict** (we deleted, they changed) |
//!
//! Equal targets are equal by content hash, so the comparison is exact and
//! deterministic — the property a cached, replayable merge relies on (DESIGN.md
//! §6.1). The merged tree is written back to the CAS so the Merge node binds a
//! real, materializable `contentRef`.
//!
//! Conversation merge has no clean three-way analogue; the synthetic-transcript
//! schema for it is frozen in [`spork_merge::synthesize`] and consumed by the
//! daemon — this module reconciles *code* and records the resolution.
//!
//! Design references: DESIGN.md §6.5 (explicit merge, three-way against NCA),
//! §7.1 (taxonomy), A.4 (conflict surfacing, clean-or-conflict-set).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spork_cas::{EntryKind, ObjectStore, StorageBackend};
use spork_graph::EdgeType;
use spork_hash::Hash;
use spork_merge::{ConflictResolution, ResolutionChoice};
use spork_registry::{Family, NodeTypeDescriptor, Port, PortDirection, PortKind, StalenessRule};

use crate::descriptor::{type_version, SNAPSHOT_OUT_PORT_NAME};
use crate::error::NodesError;

/// The stable type id (the node `kind`) of the Merge built-in.
pub const MERGE_KIND: &str = "merge";

/// The schema version stamped on a freshly built [`MergePayload`].
pub const MERGE_PAYLOAD_VERSION: u16 = 1;

/// One conflicting path in a three-way merge, returned as data for the F3-UI
/// resolver (DESIGN.md A.4).
///
/// Carries the path and the three content references the resolver presents in its
/// base / ours / theirs view. A `None` ref means the file is absent on that side
/// (an add/delete conflict). No resolution is *chosen* here — that is the UI's
/// job; this is the conflict *set* the merge returns instead of a half-node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileConflict {
    /// The repository-relative path that conflicts.
    pub path: String,
    /// The file's content ref in the common ancestor, or `None` if absent there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<Hash>,
    /// The file's content ref on our side, or `None` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ours: Option<Hash>,
    /// The file's content ref on their side, or `None` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theirs: Option<Hash>,
}

/// The outcome of a three-way reconciliation (DESIGN.md A.4).
///
/// Either a clean merge — a merged root-tree `contentRef` plus the (empty)
/// [`ConflictResolution`] — or a conflict set returned as data. A conflicting
/// merge builds **no** node (DESIGN.md A.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// A clean merge: a materializable merged tree and its resolution record.
    Clean {
        /// The content hash of the merged root tree (the Merge node's
        /// `contentRef`).
        merged_tree: Hash,
        /// The resolution record (empty `resolutions` for a fully-clean merge).
        resolution: ConflictResolution,
    },
    /// A conflicting merge: the per-file conflict set, no node built.
    Conflicts(Vec<FileConflict>),
}

impl MergeOutcome {
    /// Whether this outcome is a clean merge.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        matches!(self, MergeOutcome::Clean { .. })
    }

    /// The conflict set, empty for a clean merge.
    #[must_use]
    pub fn conflicts(&self) -> &[FileConflict] {
        match self {
            MergeOutcome::Conflicts(c) => c,
            MergeOutcome::Clean { .. } => &[],
        }
    }
}

/// The schema-versioned payload of a Merge node (DESIGN.md §7.1).
///
/// `base_ref` is the nearest-common-ancestor tree the merge was computed against;
/// `conflict_resolution` is the stored record of how every conflicting file was
/// settled (empty for a clean merge). The merged codebase `contentRef` is bound
/// via the descriptor's SnapshotRef out-port and carried on the node envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergePayload {
    /// The schema version of this payload ([`MERGE_PAYLOAD_VERSION`]).
    pub schema_version: u16,
    /// The nearest-common-ancestor root tree the merge was computed against.
    pub base_ref: Hash,
    /// The stored per-file conflict-resolution record (empty for a clean merge).
    pub conflict_resolution: ConflictResolution,
}

impl MergePayload {
    /// Construct a Merge payload for a merge computed against `base_ref`, carrying
    /// `resolution`.
    #[must_use]
    pub fn new(base_ref: Hash, resolution: ConflictResolution) -> Self {
        MergePayload {
            schema_version: MERGE_PAYLOAD_VERSION,
            base_ref,
            conflict_resolution: resolution,
        }
    }

    /// Render this payload to the JSON `payload` value the daemon stores.
    ///
    /// # Errors
    /// [`NodesError::Serialize`](crate::NodesError::Serialize) on an encoding
    /// failure.
    pub fn to_value(&self) -> crate::Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// A leaf file entry from a flattened tree: its kind, mode, and target blob.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileEntry {
    kind: EntryKind,
    mode: u32,
    target: Hash,
}

/// Compute a three-way reconciliation of two root trees against their nearest
/// common ancestor, over the content store `store`.
///
/// `base` is the NCA root tree, `ours` and `theirs` are the two sides' root
/// trees. The result is either a [`MergeOutcome::Clean`] with a merged tree
/// written back to the store, or a [`MergeOutcome::Conflicts`] conflict set (no
/// tree written) — the clean-or-conflict-set contract of DESIGN.md A.4.
///
/// The merge is file-level: a path is a conflict only when both sides changed it
/// to *different* content (or both added different content, or one deleted while
/// the other changed). When the result is clean, the merged tree is genuinely
/// materializable — it is built from real, content-addressed file entries.
///
/// # Errors
/// [`NodesError::Cas`] if a tree cannot be read or the merged tree cannot be
/// written; [`NodesError::Merge`] if an input is structurally malformed.
pub fn three_way_merge<S: StorageBackend + Sync>(
    store: &ObjectStore<S>,
    base: &Hash,
    ours: &Hash,
    theirs: &Hash,
) -> crate::Result<MergeOutcome> {
    let base_files = flatten(store, base)?;
    let ours_files = flatten(store, ours)?;
    let theirs_files = flatten(store, theirs)?;

    // The union of every path that appears on any side, in deterministic order.
    let mut paths: BTreeMap<String, ()> = BTreeMap::new();
    for f in base_files
        .keys()
        .chain(ours_files.keys())
        .chain(theirs_files.keys())
    {
        paths.insert(f.clone(), ());
    }

    let mut merged: BTreeMap<String, FileEntry> = BTreeMap::new();
    let mut conflicts: Vec<FileConflict> = Vec::new();

    for path in paths.keys() {
        let b = base_files.get(path);
        let o = ours_files.get(path);
        let t = theirs_files.get(path);

        match reconcile_path(b, o, t) {
            PathResolution::Keep(entry) => {
                merged.insert(path.clone(), entry);
            }
            PathResolution::Delete => {
                // The file is absent in the merged result.
            }
            PathResolution::Conflict => {
                conflicts.push(FileConflict {
                    path: path.clone(),
                    base: b.map(|e| e.target),
                    ours: o.map(|e| e.target),
                    theirs: t.map(|e| e.target),
                });
            }
        }
    }

    if !conflicts.is_empty() {
        // Conflicting merge: return the conflict set, build NO node/tree
        // (DESIGN A.4).
        return Ok(MergeOutcome::Conflicts(conflicts));
    }

    // Clean merge: assemble and store the merged root tree so the Merge node
    // binds a real, materializable contentRef.
    let merged_tree = write_tree(store, &merged)?;
    Ok(MergeOutcome::Clean {
        merged_tree,
        resolution: ConflictResolution::new(Vec::new()),
    })
}

/// Apply a *pre-supplied* resolution to an otherwise-conflicting merge,
/// producing a clean, materializable merged tree (DESIGN.md A.4: a Merge node is
/// created once every conflict has a stored resolution).
///
/// `base`/`ours`/`theirs` are the three root trees; `resolution` supplies a
/// [`ResolutionChoice`] for every path that conflicts. A path resolved
/// [`ResolutionChoice::Ours`] / [`ResolutionChoice::Theirs`] takes that side's
/// content; [`ResolutionChoice::Merged`] takes the supplied content ref. Any
/// conflict left *unresolved* by `resolution` is returned as a (reduced)
/// conflict set rather than guessed.
///
/// # Errors
/// [`NodesError::Cas`] on a store failure; [`NodesError::Merge`] if a resolution
/// names a path that is not actually a conflict (a malformed resolution).
pub fn three_way_merge_with_resolution<S: StorageBackend + Sync>(
    store: &ObjectStore<S>,
    base: &Hash,
    ours: &Hash,
    theirs: &Hash,
    resolution: &ConflictResolution,
) -> crate::Result<MergeOutcome> {
    let outcome = three_way_merge(store, base, ours, theirs)?;
    let conflicts = match &outcome {
        // Already clean — a supplied resolution for a clean merge is a no-op, and
        // a clean merge with a *non-empty* resolution is a malformed input.
        MergeOutcome::Clean { merged_tree, .. } => {
            if resolution.resolutions.is_empty() {
                return Ok(outcome);
            }
            return Err(NodesError::Merge(format!(
                "resolution supplied for an already-clean merge (merged tree {})",
                merged_tree.to_hex()
            )));
        }
        MergeOutcome::Conflicts(c) => c.clone(),
    };

    // Re-derive the auto-merged (non-conflicting) entries, then overlay the
    // supplied resolutions for the conflicting paths.
    let base_files = flatten(store, base)?;
    let ours_files = flatten(store, ours)?;
    let theirs_files = flatten(store, theirs)?;

    let mut merged: BTreeMap<String, FileEntry> = BTreeMap::new();
    let mut paths: BTreeMap<String, ()> = BTreeMap::new();
    for f in base_files
        .keys()
        .chain(ours_files.keys())
        .chain(theirs_files.keys())
    {
        paths.insert(f.clone(), ());
    }
    let conflict_paths: BTreeMap<&str, &FileConflict> =
        conflicts.iter().map(|c| (c.path.as_str(), c)).collect();
    let resolved: BTreeMap<&str, &ResolutionChoice> = resolution
        .resolutions
        .iter()
        .map(|r| (r.path.as_str(), &r.chosen))
        .collect();

    let mut remaining: Vec<FileConflict> = Vec::new();

    for path in paths.keys() {
        if let Some(conflict) = conflict_paths.get(path.as_str()) {
            // A conflicting path: settle it from the supplied resolution.
            match resolved.get(path.as_str()) {
                Some(ResolutionChoice::Ours) => {
                    if let Some(e) = ours_files.get(path) {
                        merged.insert(path.clone(), e.clone());
                    }
                }
                Some(ResolutionChoice::Theirs) => {
                    if let Some(e) = theirs_files.get(path) {
                        merged.insert(path.clone(), e.clone());
                    }
                }
                Some(ResolutionChoice::Merged { content_ref }) => {
                    merged.insert(
                        path.clone(),
                        FileEntry {
                            kind: EntryKind::File,
                            mode: ours_files
                                .get(path)
                                .or_else(|| theirs_files.get(path))
                                .map(|e| e.mode)
                                .unwrap_or(0o644),
                            target: *content_ref,
                        },
                    );
                }
                None => remaining.push((*conflict).clone()),
            }
        } else {
            // A non-conflicting path: re-apply the automatic reconciliation.
            let b = base_files.get(path);
            let o = ours_files.get(path);
            let t = theirs_files.get(path);
            match reconcile_path(b, o, t) {
                PathResolution::Keep(entry) => {
                    merged.insert(path.clone(), entry);
                }
                PathResolution::Delete | PathResolution::Conflict => {}
            }
        }
    }

    // A resolution that named a non-conflicting path is malformed.
    for r in &resolution.resolutions {
        if !conflict_paths.contains_key(r.path.as_str()) {
            return Err(NodesError::Merge(format!(
                "resolution names {:?}, which is not a conflicting path",
                r.path
            )));
        }
    }

    if !remaining.is_empty() {
        return Ok(MergeOutcome::Conflicts(remaining));
    }

    let merged_tree = write_tree(store, &merged)?;
    Ok(MergeOutcome::Clean {
        merged_tree,
        resolution: resolution.clone(),
    })
}

/// The per-path decision a three-way reconciliation reaches.
enum PathResolution {
    /// Keep this file entry in the merged tree.
    Keep(FileEntry),
    /// The file is absent in the merged result (a clean delete).
    Delete,
    /// The path conflicts and must be resolved by the UI.
    Conflict,
}

/// Decide one path from its base/ours/theirs entries (the three-way table).
fn reconcile_path(
    base: Option<&FileEntry>,
    ours: Option<&FileEntry>,
    theirs: Option<&FileEntry>,
) -> PathResolution {
    match (base, ours, theirs) {
        // Present on no side — impossible (the path came from some side).
        (None, None, None) => PathResolution::Delete,
        // Added on one side only.
        (None, Some(o), None) => PathResolution::Keep(o.clone()),
        (None, None, Some(t)) => PathResolution::Keep(t.clone()),
        // Added on both sides: clean iff identical, else conflict.
        (None, Some(o), Some(t)) => {
            if o == t {
                PathResolution::Keep(o.clone())
            } else {
                PathResolution::Conflict
            }
        }
        // Existed at base, deleted on both sides: clean delete.
        (Some(_), None, None) => PathResolution::Delete,
        // Existed at base, deleted by us: clean iff they did not change it.
        (Some(b), None, Some(t)) => {
            if b == t {
                PathResolution::Delete
            } else {
                PathResolution::Conflict
            }
        }
        // Existed at base, deleted by them: clean iff we did not change it.
        (Some(b), Some(o), None) => {
            if b == o {
                PathResolution::Delete
            } else {
                PathResolution::Conflict
            }
        }
        // Existed on all three sides: the core three-way case.
        (Some(b), Some(o), Some(t)) => {
            if o == t {
                // Both ended at the same content (incl. neither changing it).
                PathResolution::Keep(o.clone())
            } else if b == o {
                // Only they changed it.
                PathResolution::Keep(t.clone())
            } else if b == t {
                // Only we changed it.
                PathResolution::Keep(o.clone())
            } else {
                // Both changed it differently.
                PathResolution::Conflict
            }
        }
    }
}

/// Recursively flatten a root tree into a path → leaf-entry map (files and
/// symlinks; directories are descended into).
fn flatten<S: StorageBackend + Sync>(
    store: &ObjectStore<S>,
    root: &Hash,
) -> crate::Result<BTreeMap<String, FileEntry>> {
    let mut out = BTreeMap::new();
    flatten_into(store, root, "", &mut out)?;
    Ok(out)
}

/// The recursion helper for [`flatten`], prefixing entries with `prefix`.
fn flatten_into<S: StorageBackend + Sync>(
    store: &ObjectStore<S>,
    tree_hash: &Hash,
    prefix: &str,
    out: &mut BTreeMap<String, FileEntry>,
) -> crate::Result<()> {
    let tree = store.read_tree(tree_hash)?;
    for entry in tree.entries {
        let path = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        match entry.kind {
            EntryKind::Dir => flatten_into(store, &entry.target, &path, out)?,
            EntryKind::File | EntryKind::Symlink => {
                out.insert(
                    path,
                    FileEntry {
                        kind: entry.kind,
                        mode: entry.mode,
                        target: entry.target,
                    },
                );
            }
        }
    }
    Ok(())
}

/// Re-assemble a path → leaf-entry map into a genuine, materializable CAS root
/// tree, returning its content hash.
///
/// The merged file set is materialized into a throwaway directory (each file's
/// bytes reassembled from its content-addressed blob, each symlink re-created
/// from its stored target-path bytes) and then re-captured with the public
/// [`ObjectStore::put_tree`] under a permissive ignore matcher. Capturing through
/// the store's own path is what makes the result a real [`Tree`] object that
/// [`ObjectStore::read_tree`] resolves and
/// [`ObjectStore::materialize_tree`](spork_cas::ObjectStore::materialize_tree)
/// restores — so the Merge node binds a genuinely materializable `contentRef`
/// (DESIGN.md A.4, "a Merge node is always in a clean, materializable state").
/// The temp directory is created and dropped entirely here; nothing persists.
fn write_tree<S: StorageBackend + Sync>(
    store: &ObjectStore<S>,
    files: &BTreeMap<String, FileEntry>,
) -> crate::Result<Hash> {
    let scratch = tempfile::tempdir()
        .map_err(|e| NodesError::Cas(format!("create merge scratch dir: {e}")))?;
    let root = scratch.path();

    for (path, entry) in files {
        let dest = root.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| NodesError::Cas(format!("create dir {parent:?}: {e}")))?;
        }
        match entry.kind {
            EntryKind::File => {
                let bytes = store.read_blob(&entry.target)?;
                std::fs::write(&dest, &bytes)
                    .map_err(|e| NodesError::Cas(format!("write merged file {dest:?}: {e}")))?;
                set_mode(&dest, entry.mode);
            }
            EntryKind::Symlink => {
                let target_bytes = store.read_blob(&entry.target)?;
                let target = String::from_utf8(target_bytes).map_err(|e| {
                    NodesError::Merge(format!("symlink {path:?} target is not UTF-8: {e}"))
                })?;
                create_symlink(&target, &dest)?;
            }
            EntryKind::Dir => {
                // `flatten` never yields a Dir leaf (it recurses into them), so a
                // Dir here would be a logic error; create the directory defensively.
                std::fs::create_dir_all(&dest)
                    .map_err(|e| NodesError::Cas(format!("create dir {dest:?}: {e}")))?;
            }
        }
    }

    let matcher = merge_ignore_matcher();
    let (hash, _stats) = store.put_tree(root, &matcher)?;
    Ok(hash)
}

/// A permissive ignore matcher for the merge re-capture.
///
/// The merged file set is already the reconciled, kept set (the inputs were
/// captured under the project's real ignore profile, and `flatten` only ever
/// surfaced their kept entries), so the re-capture must keep *everything* it is
/// handed — hence the default empty profile. Using the F0 ignore machinery (not a
/// hand-rolled walk) keeps the captured tree identical to what the rest of the
/// system produces (DESIGN.md §10.5).
fn merge_ignore_matcher() -> spork_ignore::IgnoreMatcher {
    // An empty pattern set keeps every materialized entry — `from_patterns` over
    // an empty iterator is a valid, permissive profile.
    let profile = spork_ignore::IgnoreProfile::from_patterns(std::iter::empty::<String>())
        .expect("an empty pattern set is a valid profile");
    spork_ignore::IgnoreMatcher::new(&profile)
}

/// Apply POSIX permission bits to a materialized file (Unix only; a no-op
/// elsewhere). Failure to chmod is non-fatal — the merged content is already
/// written — so it is best-effort.
#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

/// Non-Unix stub for [`set_mode`] (mode bits are not portable).
#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) {}

/// Create a symlink at `link` pointing at `target` (Unix).
#[cfg(unix)]
fn create_symlink(target: &str, link: &std::path::Path) -> crate::Result<()> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| NodesError::Cas(format!("create symlink {link:?} -> {target:?}: {e}")))
}

/// Create a symlink at `link` pointing at `target` (non-Unix: file symlink).
#[cfg(not(unix))]
fn create_symlink(target: &str, link: &std::path::Path) -> crate::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
            .map_err(|e| NodesError::Cas(format!("create symlink {link:?} -> {target:?}: {e}")))
    }
    #[cfg(not(windows))]
    {
        let _ = (target, link);
        Err(NodesError::Merge(
            "symlinks are not supported on this platform".into(),
        ))
    }
}

/// Build the Merge [`NodeTypeDescriptor`] (DESIGN.md §7.1).
///
/// A [`Family::Mutating`] type that `owns_snapshot` (≥2 parents) and exposes a
/// [`PortKind::SnapshotRef`] out-port (the contentRef rule, DESIGN.md §7.2). It
/// originates the structural edges plus [`EdgeType::MergeParent`] for its
/// second-and-later parents (DESIGN.md §6.3).
#[must_use]
pub fn descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: MERGE_KIND.to_string(),
        type_version: type_version(),
        family: Family::Mutating,
        owns_snapshot: true,
        payload_schema: json!({
            "type": "object",
            "properties": {
                "schema_version": { "type": "integer", "minimum": 1 },
                "base_ref": { "type": "string", "description": "nearest-common-ancestor tree hash" },
                "conflict_resolution": { "type": "object" }
            },
            "required": ["schema_version", "base_ref", "conflict_resolution"]
        }),
        result_schema: None,
        allowed_edges: vec![
            EdgeType::ParentChild,
            EdgeType::Branch,
            EdgeType::DerivedFrom,
            EdgeType::MergeParent,
        ],
        ports: vec![Port {
            name: SNAPSHOT_OUT_PORT_NAME.to_string(),
            direction: PortDirection::Out,
            kind: PortKind::SnapshotRef,
            schema: json!({ "type": "string", "description": "content-addressed merged tree hash" }),
        }],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec!["snapshot.write".to_string()],
        ui_contributions: json!({ "color": "#ec4899", "icon": "git-merge", "displayName": "Merge" }),
        revoked_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_cas::LooseStore;
    use spork_merge::FileResolution;
    use spork_registry::NodeTypeRegistry;
    use tempfile::tempdir;

    /// A store over a temp dir for building trees in tests.
    fn store() -> (tempfile::TempDir, ObjectStore<LooseStore>) {
        let dir = tempdir().unwrap();
        let backend = LooseStore::open(dir.path()).unwrap();
        (dir, ObjectStore::new(backend))
    }

    /// Put a file's bytes and return its content hash (the blob id stand-in: the
    /// merge compares targets by hash, so any stable per-content hash works).
    fn put_file(store: &ObjectStore<LooseStore>, content: &[u8]) -> Hash {
        store.put_blob_bytes(content).unwrap().0
    }

    /// Build and store a flat root tree from (name, content) pairs, returning its
    /// root hash.
    fn build_tree(store: &ObjectStore<LooseStore>, files: &[(&str, &[u8])]) -> Hash {
        let mut map = BTreeMap::new();
        for (name, content) in files {
            map.insert(
                (*name).to_string(),
                FileEntry {
                    kind: EntryKind::File,
                    mode: 0o644,
                    target: put_file(store, content),
                },
            );
        }
        write_tree(store, &map).unwrap()
    }

    #[test]
    fn descriptor_is_mutating_and_owns_snapshot() {
        let d = descriptor();
        assert_eq!(d.id, MERGE_KIND);
        assert_eq!(d.family, Family::Mutating);
        assert!(d.owns_snapshot);
        assert!(d.has_snapshot_out_port());
        assert!(d.allowed_edges.contains(&EdgeType::MergeParent));
        let mut reg = NodeTypeRegistry::new();
        reg.register(d).unwrap();
    }

    #[test]
    fn non_conflicting_merge_is_clean_and_materializable() {
        let (_dir, store) = store();
        // base: a=1, b=1; ours changes a; theirs changes b — disjoint edits.
        let base = build_tree(&store, &[("a.txt", b"1"), ("b.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"2"), ("b.txt", b"1")]);
        let theirs = build_tree(&store, &[("a.txt", b"1"), ("b.txt", b"2")]);

        let outcome = three_way_merge(&store, &base, &ours, &theirs).unwrap();
        assert!(outcome.is_clean());
        let MergeOutcome::Clean {
            merged_tree,
            resolution,
        } = outcome
        else {
            panic!("expected clean");
        };
        assert!(resolution.is_clean());
        // The merged tree is real: re-flatten it and confirm both edits landed.
        let merged = flatten(&store, &merged_tree).unwrap();
        assert_eq!(merged["a.txt"].target, put_file(&store, b"2"));
        assert_eq!(merged["b.txt"].target, put_file(&store, b"2"));
    }

    #[test]
    fn both_sides_same_change_is_clean() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"2")]);
        let theirs = build_tree(&store, &[("a.txt", b"2")]);
        let outcome = three_way_merge(&store, &base, &ours, &theirs).unwrap();
        assert!(outcome.is_clean());
    }

    #[test]
    fn conflicting_merge_returns_a_conflict_set_and_no_node() {
        let (_dir, store) = store();
        // Both change a.txt to different content -> conflict.
        let base = build_tree(&store, &[("a.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"2")]);
        let theirs = build_tree(&store, &[("a.txt", b"3")]);
        let outcome = three_way_merge(&store, &base, &ours, &theirs).unwrap();
        assert!(!outcome.is_clean());
        let conflicts = outcome.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "a.txt");
        assert_eq!(conflicts[0].base, Some(put_file(&store, b"1")));
        assert_eq!(conflicts[0].ours, Some(put_file(&store, b"2")));
        assert_eq!(conflicts[0].theirs, Some(put_file(&store, b"3")));
    }

    #[test]
    fn add_add_conflict_when_content_differs() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[]);
        let ours = build_tree(&store, &[("new.txt", b"ours")]);
        let theirs = build_tree(&store, &[("new.txt", b"theirs")]);
        let outcome = three_way_merge(&store, &base, &ours, &theirs).unwrap();
        assert_eq!(outcome.conflicts().len(), 1);
        assert_eq!(outcome.conflicts()[0].base, None);
    }

    #[test]
    fn delete_vs_modify_is_a_conflict() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1")]);
        let ours = build_tree(&store, &[]); // we deleted a.txt
        let theirs = build_tree(&store, &[("a.txt", b"2")]); // they modified it
        let outcome = three_way_merge(&store, &base, &ours, &theirs).unwrap();
        assert_eq!(outcome.conflicts().len(), 1);
        assert_eq!(outcome.conflicts()[0].ours, None);
    }

    #[test]
    fn clean_delete_when_other_side_unchanged() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1"), ("b.txt", b"1")]);
        let ours = build_tree(&store, &[("b.txt", b"1")]); // deleted a.txt
        let theirs = build_tree(&store, &[("a.txt", b"1"), ("b.txt", b"1")]); // unchanged
        let outcome = three_way_merge(&store, &base, &ours, &theirs).unwrap();
        assert!(outcome.is_clean());
        let MergeOutcome::Clean { merged_tree, .. } = outcome else {
            panic!();
        };
        let merged = flatten(&store, &merged_tree).unwrap();
        assert!(!merged.contains_key("a.txt"));
        assert!(merged.contains_key("b.txt"));
    }

    #[test]
    fn resolution_settles_a_conflict_into_a_clean_tree() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"2")]);
        let theirs = build_tree(&store, &[("a.txt", b"3")]);
        // Resolve the conflict by taking theirs.
        let resolution =
            ConflictResolution::new(vec![FileResolution::new("a.txt", ResolutionChoice::Theirs)]);
        let outcome =
            three_way_merge_with_resolution(&store, &base, &ours, &theirs, &resolution).unwrap();
        assert!(outcome.is_clean());
        let MergeOutcome::Clean {
            merged_tree,
            resolution: res,
        } = outcome
        else {
            panic!();
        };
        assert!(!res.is_clean());
        let merged = flatten(&store, &merged_tree).unwrap();
        assert_eq!(merged["a.txt"].target, put_file(&store, b"3"));
    }

    #[test]
    fn merged_content_ref_resolution_is_applied() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"2")]);
        let theirs = build_tree(&store, &[("a.txt", b"3")]);
        let hand_merged = put_file(&store, b"hand-merged");
        let resolution = ConflictResolution::new(vec![FileResolution::new(
            "a.txt",
            ResolutionChoice::Merged {
                content_ref: hand_merged,
            },
        )]);
        let outcome =
            three_way_merge_with_resolution(&store, &base, &ours, &theirs, &resolution).unwrap();
        let MergeOutcome::Clean { merged_tree, .. } = outcome else {
            panic!();
        };
        let merged = flatten(&store, &merged_tree).unwrap();
        assert_eq!(merged["a.txt"].target, hand_merged);
    }

    #[test]
    fn partial_resolution_returns_remaining_conflicts() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1"), ("b.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"2"), ("b.txt", b"2")]);
        let theirs = build_tree(&store, &[("a.txt", b"3"), ("b.txt", b"3")]);
        // Resolve only a.txt; b.txt stays conflicting.
        let resolution =
            ConflictResolution::new(vec![FileResolution::new("a.txt", ResolutionChoice::Ours)]);
        let outcome =
            three_way_merge_with_resolution(&store, &base, &ours, &theirs, &resolution).unwrap();
        assert!(!outcome.is_clean());
        assert_eq!(outcome.conflicts().len(), 1);
        assert_eq!(outcome.conflicts()[0].path, "b.txt");
    }

    #[test]
    fn resolution_for_non_conflicting_path_is_rejected() {
        let (_dir, store) = store();
        let base = build_tree(&store, &[("a.txt", b"1")]);
        let ours = build_tree(&store, &[("a.txt", b"1")]);
        let theirs = build_tree(&store, &[("a.txt", b"1")]);
        let resolution =
            ConflictResolution::new(vec![FileResolution::new("a.txt", ResolutionChoice::Ours)]);
        let err = three_way_merge_with_resolution(&store, &base, &ours, &theirs, &resolution)
            .unwrap_err();
        assert!(matches!(err, NodesError::Merge(_)));
    }

    #[test]
    fn nested_directory_merge_round_trips() {
        let (_dir, store) = store();
        // Build trees with nested paths by hand through write_tree.
        let mut base = BTreeMap::new();
        base.insert(
            "src/lib.rs".to_string(),
            FileEntry {
                kind: EntryKind::File,
                mode: 0o644,
                target: put_file(&store, b"v1"),
            },
        );
        let base_tree = write_tree(&store, &base).unwrap();

        let mut ours = BTreeMap::new();
        ours.insert(
            "src/lib.rs".to_string(),
            FileEntry {
                kind: EntryKind::File,
                mode: 0o644,
                target: put_file(&store, b"v2"),
            },
        );
        let ours_tree = write_tree(&store, &ours).unwrap();

        // theirs unchanged from base.
        let outcome = three_way_merge(&store, &base_tree, &ours_tree, &base_tree).unwrap();
        assert!(outcome.is_clean());
        let MergeOutcome::Clean { merged_tree, .. } = outcome else {
            panic!()
        };
        let merged = flatten(&store, &merged_tree).unwrap();
        assert_eq!(merged["src/lib.rs"].target, put_file(&store, b"v2"));
    }

    #[test]
    fn payload_is_versioned_and_round_trips() {
        let p = MergePayload::new(
            Hash::from_bytes([1; 32]),
            ConflictResolution::new(Vec::new()),
        );
        assert_eq!(p.schema_version, MERGE_PAYLOAD_VERSION);
        let v = p.to_value().unwrap();
        let back: MergePayload = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }
}
