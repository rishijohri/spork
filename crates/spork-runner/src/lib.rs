//! Spork F4 result-normalization seam — one envelope for every check kind.
//!
//! This crate freezes the seam that makes diffing, baselining, gates, and
//! caching key off a single normalized result rather than the runner type.
//! Every check kind round-trips through one `ResultEnvelope`, so core code never
//! branches on whether a run was a lint, a test suite, or a perf probe. Per the
//! foundation discipline (CLAUDE.md C3) the trait is the seam and F4 ships
//! exactly one real runner behind it; new check kinds are additive.
//!
//! The frozen surface (filled in as the F4 implementation lands):
//!
//! - `ResultEnvelope` — the normalized, schema-versioned result: outcome,
//!   per-unit results, metrics (each keyed by an id from a metric-id registry,
//!   with an optimization direction), violations, an artifact manifest, a run
//!   id, and the cache `input_digest` (CLAUDE.md C5). Test-shaped, perf-shaped,
//!   and lint-shaped results are all representable in this one struct.
//! - `CheckSpec` — the versioned, kind-tagged check request, including the
//!   `impure` flag that disables caching.
//! - `Runner` — the check SPI (`describe` / `prepare` / `run` / `normalize` /
//!   `collect_artifacts`). F4 implements the `SanityRunner`: a deterministic
//!   pattern / lint check that reads the materialized worktree via `spork-exec`
//!   and emits violations and an outcome, real and complete for one kind.
//! - The input-digest cache: `input_digest =
//!   blake3(canon(parent_content_ref + canonical(config) + runner_version))`,
//!   with a per-artifact derivation-key generation so a formula change opens a
//!   new generation instead of poisoning the old. An identical
//!   `(spec, input_tree, runner_version)` re-run is a cache hit; an impure spec
//!   never caches.
//!
//! This realizes the runner / result model in DESIGN.md §8.1 ("Checks &
//! runners") and §8.2 ("The result envelope"), and the content-addressed
//! derivation cache in §4.4.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cache;
mod caching;
mod digest;
mod envelope;
mod error;
mod metric;
mod runner;
mod sanity;
mod spec;

pub use cache::{CasResultCache, InMemoryResultCache, ResultCache};
pub use caching::{CacheStatus, CachingRunner, CheckRun};
pub use digest::{
    input_digest, ChangeScope, DerivationKey, DERIVATION_GENERATION_V1, DERIVATION_KEY_VERSION,
};
pub use envelope::{
    ArtifactManifest, ArtifactRef, Outcome, ResultEnvelope, UnitResult, UnitStatus, Violation,
    ARTIFACT_MANIFEST_VERSION, RESULT_ENVELOPE_VERSION,
};
pub use error::{Result, RunnerError};
pub use metric::{
    Direction, Metric, MetricDescriptor, MetricId, MetricRegistry, METRIC_FILES_SCANNED,
    METRIC_VIOLATION_COUNT,
};
pub use runner::{Runner, RunnerCapabilities, SandboxContext};
pub use sanity::{SanityRunner, SANITY_GENERATION, SANITY_KIND, SANITY_VERSION};
pub use spec::{CheckSpec, CHECK_SPEC_VERSION};

// Re-export the run/cancel value types from spork-exec that the Runner SPI
// traffics in, so a downstream crate can use the SPI without separately
// depending on spork-exec for these types.
pub use spork_exec::{CancelToken, PreparedRun, RawRunOutput, SandboxTier, Workspace};
