//! The cache-aware orchestrator: [`CachingRunner`].
//!
//! A bare [`Runner`](crate::Runner) just prepares, runs, and normalizes; it
//! knows nothing about caching. The caching policy — *which* runs are cacheable,
//! *how* the key is derived, and the append-only history discipline — lives here,
//! once, so every runner inherits it without re-implementing it (DESIGN.md §8.1,
//! §8.2, §9.2). [`CachingRunner`] wraps a runner and a
//! [`ResultCache`](crate::ResultCache) and exposes one method,
//! [`check`](CachingRunner::check), that runs the whole pipeline against a
//! materialized [`SandboxContext`](crate::SandboxContext) with the cache in the
//! loop:
//!
//! 1. Build the [`DerivationKey`](crate::DerivationKey) from the input tree the
//!    check reads, the canonical config, the runner version, and the runner's
//!    formula generation.
//! 2. **Impure short-circuit.** If the [`CheckSpec`](crate::CheckSpec) is
//!    `impure`, skip the cache entirely — never look up, never store — and always
//!    re-run (DESIGN.md §9.2).
//! 3. **Cache hit.** Otherwise look the key up; a hit returns the *stored*
//!    envelope (the original run, with its original `run_id`) and **does not
//!    re-execute** the runner — the append-only history is preserved (DESIGN.md
//!    §7, §8.2).
//! 4. **Cache miss.** Run `prepare` → `run` → `normalize` to produce a fresh
//!    envelope (a new `run_id`), store it under the key, and return it.
//!
//! The outcome of a [`check`](CachingRunner::check) is reported as a
//! [`CheckRun`] that records whether it was a hit or a miss, so a caller (and a
//! test) can prove "an identical re-run was a cache hit, not a re-execution."
//!
//! Design references: DESIGN.md §8.1 (dedup against a content-addressed result
//! cache), §8.2 (small queryable envelope + append-only), §9.2 (replayable
//! derivations; impure never caches), §4.4 (`inputDigest` / `derivationKey`).

use spork_exec::CancelToken;

use crate::cache::ResultCache;
use crate::digest::{input_digest, ChangeScope, DerivationKey};
use crate::envelope::ResultEnvelope;
use crate::error::Result;
use crate::runner::{Runner, SandboxContext};
use crate::spec::CheckSpec;

/// Whether a [`check`](CachingRunner::check) was served from cache or executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// The stored envelope was returned without re-executing the runner.
    Hit,
    /// The runner executed and the fresh envelope was stored.
    Miss,
    /// The spec was impure, so the cache was bypassed and the runner executed
    /// (no store, no lookup).
    Bypassed,
}

impl CacheStatus {
    /// Whether the runner actually executed (a miss or an impure bypass), as
    /// opposed to a cache hit that skipped execution.
    #[must_use]
    pub fn executed(&self) -> bool {
        matches!(self, CacheStatus::Miss | CacheStatus::Bypassed)
    }
}

/// The result of one [`check`](CachingRunner::check): the normalized envelope
/// plus how the cache served it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    /// The normalized result.
    pub envelope: ResultEnvelope,
    /// Whether it was a hit, a miss, or an impure bypass.
    pub status: CacheStatus,
}

/// A [`Runner`] wrapped with the cache-and-impurity policy.
///
/// Owns the runner and a [`ResultCache`] and runs the full pipeline through
/// [`check`](CachingRunner::check). The runner's
/// [`describe`](Runner::describe) supplies the version and formula generation
/// folded into the cache key, so two runners with different formula generations
/// never read each other's entries.
#[derive(Debug)]
pub struct CachingRunner<R: Runner, C: ResultCache> {
    runner: R,
    cache: C,
}

impl<R: Runner, C: ResultCache> CachingRunner<R, C> {
    /// Wrap `runner` with `cache`.
    #[must_use]
    pub fn new(runner: R, cache: C) -> Self {
        CachingRunner { runner, cache }
    }

    /// Borrow the wrapped runner.
    #[must_use]
    pub fn runner(&self) -> &R {
        &self.runner
    }

    /// Borrow the wrapped cache.
    #[must_use]
    pub fn cache(&self) -> &C {
        &self.cache
    }

    /// Compute the [`DerivationKey`] for `spec` against `ctx`.
    ///
    /// Exposed so a caller can inspect or pre-compute a key (e.g. to check the
    /// cache out-of-band). The key folds in the input tree the check reads, the
    /// canonical config, the runner version, and the runner's formula
    /// generation.
    ///
    /// # Errors
    /// [`RunnerError::Canon`](crate::RunnerError::Canon) if the config cannot be
    /// canonicalized.
    pub fn derivation_key(&self, ctx: &SandboxContext, spec: &CheckSpec) -> Result<DerivationKey> {
        let caps = self.runner.describe();
        let scope = ChangeScope::from_paths(&ctx.changed_paths);
        let digest = input_digest(&ctx.input_tree, spec, &caps.version, &scope)?;
        Ok(DerivationKey::new(digest, caps.generation))
    }

    /// Run the full prepare → run → normalize pipeline with the cache in the
    /// loop, honoring impurity and append-only history.
    ///
    /// See the [module docs](crate::caching) for the four-step policy.
    ///
    /// # Errors
    /// Any [`RunnerError`](crate::RunnerError) from key derivation, cache I/O, or
    /// the underlying runner.
    pub fn check(
        &self,
        ctx: &SandboxContext,
        spec: &CheckSpec,
        signal: &CancelToken,
    ) -> Result<CheckRun> {
        // Impure specs bypass the cache entirely: never look up, never store,
        // always re-run (DESIGN.md §9.2).
        if spec.impure {
            let envelope = self.execute(ctx, spec, signal)?;
            return Ok(CheckRun {
                envelope,
                status: CacheStatus::Bypassed,
            });
        }

        let key = self.derivation_key(ctx, spec)?;

        // Cache hit: return the stored run unchanged, without re-executing
        // (append-only history is preserved — same run_id).
        if let Some(envelope) = self.cache.get(&key)? {
            return Ok(CheckRun {
                envelope,
                status: CacheStatus::Hit,
            });
        }

        // Cache miss: execute, store the fresh envelope, and return it.
        let envelope = self.execute(ctx, spec, signal)?;
        self.cache.put(&key, &envelope)?;
        Ok(CheckRun {
            envelope,
            status: CacheStatus::Miss,
        })
    }

    /// Run the runner's SPI (prepare → run → normalize) and stamp the result's
    /// `input_digest` with the cache key's input digest.
    fn execute(
        &self,
        ctx: &SandboxContext,
        spec: &CheckSpec,
        signal: &CancelToken,
    ) -> Result<ResultEnvelope> {
        let prepared = self.runner.prepare(ctx, spec)?;
        let raw = self.runner.run(prepared, signal)?;
        let mut envelope = self.runner.normalize(raw, spec)?;
        // Bind the envelope's input_digest to the derivation key's input digest
        // so the stored result self-describes the exact input it validated
        // (including the change scope), even for an impure run (where there is no
        // stored key). For a pure run this matches what the cache lookup used.
        let caps = self.runner.describe();
        let scope = ChangeScope::from_paths(&ctx.changed_paths);
        envelope.input_digest = input_digest(&ctx.input_tree, spec, &caps.version, &scope)?;
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::InMemoryResultCache;
    use crate::envelope::Outcome;
    use crate::runner::{RunnerCapabilities, SandboxContext};
    use spork_exec::{PreparedRun, RawRunOutput, Workspace};
    use spork_hash::Hash;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A runner that counts how many times it actually executed, so a test can
    /// prove a cache hit skipped execution.
    #[derive(Debug)]
    struct CountingRunner {
        runs: Arc<AtomicUsize>,
    }

    impl CountingRunner {
        fn new() -> Self {
            CountingRunner {
                runs: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Runner for CountingRunner {
        fn describe(&self) -> RunnerCapabilities {
            RunnerCapabilities {
                name: "counting".into(),
                version: "counting@1".into(),
                generation: 1,
                kinds: vec!["counting".into()],
                hermetic: true,
            }
        }
        fn prepare(&self, _ctx: &SandboxContext, _spec: &CheckSpec) -> Result<PreparedRun> {
            Ok(PreparedRun::new("noop", []))
        }
        fn run(&self, _prepared: PreparedRun, _signal: &CancelToken) -> Result<RawRunOutput> {
            // Record the run count in stdout so `normalize` can stamp a
            // per-execution-distinct outcome without interior mutability.
            let n = self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(RawRunOutput::new(
                Some(0),
                n.to_string().into_bytes(),
                Vec::new(),
            ))
        }
        fn normalize(&self, raw: RawRunOutput, _spec: &CheckSpec) -> Result<ResultEnvelope> {
            // The first execution (#0) passes; any later one fails, so two real
            // executions yield distinguishable envelopes.
            let n: usize = String::from_utf8_lossy(&raw.stdout).parse().unwrap_or(0);
            let outcome = if n == 0 {
                Outcome::Passed
            } else {
                Outcome::Failed
            };
            Ok(ResultEnvelope::new(
                outcome,
                ulid::Ulid::new(),
                Hash::from_bytes([0; 32]),
            ))
        }
        fn collect_artifacts(&self, _raw: &RawRunOutput) -> crate::envelope::ArtifactManifest {
            crate::envelope::ArtifactManifest::new()
        }
    }

    fn ctx() -> SandboxContext {
        // A workspace value is required; build a throwaway one via the public
        // fields. (The caching path never touches the filesystem with this fake
        // runner.)
        let lease = spork_exec::Lease {
            schema_version: 1,
            id: ulid::Ulid::new(),
            owner_pid: 0,
            ttl_ms: 1,
            heartbeat_ms: 1,
            durable: true,
            last_heartbeat_ms: 0,
        };
        let ws = Workspace {
            root: std::path::PathBuf::from("/tmp/ws"),
            snapshot: Hash::from_bytes([0; 32]),
            env_manifest_hash: Hash::from_bytes([0; 32]),
            lease,
        };
        SandboxContext::new(ws, Hash::from_bytes([42; 32]))
    }

    #[test]
    fn miss_then_hit_skips_re_execution() {
        let runner = CountingRunner::new();
        let runs = runner.runs.clone();
        let caching = CachingRunner::new(runner, InMemoryResultCache::new());
        let spec = CheckSpec::new("counting", serde_json::json!({}));
        let signal = CancelToken::new();
        let c = ctx();

        let first = caching.check(&c, &spec, &signal).unwrap();
        assert_eq!(first.status, CacheStatus::Miss);
        assert_eq!(runs.load(Ordering::SeqCst), 1);

        let second = caching.check(&c, &spec, &signal).unwrap();
        assert_eq!(second.status, CacheStatus::Hit);
        // The runner did NOT execute again.
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        // The hit returned the *first* run unchanged (same run_id, same outcome).
        assert_eq!(second.envelope.run_id, first.envelope.run_id);
        assert_eq!(second.envelope.outcome, Outcome::Passed);
    }

    #[test]
    fn impure_never_caches_and_always_re_runs() {
        let runner = CountingRunner::new();
        let runs = runner.runs.clone();
        let caching = CachingRunner::new(runner, InMemoryResultCache::new());
        let spec = CheckSpec::new("counting", serde_json::json!({})).impure();
        let signal = CancelToken::new();
        let c = ctx();

        let first = caching.check(&c, &spec, &signal).unwrap();
        assert_eq!(first.status, CacheStatus::Bypassed);
        let second = caching.check(&c, &spec, &signal).unwrap();
        assert_eq!(second.status, CacheStatus::Bypassed);
        // Both executed (impure bypasses the cache entirely).
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        // The cache stayed empty.
        assert!(caching.cache().is_empty());
        // Different runs => different envelopes (the second outcome flipped).
        assert_ne!(first.envelope.run_id, second.envelope.run_id);
    }

    #[test]
    fn cache_status_executed_predicate() {
        assert!(!CacheStatus::Hit.executed());
        assert!(CacheStatus::Miss.executed());
        assert!(CacheStatus::Bypassed.executed());
    }
}
