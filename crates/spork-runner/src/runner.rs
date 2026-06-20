//! The check SPI: [`Runner`], [`RunnerCapabilities`], and [`SandboxContext`].
//!
//! Every check kind — tests, perf, lint/sanity — is a [`Runner`] adapter, not a
//! bespoke pipeline (DESIGN.md §8.1). Behind this one interface, storage,
//! diffing, baselining, gating, and caching are implemented once: a runner only
//! has to (1) describe its capabilities, (2) `prepare` a
//! [`PreparedRun`](spork_exec::PreparedRun) against a materialized worktree, (3)
//! `run` it through the isolation backend, (4) `normalize` the raw output into
//! the single [`ResultEnvelope`](crate::ResultEnvelope), and (5) point at the
//! bulky artifacts it produced. A new check kind is a new `Runner` plus a result
//! schema and instantly inherits the whole pipeline.
//!
//! The five methods mirror the SPI in DESIGN.md §8.1 verbatim:
//!
//! ```text
//! interface Runner {
//!   describe(): RunnerCapabilities
//!   prepare(ctx: SandboxContext, spec: CheckSpec): PreparedRun
//!   run(prepared, signal): RawRunOutput
//!   normalize(raw, spec): ResultEnvelope   // the load-bearing contract
//!   collectArtifacts(raw): ArtifactManifest
//! }
//! ```
//!
//! Per the foundation discipline (CLAUDE.md C3) the trait is the seam and F4
//! ships exactly one real runner behind it ([`SanityRunner`](crate::SanityRunner)).
//!
//! Design references: DESIGN.md §8.1 (one Runner SPI; the load-bearing
//! `ResultEnvelope`), §8.2 (execution against a materialized tree), §4.4
//! (`inputDigest` / `derivationKey`).

use spork_exec::{CancelToken, PreparedRun, RawRunOutput, Workspace};
use spork_hash::Hash;

use crate::envelope::{ArtifactManifest, ResultEnvelope};
use crate::error::Result;
use crate::spec::CheckSpec;

/// What a runner is and what it can handle.
///
/// Returned by [`Runner::describe`] so the orchestrator can route a
/// [`CheckSpec`] to a runner that supports its `kind`, and so the cache key can
/// fold in the runner's [`version`](RunnerCapabilities::version) and formula
/// [`generation`](RunnerCapabilities::generation) (DESIGN.md §8.1, §4.4).
///
/// [`version`](RunnerCapabilities::version) is the analogue of the
/// `runnerImageDigest` in the `(checkSpecId@version, inputTreeHash,
/// runnerImageDigest)` cache tuple: change the formula, change the version, and
/// old and new results never collide. [`generation`](RunnerCapabilities::generation)
/// is the coarse derivation-key generation the cache uses to keep generations
/// isolated (see [`DerivationKey`](crate::DerivationKey)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerCapabilities {
    /// A stable runner name (e.g. `"sanity"`).
    pub name: String,
    /// The runner version string folded into the cache key (e.g. `"sanity@1"`).
    pub version: String,
    /// The derivation-formula generation the cache keys off (bump on a formula
    /// change to open a fresh, non-poisoning generation).
    pub generation: u32,
    /// The check kinds this runner handles.
    pub kinds: Vec<String>,
    /// Whether this runner is hermetic by contract (no network, read-only base,
    /// pinned tools). Sanity checks are hermetic (DESIGN.md §8.2).
    pub hermetic: bool,
}

impl RunnerCapabilities {
    /// Whether this runner handles the given check kind.
    #[must_use]
    pub fn handles(&self, kind: &str) -> bool {
        self.kinds.iter().any(|k| k == kind)
    }
}

/// The materialized context a runner prepares and executes against.
///
/// A check runs inside an isolated sandbox **materialized from the parent tree**,
/// never the user's live working directory (DESIGN.md §8.2). This bundles the
/// provisioned [`Workspace`](spork_exec::Workspace) (the CoW copy the runner
/// reads and the backend executes in) with the content hash of the input tree
/// the check actually reads ([`input_tree`](SandboxContext::input_tree)) — the
/// honestly-declared input that feeds the cache key. The optional
/// [`changed_paths`](SandboxContext::changed_paths) carries the change scope so a
/// runner can scope its work to what an edit touched (DESIGN.md §8.2,
/// change-scoped auto-run).
#[derive(Debug, Clone)]
pub struct SandboxContext {
    /// The provisioned CoW workspace the check reads (and the backend runs in).
    pub workspace: Workspace,
    /// The content hash of the input tree the check reads — its declared input,
    /// which feeds [`input_digest`](crate::input_digest).
    pub input_tree: Hash,
    /// The paths changed by the edit that triggered this check, relative to the
    /// worktree root. Empty means "scan everything" (a full run rather than a
    /// change-scoped one).
    pub changed_paths: Vec<String>,
}

impl SandboxContext {
    /// Construct a full-scope context (scan everything) for a workspace whose
    /// input tree hash is `input_tree`.
    #[must_use]
    pub fn new(workspace: Workspace, input_tree: Hash) -> Self {
        SandboxContext {
            workspace,
            input_tree,
            changed_paths: Vec::new(),
        }
    }

    /// Restrict the check to a set of changed paths (builder style).
    #[must_use]
    pub fn with_changed_paths(mut self, paths: impl IntoIterator<Item = String>) -> Self {
        self.changed_paths = paths.into_iter().collect();
        self
    }

    /// Whether this context is change-scoped (has a non-empty change set).
    #[must_use]
    pub fn is_change_scoped(&self) -> bool {
        !self.changed_paths.is_empty()
    }
}

/// The uniform execution interface for every check kind (DESIGN.md §8.1).
///
/// See the [module docs](crate::runner) for the rationale. Implementations are
/// `Sync` so the orchestrator can hold a runner behind `&self`.
pub trait Runner {
    /// Describe this runner's identity, version, and supported kinds.
    fn describe(&self) -> RunnerCapabilities;

    /// Prepare a [`PreparedRun`](spork_exec::PreparedRun) for `spec` against the
    /// materialized `ctx`.
    ///
    /// For a runner that shells out to a tool this builds the command line; for
    /// an in-process runner (like the sanity runner, which reads the worktree
    /// directly) the prepared run is a marker the backend need not actually
    /// spawn — the work happens in [`normalize`](Runner::normalize) over the
    /// materialized tree. Either way the contract is the same: the runner never
    /// reads outside the workspace.
    ///
    /// # Errors
    /// [`RunnerError::UnsupportedKind`](crate::RunnerError::UnsupportedKind) if
    /// this runner does not handle `spec.kind`;
    /// [`RunnerError::Config`](crate::RunnerError::Config) if the config is
    /// malformed.
    fn prepare(&self, ctx: &SandboxContext, spec: &CheckSpec) -> Result<PreparedRun>;

    /// Run a prepared check, honoring the cancellation signal, and return the
    /// raw (uninterpreted) output.
    ///
    /// # Errors
    /// [`RunnerError::Exec`](crate::RunnerError::Exec) if execution fails, or a
    /// cancellation surfaced from the backend.
    fn run(&self, prepared: PreparedRun, signal: &CancelToken) -> Result<RawRunOutput>;

    /// Normalize raw output into the single [`ResultEnvelope`](crate::ResultEnvelope).
    ///
    /// **This is the load-bearing contract** (DESIGN.md §8.1): whatever the
    /// check kind, the result is expressed in the one envelope shape so the core
    /// never branches on runner type. The runner fills in the
    /// [`input_digest`](ResultEnvelope::input_digest) for the result; the caller
    /// (or the [`CachingRunner`](crate::CachingRunner)) is responsible for cache
    /// lookup/store keyed off it.
    ///
    /// # Errors
    /// [`RunnerError`](crate::RunnerError) if the raw output cannot be
    /// normalized (e.g. an unparseable tool report, or an unregistered metric
    /// id).
    fn normalize(&self, raw: RawRunOutput, spec: &CheckSpec) -> Result<ResultEnvelope>;

    /// Collect the content-addressed manifest of bulky artifacts this run
    /// produced (logs, coverage, traces).
    ///
    /// The small queryable result lives in the envelope; bulky outputs are
    /// referenced here and stored separately for lazy load and cross-branch
    /// dedup (DESIGN.md §8.2). A runner with nothing bulky returns an empty
    /// manifest.
    fn collect_artifacts(&self, raw: &RawRunOutput) -> ArtifactManifest;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_handles_declared_kinds() {
        let caps = RunnerCapabilities {
            name: "sanity".into(),
            version: "sanity@1".into(),
            generation: 1,
            kinds: vec!["sanity".into(), "pattern".into()],
            hermetic: true,
        };
        assert!(caps.handles("sanity"));
        assert!(caps.handles("pattern"));
        assert!(!caps.handles("test"));
    }

    // The change-scope behavior of `SandboxContext` is exercised end-to-end
    // against a real provisioned `Workspace` in the integration tests
    // (`tests/sanity_runner.rs`), where a backend produces a genuine workspace —
    // `Workspace`/`Lease` are deliberately not hand-constructed here so this unit
    // test stays decoupled from those crates' internal field layout.
}
