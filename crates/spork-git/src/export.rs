//! [`export_to_git`]: project a Spork node tree into a real Git commit on a new
//! branch — without moving HEAD, touching the index, or altering the working
//! tree.
//!
//! This is the write half of the non-invasive bridge (DESIGN.md §10.4:
//! "`exportToGit(nodeId)` projects any node into a real commit/branch on demand
//! for team handoff"). It reads a [`spork_cas`] tree (the content-addressed
//! directory manifest a Spork node points at), translates each blob/tree/symlink
//! into the equivalent Git object via libgit2 plumbing, writes one commit over
//! the resulting Git tree, and points a brand-new branch ref at that commit.
//!
//! # Why it is non-invasive
//!
//! Every operation here is a pure *object-database write plus one new ref*:
//! - Git blobs and trees are content-addressed; writing them adds objects to
//!   `.git/objects` but changes no existing object and no ref.
//! - The commit is created with `update_ref = None`, so libgit2 does **not**
//!   move `HEAD` or any branch.
//! - The branch is created with `Repository::reference` on a fresh
//!   `refs/heads/<name>` (non-force), so it can only *add* a ref, never clobber
//!   an existing one.
//! - The index and working tree are never read for writing and never written:
//!   no `checkout`, no `add`, no index update.
//!
//! The net effect on a repository is exactly "a new branch ref appeared, and the
//! object store gained some objects"; `HEAD`, the index, and the working tree are
//! byte-for-byte unchanged. The crate's tests assert this with a before/after
//! byte capture over a temp repo.
//!
//! Design reference: DESIGN.md §10.4.

use git2::{FileMode, Oid, Repository, Signature};
use spork_cas::{EntryKind, ObjectStore, StorageBackend};
use spork_hash::Hash;

use crate::context::open_repo;
use crate::error::{GitError, Result};

/// The author/committer identity stamped on an exported commit.
///
/// Spork-exported commits are deterministic, machine-authored handoff artifacts,
/// so they carry a fixed, recognizable identity rather than the user's git
/// config (which would be a read of, and a coupling to, mutable environment
/// state). A caller that needs a different identity can be added additively
/// later (CLAUDE.md C3); the v1 contract is this fixed signature.
const EXPORT_AUTHOR_NAME: &str = "Spork Export";
/// The email stamped on an exported commit (see [`EXPORT_AUTHOR_NAME`]).
const EXPORT_AUTHOR_EMAIL: &str = "export@spork.local";

/// Project the Spork tree `node_tree` into a Git commit on a new branch named
/// `branch_name`, leaving the user's `HEAD`, index, and working tree unchanged.
///
/// `store` is the content-addressed object store holding the node's tree
/// ([`spork_cas`]); `node_tree` is the id of that tree's root [`spork_cas::Tree`]
/// (the directory manifest a Spork node points at). The function:
///
/// 1. Recursively reads the Spork tree from `store` and writes the equivalent
///    Git blobs/trees into `repo_path`'s object database (executable bits and
///    symlinks preserved).
/// 2. Creates one commit over the resulting Git root tree with **no parent** and
///    `update_ref = None` (so no ref is moved).
/// 3. Creates a new `refs/heads/<branch_name>` pointing at that commit
///    (non-force: it fails if the branch already exists rather than clobbering
///    it).
///
/// Returns the new commit's SHA-1 as a 40-character lowercase hex string.
///
/// The repository's `HEAD`, index, and working tree are not modified. This is
/// the non-invasive export contract of DESIGN.md §10.4.
///
/// # Errors
/// - [`GitError::NotARepo`] if `repo_path` is not a Git repository.
/// - [`GitError::Io`] if reading the Spork tree from `store` fails.
/// - [`GitError::Git`] if a libgit2 write fails, including the branch already
///   existing (a non-force ref create over an existing ref).
pub fn export_to_git<S: StorageBackend + Sync>(
    repo_path: impl AsRef<std::path::Path>,
    store: &ObjectStore<S>,
    node_tree: Hash,
    branch_name: &str,
) -> Result<String> {
    let repo = open_repo(repo_path.as_ref())?;

    // 1. Translate the Spork content tree into Git objects.
    let git_tree = write_spork_tree(&repo, store, &node_tree)?;

    // 2. One parentless commit over that tree, with NO ref update (update_ref =
    //    None) so HEAD/branches are untouched.
    let tree = repo.find_tree(git_tree).map_err(GitError::from_git)?;
    // A fixed, deterministic-shaped signature. `Signature::now` reads the wall
    // clock for the timestamp; that affects only the commit's own identity (a new
    // object), never the repository's existing HEAD/index/working tree, which is
    // what the non-invasive contract protects.
    let sig: Signature<'_> =
        Signature::now(EXPORT_AUTHOR_NAME, EXPORT_AUTHOR_EMAIL).map_err(GitError::from_git)?;
    let message = format!("spork export: {branch_name}\n");
    let commit_oid = repo
        .commit(None, &sig, &sig, &message, &tree, &[])
        .map_err(GitError::from_git)?;

    // 3. Add a new branch ref pointing at the commit. `force = false` means this
    //    can only ADD a ref, never overwrite the user's existing branch.
    let ref_name = format!("refs/heads/{branch_name}");
    repo.reference(&ref_name, commit_oid, false, &message)
        .map_err(GitError::from_git)?;

    Ok(commit_oid.to_string())
}

/// Recursively translate the Spork [`spork_cas::Tree`] `tree_id` into Git objects
/// in `repo`, returning the resulting Git tree [`Oid`].
///
/// Each [`spork_cas::TreeEntry`] becomes a Git tree-builder entry:
/// - a file → a Git blob (`FileMode::Blob` / `BlobExecutable` per the stored
///   mode's owner-execute bit);
/// - a directory → a recursively built Git subtree (`FileMode::Tree`);
/// - a symlink → a Git symlink blob whose content is the link target text
///   (`FileMode::Link`), matching how Git itself stores symlinks.
fn write_spork_tree<S: StorageBackend + Sync>(
    repo: &Repository,
    store: &ObjectStore<S>,
    tree_id: &Hash,
) -> Result<Oid> {
    let spork_tree = store.read_tree(tree_id)?;
    let mut builder = repo.treebuilder(None).map_err(GitError::from_git)?;

    for entry in &spork_tree.entries {
        let (oid, mode) = match entry.kind {
            EntryKind::File => {
                let bytes = store.read_blob(&entry.target)?;
                let oid = repo.blob(&bytes).map_err(GitError::from_git)?;
                let mode = if entry.mode & 0o100 != 0 {
                    FileMode::BlobExecutable
                } else {
                    FileMode::Blob
                };
                (oid, mode)
            }
            EntryKind::Dir => {
                let oid = write_spork_tree(repo, store, &entry.target)?;
                (oid, FileMode::Tree)
            }
            EntryKind::Symlink => {
                // A Spork symlink stores its target path text as a blob (see
                // spork-cas object model). Git stores a symlink the same way: a
                // blob holding the target, with the link file mode.
                let target_bytes = store.read_blob(&entry.target)?;
                let oid = repo.blob(&target_bytes).map_err(GitError::from_git)?;
                (oid, FileMode::Link)
            }
        };
        builder
            .insert(&entry.name, oid, i32::from(mode))
            .map_err(GitError::from_git)?;
    }

    builder.write().map_err(GitError::from_git)
}
