//! The error type for the non-invasive Git bridge.
//!
//! Every fallible entry point in this crate returns [`GitError`]. The variants
//! deliberately stay coarse — the contract a caller cares about is "is this a
//! real repository?", "did a git plumbing call fail?", and "did the surrounding
//! filesystem/content-store I/O fail?" — and the rich underlying detail (a
//! [`git2::Error`] message, a [`std::io::Error`] message, or a
//! [`spork_cas::CasError`] message) is preserved as a string so nothing is
//! silently swallowed.
//!
//! Design reference: DESIGN.md §10.4 ("Git coexistence is bidirectional but
//! non-invasive").

use thiserror::Error;

/// An error from importing or exporting Git state.
///
/// `#[non_exhaustive]` so future, additive failure modes (e.g. a dedicated
/// signing error) can be introduced without breaking downstream `match`es — the
/// no-domino discipline (CLAUDE.md C3).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GitError {
    /// `repo_path` is not (inside) a Git repository, so there is nothing to
    /// read from or write a branch into.
    #[error("not a git repository")]
    NotARepo,

    /// A `git2`/libgit2 plumbing call failed. The wrapped string is the
    /// underlying libgit2 message, preserved verbatim.
    #[error("git error: {0}")]
    Git(String),

    /// A surrounding I/O or content-store operation failed (reading the CAS
    /// tree, capturing the working tree, etc.). The wrapped string is the
    /// underlying error message.
    #[error("io error: {0}")]
    Io(String),
}

impl GitError {
    /// Map a [`git2::Error`] into a [`GitError`].
    ///
    /// A "not found" / "bare repo" / "unborn branch" class of failure when
    /// *opening* a repository is surfaced as [`GitError::NotARepo`]; every other
    /// libgit2 failure is preserved as [`GitError::Git`] with its message. The
    /// repository-open path uses [`GitError::from_open`] for the cleaner
    /// not-a-repo mapping; this conversion is the general one used once a repo is
    /// already open.
    pub(crate) fn from_git(e: git2::Error) -> Self {
        GitError::Git(e.message().to_string())
    }

    /// Map a [`git2::Error`] returned by [`git2::Repository::open`] /
    /// `discover` into a [`GitError`], collapsing the "no repository here"
    /// cases to [`GitError::NotARepo`].
    pub(crate) fn from_open(e: git2::Error) -> Self {
        use git2::ErrorCode;
        match e.code() {
            ErrorCode::NotFound => GitError::NotARepo,
            _ => GitError::Git(e.message().to_string()),
        }
    }
}

impl From<spork_cas::CasError> for GitError {
    fn from(e: spork_cas::CasError) -> Self {
        GitError::Io(e.to_string())
    }
}

impl From<std::io::Error> for GitError {
    fn from(e: std::io::Error) -> Self {
        GitError::Io(e.to_string())
    }
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, GitError>;
