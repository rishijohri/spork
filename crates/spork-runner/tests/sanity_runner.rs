//! End-to-end tests for the F4 result-normalization seam: the
//! [`SanityRunner`](spork_runner::SanityRunner) running change-scoped against a
//! real [`WorktreeCowBackend`](spork_exec::WorktreeCowBackend) materialization,
//! with the content-addressed derivation cache in the loop.
//!
//! These prove the F4 definition-of-done for the runner crate (DESIGN.md §8.1,
//! §8.2, §9.2, §4.4):
//!
//! - A Sanity `CheckSpec` auto-runs change-scoped against a CoW materialization
//!   and produces an append-only [`ResultEnvelope`](spork_runner::ResultEnvelope)
//!   with violations + an outcome.
//! - An identical `(spec, input_tree, runner_version)` re-run is a **cache hit**
//!   that returns the stored run without re-executing.
//! - An **impure** spec never caches and always re-runs.
//! - A tests-shaped, a perf-shaped, and a lint-shaped result all round-trip
//!   through the same `ResultEnvelope` (no core code branches on kind).
//! - A metric-id rename is contained by the registry (no silent gate break).

use std::fs;
use std::path::Path;

use spork_asset::LocalCasAssetStore;
use spork_cas::{LooseStore, ObjectStore};
use spork_exec::{CancelToken, EnvManifest, IsolationBackend, LeaseLedger, WorktreeCowBackend};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use spork_runner::{
    input_digest, CacheStatus, CachingRunner, ChangeScope, CheckSpec, Direction,
    InMemoryResultCache, Metric, MetricDescriptor, MetricRegistry, Outcome, ResultEnvelope, Runner,
    SandboxContext, SanityRunner, UnitResult, UnitStatus, METRIC_VIOLATION_COUNT, SANITY_KIND,
    SANITY_VERSION,
};
use tempfile::TempDir;

/// A fully-wired backend over the real `spork-cas` / `spork-asset` stack, plus
/// the handles a test needs.
struct Harness {
    _tmp: TempDir,
    backend: WorktreeCowBackend<LooseStore, LocalCasAssetStore>,
    snapshot: spork_hash::Hash,
}

/// Build a checkout, capture it into a CAS object store, and wire a backend.
fn harness(build: impl FnOnce(&Path)) -> Harness {
    let tmp = TempDir::new().unwrap();
    let source_root = tmp.path().join("checkout");
    fs::create_dir_all(&source_root).unwrap();
    build(&source_root);

    let objects = ObjectStore::new(LooseStore::open(tmp.path().join("cas")).unwrap());
    let profile = IgnoreProfile::default_profile();
    let matcher = IgnoreMatcher::new(&profile);
    let (snapshot, _root_tree, _stats) = objects
        .capture_snapshot(&source_root, &matcher, profile.hash(), None)
        .unwrap();

    let assets = LocalCasAssetStore::open(tmp.path().join("assets")).unwrap();
    let ledger = LeaseLedger::open(tmp.path().join("leases.json")).unwrap();
    let backend = WorktreeCowBackend::new(objects, assets, tmp.path().join("worktrees"), ledger);

    Harness {
        _tmp: tmp,
        backend,
        snapshot,
    }
}

/// Provision a worktree and return a `SandboxContext` for it. The input-tree
/// hash is the snapshot the worktree was materialized from (the honestly-declared
/// input the sanity check reads).
fn provision(h: &Harness) -> SandboxContext {
    let env = EnvManifest::new();
    let ws = h.backend.provision(h.snapshot, &env).unwrap();
    SandboxContext::new(ws, h.snapshot)
}

#[test]
fn sanity_runs_against_materialized_tree_and_finds_violations() {
    let h = harness(|root| {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), b"fn main() {} // FIXME tidy up\n").unwrap();
        fs::write(root.join("src/ok.rs"), b"fn ok() {}\n").unwrap();
    });
    let ctx = provision(&h);
    let runner = SanityRunner::new();
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    let prepared = runner.prepare(&ctx, &spec).unwrap();
    let raw = runner.run(prepared, &signal).unwrap();
    let env = runner.normalize(raw, &spec).unwrap();

    assert_eq!(env.outcome, Outcome::Failed);
    assert_eq!(env.violations.len(), 1);
    assert_eq!(env.violations[0].rule_id, "forbid-pattern:FIXME");
    assert_eq!(env.violations[0].file, "src/main.rs");
    assert_eq!(env.violations[0].line, Some(1));
    // The envelope is keyed by the input tree it read.
    assert_eq!(env.input_digest, h.snapshot);

    // Backend never touched the user checkout: the original source still exists,
    // and the runner read only the worktree copy.
    h.backend.teardown(ctx.workspace).unwrap();
}

#[test]
fn sanity_passes_clean_tree() {
    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"fn a() {}\n").unwrap();
    });
    let ctx = provision(&h);
    let runner = SanityRunner::new();
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    let prepared = runner.prepare(&ctx, &spec).unwrap();
    let raw = runner.run(prepared, &signal).unwrap();
    let env = runner.normalize(raw, &spec).unwrap();
    assert_eq!(env.outcome, Outcome::Passed);
    assert!(env.violations.is_empty());
}

#[test]
fn change_scoping_only_scans_changed_paths() {
    let h = harness(|root| {
        fs::create_dir_all(root.join("src")).unwrap();
        // Two files each contain a forbidden pattern.
        fs::write(root.join("src/touched.rs"), b"// FIXME here\n").unwrap();
        fs::write(root.join("src/untouched.rs"), b"// FIXME there\n").unwrap();
    });
    let ctx = provision(&h).with_changed_paths(["src/touched.rs".to_string()]);
    assert!(ctx.is_change_scoped());

    let runner = SanityRunner::new();
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    let prepared = runner.prepare(&ctx, &spec).unwrap();
    let raw = runner.run(prepared, &signal).unwrap();
    let env = runner.normalize(raw, &spec).unwrap();

    // Only the changed file was scanned, so only its violation appears.
    assert_eq!(env.violations.len(), 1);
    assert_eq!(env.violations[0].file, "src/touched.rs");
}

#[test]
fn max_line_length_rule_against_worktree() {
    let h = harness(|root| {
        fs::write(
            root.join("wide.txt"),
            b"short\nthis line is definitely too long\n",
        )
        .unwrap();
    });
    let ctx = provision(&h);
    let runner = SanityRunner::new();
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "max_line_length": 10 }));
    let signal = CancelToken::new();

    let prepared = runner.prepare(&ctx, &spec).unwrap();
    let raw = runner.run(prepared, &signal).unwrap();
    let env = runner.normalize(raw, &spec).unwrap();
    assert_eq!(env.outcome, Outcome::Failed);
    assert_eq!(env.violations.len(), 1);
    assert_eq!(env.violations[0].rule_id, "max-line-length");
    assert_eq!(env.violations[0].line, Some(2));
}

#[test]
fn identical_rerun_is_a_cache_hit_no_reexecution() {
    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"// FIXME\n").unwrap();
    });
    let runner = SanityRunner::new();
    let cache = InMemoryResultCache::new();
    let caching = CachingRunner::new(runner, cache);
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    // First run against a freshly provisioned worktree: a cache miss.
    let ctx1 = provision(&h);
    let first = caching.check(&ctx1, &spec, &signal).unwrap();
    assert_eq!(first.status, CacheStatus::Miss);
    assert_eq!(first.envelope.outcome, Outcome::Failed);

    // A second provisioning of the SAME snapshot yields the SAME input tree, so
    // an identical (spec, input_tree, runner_version) is a cache HIT that returns
    // the stored run unchanged — no re-execution, same run_id (append-only).
    let ctx2 = provision(&h);
    assert_eq!(ctx1.input_tree, ctx2.input_tree);
    let second = caching.check(&ctx2, &spec, &signal).unwrap();
    assert_eq!(second.status, CacheStatus::Hit);
    assert_eq!(second.envelope.run_id, first.envelope.run_id);
    assert_eq!(second.envelope, first.envelope);
    assert_eq!(caching.cache().len(), 1);
}

#[test]
fn impure_spec_never_caches() {
    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"// FIXME\n").unwrap();
    });
    let runner = SanityRunner::new();
    let caching = CachingRunner::new(runner, InMemoryResultCache::new());
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] })).impure();
    let signal = CancelToken::new();

    let ctx1 = provision(&h);
    let first = caching.check(&ctx1, &spec, &signal).unwrap();
    assert_eq!(first.status, CacheStatus::Bypassed);

    let ctx2 = provision(&h);
    let second = caching.check(&ctx2, &spec, &signal).unwrap();
    assert_eq!(second.status, CacheStatus::Bypassed);

    // Each impure run is a fresh execution with its own run_id; nothing cached.
    assert_ne!(first.envelope.run_id, second.envelope.run_id);
    assert!(caching.cache().is_empty());
}

#[test]
fn different_config_is_a_distinct_cache_entry() {
    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"// FIXME and TODO\n").unwrap();
    });
    let runner = SanityRunner::new();
    let caching = CachingRunner::new(runner, InMemoryResultCache::new());
    let signal = CancelToken::new();

    let ctx = provision(&h);
    let fixme = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let todo = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["TODO"] }));

    let a = caching.check(&ctx, &fixme, &signal).unwrap();
    let b = caching.check(&ctx, &todo, &signal).unwrap();
    assert_eq!(a.status, CacheStatus::Miss);
    assert_eq!(b.status, CacheStatus::Miss); // distinct config => distinct entry
    assert_eq!(caching.cache().len(), 2);
    // Both differ in which rule fired.
    assert_eq!(a.envelope.violations[0].rule_id, "forbid-pattern:FIXME");
    assert_eq!(b.envelope.violations[0].rule_id, "forbid-pattern:TODO");
}

#[test]
fn input_digest_matches_caching_runner_derivation() {
    // The standalone `input_digest` helper and the CachingRunner agree on the
    // key, so an out-of-band cache probe and the in-loop lookup target the same
    // entry.
    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"ok\n").unwrap();
    });
    let ctx = provision(&h);
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let runner = SanityRunner::new();
    let caching = CachingRunner::new(SanityRunner::new(), InMemoryResultCache::new());

    let direct = input_digest(&ctx.input_tree, &spec, SANITY_VERSION, &ChangeScope::Full).unwrap();
    let key = caching.derivation_key(&ctx, &spec).unwrap();
    assert_eq!(direct, key.input_digest);
    assert_eq!(runner.describe().version, SANITY_VERSION);
}

#[test]
fn change_scoped_run_does_not_stale_hit_a_full_scan() {
    // The cache-soundness regression: a change-scoped run (scans one file) and a
    // full scan over the SAME snapshot must be DISTINCT cache entries. If the
    // scope were absent from the key, the change-scoped result would be a stale
    // hit for the full scan — a false green over the unscanned files (DESIGN.md
    // §8.1 under-declaration hazard).
    let h = harness(|root| {
        fs::create_dir_all(root.join("src")).unwrap();
        // Two files, each with a forbidden pattern.
        fs::write(root.join("src/touched.rs"), b"// FIXME here\n").unwrap();
        fs::write(root.join("src/untouched.rs"), b"// FIXME there\n").unwrap();
    });
    let runner = SanityRunner::new();
    let caching = CachingRunner::new(runner, InMemoryResultCache::new());
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    // A change-scoped run that scans only one of the two offending files.
    let scoped = provision(&h).with_changed_paths(["src/touched.rs".to_string()]);
    let scoped_run = caching.check(&scoped, &spec, &signal).unwrap();
    assert_eq!(scoped_run.status, CacheStatus::Miss);
    assert_eq!(scoped_run.envelope.violations.len(), 1);

    // A full scan over the SAME snapshot must be a MISS (not a stale hit on the
    // scoped result), and must see BOTH violations.
    let full = provision(&h);
    assert_eq!(full.input_tree, scoped.input_tree); // same snapshot
    let full_run = caching.check(&full, &spec, &signal).unwrap();
    assert_eq!(
        full_run.status,
        CacheStatus::Miss,
        "full scan must not be a stale hit on the change-scoped entry"
    );
    assert_eq!(
        full_run.envelope.violations.len(),
        2,
        "full scan must see both offending files"
    );
    // Two distinct cache entries: scoped and full are different derivations.
    assert_eq!(caching.cache().len(), 2);

    // Re-running the same full scan IS a hit (proves the full-scope key is stable).
    let full2 = provision(&h);
    let full_run2 = caching.check(&full2, &spec, &signal).unwrap();
    assert_eq!(full_run2.status, CacheStatus::Hit);
    assert_eq!(full_run2.envelope.run_id, full_run.envelope.run_id);
}

#[test]
fn three_shapes_round_trip_through_one_envelope() {
    // The crux definition-of-done: a lint-shaped (real, from the runner), a
    // tests-shaped, and a perf-shaped result are all the SAME `ResultEnvelope`
    // type, and a single function consumes all three with no kind branch.
    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"// FIXME\n").unwrap();
    });
    let ctx = provision(&h);
    let runner = SanityRunner::new();
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    // Lint-shaped: produced by the real runner.
    let prepared = runner.prepare(&ctx, &spec).unwrap();
    let raw = runner.run(prepared, &signal).unwrap();
    let lint: ResultEnvelope = runner.normalize(raw, &spec).unwrap();
    assert!(!lint.violations.is_empty());

    // Tests-shaped: constructed in the same struct.
    let tests = ResultEnvelope::new(Outcome::Passed, ulid::Ulid::new(), ctx.input_tree)
        .with_unit(UnitResult::new("suite::a", UnitStatus::Passed))
        .with_unit(UnitResult::new("suite::b", UnitStatus::Failed).with_detail("boom"));

    // Perf-shaped: typed metrics with a direction, same struct.
    let perf = ResultEnvelope::new(Outcome::Passed, ulid::Ulid::new(), ctx.input_tree)
        .with_metric(Metric::new("p99_latency_ms", 1200, Direction::LowerBetter))
        .with_metric(Metric::new("throughput_rps", 9000, Direction::HigherBetter));

    // One generic consumer over all three — no `match kind`.
    fn count_passing(envs: &[ResultEnvelope]) -> usize {
        envs.iter().filter(|e| e.outcome.is_passing()).count()
    }
    let all = vec![lint.clone(), tests.clone(), perf.clone()];
    assert_eq!(count_passing(&all), 2);

    // And all three serialize/deserialize identically through the one type.
    for env in [lint, tests, perf] {
        let json = serde_json::to_string(&env).unwrap();
        let back: ResultEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }
}

#[test]
fn metric_rename_is_contained_by_the_registry() {
    // A deployment renamed `violation_count` to `lint_violations` but a gate
    // still keys off the OLD id. A registry that maps the old id as an alias of
    // the new canonical id keeps that gate working — no silent break — and the
    // runner emits the canonical id.
    let mut registry = MetricRegistry::new();
    registry.register(
        MetricDescriptor::new("lint_violations", Direction::LowerBetter)
            .with_alias(METRIC_VIOLATION_COUNT),
    );
    // The sanity runner also needs the files-scanned metric registered.
    registry.register(MetricDescriptor::new(
        spork_runner::METRIC_FILES_SCANNED,
        Direction::HigherBetter,
    ));

    let h = harness(|root| {
        fs::write(root.join("a.rs"), b"// FIXME\n").unwrap();
    });
    let ctx = provision(&h);
    let runner = SanityRunner::with_registry(registry.clone());
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    let prepared = runner.prepare(&ctx, &spec).unwrap();
    let raw = runner.run(prepared, &signal).unwrap();
    let env = runner.normalize(raw, &spec).unwrap();

    // The runner emitted the metric under the *canonical* (new) id...
    let emitted = env
        .metrics
        .iter()
        .find(|m| m.id.as_str() == "lint_violations")
        .expect("metric emitted under canonical id");
    assert_eq!(emitted.value, 1);

    // ...and a gate written against the OLD id still resolves through the
    // registry to the same descriptor — the rename did not break it.
    assert_eq!(
        registry
            .canonicalize(METRIC_VIOLATION_COUNT)
            .unwrap()
            .as_str(),
        "lint_violations"
    );

    // A genuinely unknown id is still a typed error (no silent pass).
    let probe = Metric::new("totally_unknown_metric", 1, Direction::LowerBetter);
    assert!(registry.canonicalize_metric(&probe).is_err());
}

#[test]
fn append_only_history_two_distinct_runs_for_a_changed_tree() {
    // A re-run against a *different* input tree is a new run (new run_id, new
    // input_digest) rather than a cache hit — append-only history across edits.
    let runner = SanityRunner::new();
    let caching = CachingRunner::new(runner, InMemoryResultCache::new());
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let signal = CancelToken::new();

    let dirty = harness(|root| {
        fs::write(root.join("a.rs"), b"// FIXME\n").unwrap();
    });
    let clean = harness(|root| {
        fs::write(root.join("a.rs"), b"// fixed\n").unwrap();
    });
    assert_ne!(dirty.snapshot, clean.snapshot);

    let ctx_dirty = provision(&dirty);
    let ctx_clean = provision(&clean);

    let r1 = caching.check(&ctx_dirty, &spec, &signal).unwrap();
    let r2 = caching.check(&ctx_clean, &spec, &signal).unwrap();
    assert_eq!(r1.status, CacheStatus::Miss);
    assert_eq!(r2.status, CacheStatus::Miss);
    assert_ne!(r1.envelope.run_id, r2.envelope.run_id);
    assert_ne!(r1.envelope.input_digest, r2.envelope.input_digest);
    assert_eq!(r1.envelope.outcome, Outcome::Failed);
    assert_eq!(r2.envelope.outcome, Outcome::Passed);
    // Two distinct cache entries: a full append-only history across the edit.
    assert_eq!(caching.cache().len(), 2);
}
