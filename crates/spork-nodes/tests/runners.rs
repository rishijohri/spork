//! End-to-end tests for the P5 observing runners over a real
//! [`WorktreeCowBackend`](spork_exec::WorktreeCowBackend) materialization, with
//! the F4 content-addressed derivation cache in the loop.
//!
//! These prove the P5 DoD for the runner side (DESIGN.md §8.1, §8.2, §9.2):
//!
//! - the Validation runner maps JUnit-XML into per-unit results on the one
//!   [`ResultEnvelope`](spork_runner::ResultEnvelope), run through the SPI;
//! - the Stress runner maps a perf report into typed metrics with a direction;
//! - the reused F4 Sanity runner finds violations change-scoped against the
//!   materialized tree;
//! - every runner round-trips through the F4
//!   [`CachingRunner`](spork_runner::CachingRunner): an identical re-run on the
//!   same input tree is a **cache hit** that does not re-execute (DESIGN §8.1,
//!   §9.2) — the cache-hit-on-unchanged-subtree property the auto-run relies on.

use std::fs;
use std::path::Path;

use spork_asset::LocalCasAssetStore;
use spork_cas::{LooseStore, ObjectStore};
use spork_exec::{EnvManifest, IsolationBackend, LeaseLedger, WorktreeCowBackend};
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use spork_nodes::{sanity_runner, StressRunner, ValidationRunner, STRESS_KIND, VALIDATION_KIND};
use spork_runner::{
    CacheStatus, CachingRunner, CheckSpec, Direction, InMemoryResultCache, Outcome, Runner,
    SandboxContext, UnitStatus, SANITY_KIND,
};
use tempfile::TempDir;

/// A fully-wired backend over the real CAS + asset stack.
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

/// Provision a fresh worktree and build a `SandboxContext` for it.
fn provision(h: &Harness) -> SandboxContext {
    let env = EnvManifest::new();
    let ws = h.backend.provision(h.snapshot, &env).unwrap();
    SandboxContext::new(ws, h.snapshot)
}

#[test]
fn validation_runner_maps_junit_to_units_through_the_spi() {
    let h = harness(|root| {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), b"fn f() {}\n").unwrap();
    });
    let ctx = provision(&h);
    let junit = r#"<testsuite>
        <testcase classname="m" name="a"/>
        <testcase classname="m" name="b"><failure message="boom"/></testcase>
        <testcase classname="m" name="c"><skipped/></testcase>
    </testsuite>"#;
    let spec = CheckSpec::new(
        VALIDATION_KIND,
        serde_json::json!({ "command": "cargo test", "junit_xml": junit }),
    );
    let caching = CachingRunner::new(ValidationRunner::new(), InMemoryResultCache::new());

    let first = caching
        .check(&ctx, &spec, &spork_exec::CancelToken::new())
        .unwrap();
    assert_eq!(first.status, CacheStatus::Miss);
    assert_eq!(first.envelope.outcome, Outcome::Failed); // one failing unit
    assert_eq!(first.envelope.units.len(), 3);
    let statuses: Vec<UnitStatus> = first.envelope.units.iter().map(|u| u.status).collect();
    assert!(statuses.contains(&UnitStatus::Passed));
    assert!(statuses.contains(&UnitStatus::Failed));
    assert!(statuses.contains(&UnitStatus::Skipped));

    // An identical re-run on the same input tree is a cache hit (no re-execute).
    let ctx2 = provision(&h); // same snapshot => same input tree
    let second = caching
        .check(&ctx2, &spec, &spork_exec::CancelToken::new())
        .unwrap();
    assert_eq!(second.status, CacheStatus::Hit);
    assert_eq!(second.envelope.run_id, first.envelope.run_id);
}

#[test]
fn stress_runner_maps_report_to_typed_metrics_through_the_spi() {
    let h = harness(|root| {
        fs::write(root.join("README.md"), b"hi\n").unwrap();
    });
    let ctx = provision(&h);
    let report = serde_json::json!({
        "p50_latency_us": 40000,
        "p99_latency_us": 120000,
        "throughput_rps": 9000,
        "peak_mem_bytes": 524288000_i64,
    });
    let spec = CheckSpec::new(
        STRESS_KIND,
        serde_json::json!({ "command": "k6 run load.js", "metrics": report }),
    );
    let caching = CachingRunner::new(StressRunner::new(), InMemoryResultCache::new());

    let run = caching
        .check(&ctx, &spec, &spork_exec::CancelToken::new())
        .unwrap();
    assert_eq!(run.status, CacheStatus::Miss);
    assert_eq!(run.envelope.outcome, Outcome::Passed);
    assert_eq!(run.envelope.metrics.len(), 4);
    let p99 = run
        .envelope
        .metrics
        .iter()
        .find(|m| m.id.as_str() == "p99_latency_us")
        .unwrap();
    assert_eq!(p99.value, 120000);
    assert_eq!(p99.direction, Direction::LowerBetter);
    let tput = run
        .envelope
        .metrics
        .iter()
        .find(|m| m.id.as_str() == "throughput_rps")
        .unwrap();
    assert_eq!(tput.direction, Direction::HigherBetter);
}

#[test]
fn reused_sanity_runner_finds_violations_change_scoped() {
    let h = harness(|root| {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), b"fn main() {} // FIXME later\n").unwrap();
        fs::write(root.join("src/clean.rs"), b"fn clean() {}\n").unwrap();
    });
    // Change-scoped to only the file with the violation.
    let ctx = provision(&h).with_changed_paths(["src/main.rs".to_string()]);
    let spec = CheckSpec::new(SANITY_KIND, serde_json::json!({ "forbid": ["FIXME"] }));
    let caching = CachingRunner::new(sanity_runner(), InMemoryResultCache::new());

    let run = caching
        .check(&ctx, &spec, &spork_exec::CancelToken::new())
        .unwrap();
    assert_eq!(run.status, CacheStatus::Miss);
    assert_eq!(run.envelope.outcome, Outcome::Failed);
    assert_eq!(run.envelope.violations.len(), 1);
    assert_eq!(run.envelope.violations[0].rule_id, "forbid-pattern:FIXME");
    assert_eq!(run.envelope.violations[0].file, "src/main.rs");

    // Re-run on the same input tree + change scope is a cache hit.
    let ctx2 = provision(&h).with_changed_paths(["src/main.rs".to_string()]);
    let second = caching
        .check(&ctx2, &spec, &spork_exec::CancelToken::new())
        .unwrap();
    assert_eq!(second.status, CacheStatus::Hit);
}

#[test]
fn validation_runner_handles_only_its_kind() {
    // Routing through the wrong runner is a typed refusal, not a silent wrong
    // result — the one-SPI dispatch contract (DESIGN §8.1).
    let h = harness(|root| {
        fs::write(root.join("x"), b"y").unwrap();
    });
    let ctx = provision(&h);
    let spec = CheckSpec::new(STRESS_KIND, serde_json::json!({ "command": "x" }));
    let runner = ValidationRunner::new();
    let prepared = runner.prepare(&ctx, &spec);
    assert!(prepared.is_err(), "validation must refuse a stress spec");
}
