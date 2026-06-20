//! [`GitContext`]: a read-only snapshot of a repository's HEAD state, plus the
//! [`import_git_state`] function that captures it without touching `.git`.
//!
//! `GitContext` is the metadata Spork records *about* a node's relationship to
//! the user's real Git history (DESIGN.md §10.4: "`GitContext` records the
//! user's HEAD/index/dirty-state as metadata on each node without modifying
//! `.git`"). Capturing it is strictly read-only: it opens the repository, reads
//! `HEAD`, the current branch shorthand, the dirty flag, and the parent commit
//! id — and writes nothing back.
//!
//! Design reference: DESIGN.md §10.4.

use std::path::Path;

use git2::{Repository, StatusOptions};
use serde::{Deserialize, Serialize};

use crate::error::{GitError, Result};

/// The schema version stamped on a freshly captured [`GitContext`] (CLAUDE.md
/// C5: every persisted struct carries an explicit, hashed/serialized version so
/// the captured shape can evolve additively).
pub const GIT_CONTEXT_VERSION: u16 = 1;

/// A read-only snapshot of a Git repository's HEAD-relative state.
///
/// This is the metadata Spork attaches to a node so the DAG stays aligned with
/// the user's manual Git operations (DESIGN.md §10.4). It is *captured*, never
/// *applied*: nothing in this crate writes a `GitContext` back into `.git`.
///
/// The fields describe the repository at capture time:
/// - `head` — the raw `HEAD` symbolic/target (the full ref name like
///   `refs/heads/main`, or a 40-char detached-HEAD commit sha), or `None` for an
///   empty repository with an unborn branch.
/// - `branch` — the human-friendly branch shorthand (`main`), or `None` when
///   `HEAD` is detached or unborn.
/// - `dirty` — whether the working tree or index has uncommitted changes
///   (untracked files included), so a node can record that it was created over a
///   dirty tree.
/// - `git_parent_commit` — the commit `HEAD` resolves to (the would-be parent of
///   a new commit), or `None` for an unborn branch. This is the value Spork
///   *imports* into a [`spork_cas::Snapshot::git_parent_commit`] — a lone,
///   foreign SHA-1 it never computes itself.
///
/// `schema_version` is `GIT_CONTEXT_VERSION` for a freshly captured context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitContext {
    /// Schema version (CLAUDE.md C5). New contexts use [`GIT_CONTEXT_VERSION`].
    pub schema_version: u16,
    /// The raw `HEAD` reference (full ref name, or detached commit sha), or
    /// `None` for an unborn branch (a fresh repo with no commits).
    pub head: Option<String>,
    /// The current branch shorthand (`main`), or `None` if `HEAD` is detached or
    /// unborn.
    pub branch: Option<String>,
    /// Whether the working tree / index has uncommitted changes at capture time.
    pub dirty: bool,
    /// The commit `HEAD` points at (the parent a new commit would take), or
    /// `None` for an unborn branch.
    pub git_parent_commit: Option<String>,
}

impl GitContext {
    /// The context for a path that is not a Git repository at all.
    ///
    /// Callers that want a non-failing probe (record "no git here" as metadata
    /// rather than an error) can compare against this; `import_git_state` itself
    /// returns [`GitError::NotARepo`] so the distinction is explicit.
    #[must_use]
    pub fn none() -> Self {
        GitContext {
            schema_version: GIT_CONTEXT_VERSION,
            head: None,
            branch: None,
            dirty: false,
            git_parent_commit: None,
        }
    }
}

/// Read a repository's HEAD-relative state without modifying `.git`, the index,
/// or the working tree.
///
/// This opens the repository discovered at (or above) `repo_path` and reads:
/// `HEAD` (full ref name or detached sha), the branch shorthand, whether the
/// tree is dirty, and the parent commit id. It performs **no** writes — no ref
/// creation, no index update, no checkout — so it satisfies the non-invasive
/// import contract (DESIGN.md §10.4) by construction.
///
/// An empty repository (unborn branch, no commits) is a normal, successful case:
/// `head`/`branch`/`git_parent_commit` are `None` and `dirty` reflects whether
/// any files are present.
///
/// # Errors
/// - [`GitError::NotARepo`] if `repo_path` is not inside a Git repository.
/// - [`GitError::Git`] if a libgit2 read fails for any other reason.
pub fn import_git_state(repo_path: impl AsRef<Path>) -> Result<GitContext> {
    let repo = open_repo(repo_path.as_ref())?;
    import_from_repo(&repo)
}

/// Open the repository discovered at (or above) `path`.
///
/// Uses `Repository::discover` so a path *inside* a working tree (a subdirectory)
/// still resolves to the enclosing repository, matching ordinary git behavior.
pub(crate) fn open_repo(path: &Path) -> Result<Repository> {
    Repository::discover(path).map_err(GitError::from_open)
}

/// Capture a [`GitContext`] from an already-open [`Repository`] (read-only).
fn import_from_repo(repo: &Repository) -> Result<GitContext> {
    // Dirty detection: include untracked files but never ignored ones, and never
    // recurse into ignored directories. This is a pure status query — it does not
    // refresh or write the index.
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .include_ignored(false)
        .recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(GitError::from_git)?;
    let dirty = !statuses.is_empty();

    // HEAD may be unborn (a fresh repo with no commits); that is not an error.
    match repo.head() {
        Ok(head_ref) => {
            let head = head_ref.name().ok().map(str::to_string);
            let branch = if head_ref.is_branch() {
                head_ref.shorthand().ok().map(str::to_string)
            } else {
                // Detached HEAD: there is no branch shorthand worth recording.
                None
            };
            // The commit HEAD resolves to — the parent a new commit would take.
            let git_parent_commit = head_ref.peel_to_commit().ok().map(|c| c.id().to_string());
            Ok(GitContext {
                schema_version: GIT_CONTEXT_VERSION,
                head,
                branch,
                dirty,
                git_parent_commit,
            })
        }
        Err(e) if e.code() == git2::ErrorCode::UnbornBranch => {
            // Empty repo: no commits yet. Record the unborn branch name if libgit2
            // exposes it via the symbolic HEAD reference, but no parent commit.
            let branch = unborn_branch_name(repo);
            Ok(GitContext {
                schema_version: GIT_CONTEXT_VERSION,
                head: None,
                branch,
                dirty,
                git_parent_commit: None,
            })
        }
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            // No HEAD at all (extremely fresh / partially-initialized repo).
            Ok(GitContext {
                schema_version: GIT_CONTEXT_VERSION,
                head: None,
                branch: None,
                dirty,
                git_parent_commit: None,
            })
        }
        Err(e) => Err(GitError::from_git(e)),
    }
}

/// Best-effort read of the branch an unborn `HEAD` symbolically points at
/// (e.g. `main` for a fresh `HEAD -> refs/heads/main`). Returns `None` if it
/// cannot be determined; this never writes anything.
fn unborn_branch_name(repo: &Repository) -> Option<String> {
    // `find_reference("HEAD")` on an unborn branch yields a *symbolic* reference
    // whose target name is the not-yet-created branch ref.
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target().ok().flatten()?;
    target.strip_prefix("refs/heads/").map(str::to_string)
}
