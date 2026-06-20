//! The command-execution value types: [`PreparedRun`], [`RawRunOutput`], and
//! [`CancelToken`].
//!
//! These are the data that flow through an [`IsolationBackend`](crate::IsolationBackend):
//! a [`PreparedRun`] describes *what* to run (program, args, env overlay,
//! optional working subdirectory), the backend runs it inside the materialized
//! CoW workspace, and a [`RawRunOutput`] carries the *uninterpreted* result
//! (exit code + captured stdout/stderr). The output is deliberately raw: it is a
//! later layer's job (the `Runner` SPI in `spork-runner`, P-phase) to *normalize*
//! it into a structured result. Keeping the substrate's output untyped is what
//! lets every check kind round-trip through one normalization seam without the
//! substrate knowing about check kinds (DESIGN.md §8.1, §8.2).
//!
//! A [`CancelToken`] is a cheap, clonable, thread-shareable cooperative-cancel
//! flag — the long-running stress/test tiers (P8) and the cancel path of the IPC
//! layer (F3) signal it, and a backend polls it so a cancelled run stops
//! promptly rather than running to completion (DESIGN.md §11.3, lease/heartbeat
//! lifecycle).
//!
//! Design references: DESIGN.md §5.3 (execution & isolation), §8.1-§8.2 (one
//! Runner SPI; raw output is normalized downstream), §11.1-§11.3 (the
//! `IsolationBackend` interface and lifecycle).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The current schema version stamped on a freshly constructed [`PreparedRun`].
///
/// A prepared run can be persisted (e.g. for replay or audit), so it carries a
/// schema version (constraint C5).
pub const PREPARED_RUN_VERSION: u16 = 1;

/// The current schema version stamped on a freshly constructed
/// [`RawRunOutput`].
pub const RAW_RUN_OUTPUT_VERSION: u16 = 1;

/// A fully-resolved command ready to execute inside a workspace.
///
/// This is the substrate's *input*: a program plus its arguments, an
/// environment overlay applied on top of the workspace's base environment, and
/// an optional working subdirectory **relative to the workspace root**. A
/// relative `cwd` keeps all execution confined to the CoW copy, never the user
/// checkout (DESIGN.md §5.3, §11.1).
///
/// `PreparedRun` is a value (it derives serde and `Clone`) so it can be built by
/// a higher layer, handed to a backend, and recorded for replay. Secrets are
/// **never** placed here — credentials are injected at runtime by the secrets
/// broker and scrubbed on teardown, never written into a persisted record
/// (DESIGN.md §11.3, §15.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedRun {
    /// The schema version of this record ([`PREPARED_RUN_VERSION`] for fresh
    /// values).
    pub schema_version: u16,
    /// The program to execute (looked up via the workspace's `PATH`, or an
    /// absolute path).
    pub program: String,
    /// The arguments passed to the program, in order.
    pub args: Vec<String>,
    /// Environment variables to overlay on top of the inherited environment,
    /// keyed by name. A [`BTreeMap`] so the overlay is deterministic and a
    /// `PreparedRun` hashes/serializes stably.
    pub env: BTreeMap<String, String>,
    /// An optional working directory **relative to the workspace root**. `None`
    /// runs at the workspace root. An absolute or `..`-escaping value is refused
    /// at execution time ([`ExecError::PathEscape`](crate::ExecError::PathEscape)).
    pub cwd: Option<PathBuf>,
}

impl PreparedRun {
    /// Construct a prepared run for `program` with `args`, no env overlay, at the
    /// workspace root.
    ///
    /// Stamps the current [`PREPARED_RUN_VERSION`].
    #[must_use]
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        PreparedRun {
            schema_version: PREPARED_RUN_VERSION,
            program: program.into(),
            args: args.into_iter().collect(),
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    /// Set an environment-variable overlay entry (builder style).
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set the working subdirectory, relative to the workspace root (builder
    /// style).
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

/// The uninterpreted result of a [`PreparedRun`].
///
/// Carries the process exit code and the captured stdout/stderr as raw bytes.
/// It is deliberately *not* normalized — turning this into a structured
/// [result envelope](https://docs.rs) is the job of the `Runner` SPI layer, so
/// the substrate stays agnostic to check kinds (DESIGN.md §8.1, §8.2). A
/// command that exited non-zero is a *successful run with a non-zero exit code*,
/// not an [`ExecError`](crate::ExecError); only the inability to run at all is an
/// error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawRunOutput {
    /// The schema version of this record ([`RAW_RUN_OUTPUT_VERSION`] for fresh
    /// values).
    pub schema_version: u16,
    /// The process exit code, or `None` if the process was terminated by a
    /// signal (Unix) without a normal exit.
    pub exit_code: Option<i32>,
    /// The captured standard output, as raw bytes.
    pub stdout: Vec<u8>,
    /// The captured standard error, as raw bytes.
    pub stderr: Vec<u8>,
}

impl RawRunOutput {
    /// Construct a raw output from an exit code and captured streams.
    ///
    /// Stamps the current [`RAW_RUN_OUTPUT_VERSION`].
    #[must_use]
    pub fn new(exit_code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        RawRunOutput {
            schema_version: RAW_RUN_OUTPUT_VERSION,
            exit_code,
            stdout,
            stderr,
        }
    }

    /// Whether the run exited successfully (exit code `0`).
    #[must_use]
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// A cheap, clonable cooperative-cancellation flag.
///
/// A `CancelToken` is shared (it is `Arc`-backed) between the party that may
/// request cancellation and the backend that polls it. Cloning a token shares
/// the *same* underlying flag, so cancelling any clone cancels them all. A
/// backend checks [`is_cancelled`](CancelToken::is_cancelled) at safe points and
/// returns [`ExecError::Cancelled`](crate::ExecError::Cancelled) if set.
///
/// This is the cooperative-cancel primitive the lease/heartbeat lifecycle and
/// the IPC cancel path drive (DESIGN.md §11.3); it is intentionally minimal so
/// the harder pre-emptive teardown (the reaper force-killing a workspace) can be
/// layered on top without changing this surface.
///
/// # Example
/// ```
/// use spork_exec::CancelToken;
///
/// let token = CancelToken::new();
/// let clone = token.clone();
/// assert!(!clone.is_cancelled());
/// token.cancel();
/// assert!(clone.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// Create a fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        CancelToken {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. Idempotent.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested on this token (or any clone).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_run_builder_sets_fields() {
        let run = PreparedRun::new("echo", ["hi".to_string()])
            .with_env("FOO", "bar")
            .with_cwd("sub/dir");
        assert_eq!(run.schema_version, PREPARED_RUN_VERSION);
        assert_eq!(run.program, "echo");
        assert_eq!(run.args, vec!["hi".to_string()]);
        assert_eq!(run.env.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(run.cwd, Some(PathBuf::from("sub/dir")));
    }

    #[test]
    fn prepared_run_round_trips_through_serde() {
        let run = PreparedRun::new("cargo", ["test".to_string(), "--lib".to_string()])
            .with_env("RUST_LOG", "debug");
        let json = serde_json::to_string(&run).unwrap();
        let back: PreparedRun = serde_json::from_str(&json).unwrap();
        assert_eq!(run, back);
    }

    #[test]
    fn raw_output_success_predicate() {
        assert!(RawRunOutput::new(Some(0), vec![], vec![]).success());
        assert!(!RawRunOutput::new(Some(1), vec![], vec![]).success());
        assert!(!RawRunOutput::new(None, vec![], vec![]).success());
    }

    #[test]
    fn raw_output_round_trips_through_serde() {
        let out = RawRunOutput::new(Some(0), b"out".to_vec(), b"err".to_vec());
        let json = serde_json::to_string(&out).unwrap();
        let back: RawRunOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }

    #[test]
    fn cancel_token_clones_share_one_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        assert!(!b.is_cancelled());
        b.cancel();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
        // Idempotent.
        b.cancel();
        assert!(a.is_cancelled());
    }

    #[test]
    fn cancel_token_default_is_uncancelled() {
        assert!(!CancelToken::default().is_cancelled());
    }
}
