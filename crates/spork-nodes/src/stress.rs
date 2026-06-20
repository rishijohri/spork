//! The **Stress-Test** built-in node type and its Runner (DESIGN.md §7.1, §8.1,
//! §8.2).
//!
//! A Stress node is an *observing* kind (`owns_snapshot = false`): it runs a
//! configured load/perf command against a worktree materialized from the target
//! node's snapshot and attaches an append-only
//! [`ResultEnvelope`](spork_runner::ResultEnvelope) of **perf metrics** to that
//! node without mutating it (DESIGN.md §8.1, the `StressRunner` row: "k6 / locust
//! / wrk + fuzzers → p50/p99 latency, throughput, peak mem, fuzz corpus").
//!
//! # Perf metrics with a direction
//!
//! The load-bearing surface is the normalized
//! [`Metric`](spork_runner::Metric)s: each carries an optimization
//! [`Direction`](spork_runner::Direction) so the diff/gate engine knows which way
//! a delta is an improvement without a per-id table in the core (DESIGN.md §8.1).
//! Latency and memory are [`Direction::LowerBetter`](spork_runner::Direction::LowerBetter);
//! throughput is [`Direction::HigherBetter`](spork_runner::Direction::HigherBetter).
//! Every metric id is validated through a [`MetricRegistry`](spork_runner::MetricRegistry)
//! ([`stress_metric_registry`]) so an unknown id is a typed error, never a silent
//! gate break (DESIGN.md §8.3).
//!
//! Values are integers (latency in **microseconds**, memory in **bytes**,
//! throughput in **requests/second**) because identity-bearing data must be
//! byte-stable — `spork-canon` forbids floats (DESIGN.md §6.1).
//!
//! # Fuzz corpus (optional)
//!
//! A stress run may carry an optional fuzz corpus reference; it is recorded as a
//! metric count (`fuzz_corpus_size`) and, in a live runner, the corpus blobs
//! would be content-addressed into the artifact manifest. Offline, the perf
//! numbers and the corpus size are supplied through the spec config so the
//! metric mapping is exercised deterministically.
//!
//! Design references: DESIGN.md §7.1 (taxonomy), §8.1 (one Runner SPI; typed
//! metrics with a direction), §8.2 (execution against a materialized tree;
//! append-only results), §8.3 (gates compare metric deltas by id).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spork_exec::{CancelToken, PreparedRun, RawRunOutput};
use spork_graph::EdgeType;
use spork_registry::{Family, NodeTypeDescriptor, StalenessRule};
use spork_runner::{
    ArtifactManifest, CheckSpec, Direction, Metric, MetricDescriptor, MetricRegistry, Outcome,
    ResultEnvelope, Runner, RunnerCapabilities, RunnerError, SandboxContext,
};

use crate::descriptor::type_version;

/// The check kind the stress runner handles.
pub const STRESS_KIND: &str = "stress";

/// The stress runner's version string, folded into the cache key.
pub const STRESS_VERSION: &str = "stress@1";

/// The stress runner's derivation-formula generation (DESIGN.md §9.2).
pub const STRESS_GENERATION: u32 = 1;

/// The schema version stamped on a freshly built [`StressPayload`].
pub const STRESS_PAYLOAD_VERSION: u16 = 1;

/// The marker program name a stress [`PreparedRun`] carries.
const STRESS_PROGRAM: &str = "spork-stress";

/// Metric id: 50th-percentile latency, in microseconds (lower is better).
pub const METRIC_P50_LATENCY_US: &str = "p50_latency_us";
/// Metric id: 95th-percentile latency, in microseconds (lower is better).
pub const METRIC_P95_LATENCY_US: &str = "p95_latency_us";
/// Metric id: 99th-percentile latency, in microseconds (lower is better).
pub const METRIC_P99_LATENCY_US: &str = "p99_latency_us";
/// Metric id: throughput, in requests per second (higher is better).
pub const METRIC_THROUGHPUT_RPS: &str = "throughput_rps";
/// Metric id: peak resident memory, in bytes (lower is better).
pub const METRIC_PEAK_MEM_BYTES: &str = "peak_mem_bytes";
/// Metric id: number of fuzz-corpus inputs (informational; higher is better as
/// more coverage).
pub const METRIC_FUZZ_CORPUS_SIZE: &str = "fuzz_corpus_size";

/// Build a [`MetricRegistry`] carrying the perf metric vocabulary the stress
/// runner emits, with correct optimization directions (DESIGN.md §8.1).
///
/// Every metric the runner emits is validated against this registry, so an
/// unknown id is a typed [`RunnerError::UnknownMetric`] rather than a silent gate
/// break (DESIGN.md §8.3). A deployment that renamed a metric supplies an
/// alias-carrying registry via [`StressRunner::with_registry`].
#[must_use]
pub fn stress_metric_registry() -> MetricRegistry {
    let mut reg = MetricRegistry::new();
    for id in [
        METRIC_P50_LATENCY_US,
        METRIC_P95_LATENCY_US,
        METRIC_P99_LATENCY_US,
        METRIC_PEAK_MEM_BYTES,
    ] {
        reg.register(MetricDescriptor::new(id, Direction::LowerBetter));
    }
    reg.register(MetricDescriptor::new(
        METRIC_THROUGHPUT_RPS,
        Direction::HigherBetter,
    ));
    reg.register(MetricDescriptor::new(
        METRIC_FUZZ_CORPUS_SIZE,
        Direction::HigherBetter,
    ));
    reg
}

/// The schema-versioned payload of a Stress node (DESIGN.md §7.1).
///
/// `target_node_id` is the node whose snapshot the check observes; `command` is
/// the configured load command; `duration_ms`, `concurrency`, and `seed` form the
/// load/fuzz profile (DESIGN.md §7.1 payload core). The profile feeds the cache
/// key (via the spec config) so two runs with different load shapes are distinct
/// derivations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StressPayload {
    /// The schema version of this payload ([`STRESS_PAYLOAD_VERSION`]).
    pub schema_version: u16,
    /// The node this stress test observes (its snapshot is never mutated).
    pub target_node_id: String,
    /// The configured load command (e.g. `"k6 run load.js"`, `"wrk -t4 ..."`).
    pub command: String,
    /// The load duration, in milliseconds.
    pub duration_ms: u64,
    /// The concurrency level (virtual users / connections).
    pub concurrency: u32,
    /// The deterministic seed for any fuzzing, so a run is reproducible.
    pub seed: u64,
}

impl StressPayload {
    /// Construct a stress payload targeting `target_node_id` with `command` and a
    /// default profile (1s, concurrency 1, seed 0).
    #[must_use]
    pub fn new(target_node_id: impl Into<String>, command: impl Into<String>) -> Self {
        StressPayload {
            schema_version: STRESS_PAYLOAD_VERSION,
            target_node_id: target_node_id.into(),
            command: command.into(),
            duration_ms: 1000,
            concurrency: 1,
            seed: 0,
        }
    }

    /// Set the load profile (builder style).
    #[must_use]
    pub fn with_profile(mut self, duration_ms: u64, concurrency: u32, seed: u64) -> Self {
        self.duration_ms = duration_ms;
        self.concurrency = concurrency;
        self.seed = seed;
        self
    }

    /// Render this payload to the JSON `payload` value the daemon stores.
    ///
    /// # Errors
    /// [`NodesError::Serialize`](crate::NodesError::Serialize) on an encoding
    /// failure (not possible for this float-free struct in practice).
    pub fn to_value(&self) -> crate::Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// The Stress-Test runner — an observing [`Runner`] behind the F4 SPI.
///
/// It maps a perf report into the typed [`Metric`]s of the one
/// [`ResultEnvelope`](spork_runner::ResultEnvelope), validating every id through
/// its [`MetricRegistry`] (DESIGN.md §8.1, §8.3). Offline, the perf numbers are
/// supplied through the spec config (`metrics`) so the mapping is exercised
/// deterministically.
#[derive(Debug)]
pub struct StressRunner {
    registry: MetricRegistry,
}

impl Default for StressRunner {
    fn default() -> Self {
        StressRunner::new()
    }
}

impl StressRunner {
    /// Construct a stress runner with the built-in perf metric registry.
    #[must_use]
    pub fn new() -> Self {
        StressRunner {
            registry: stress_metric_registry(),
        }
    }

    /// Construct a stress runner with an explicit metric registry (e.g. one
    /// carrying renamed-metric aliases).
    #[must_use]
    pub fn with_registry(registry: MetricRegistry) -> Self {
        StressRunner { registry }
    }

    /// Borrow the runner's metric registry.
    #[must_use]
    pub fn registry(&self) -> &MetricRegistry {
        &self.registry
    }

    /// The default optimization direction for a built-in perf metric id.
    fn direction_for(id: &str) -> Direction {
        match id {
            METRIC_THROUGHPUT_RPS | METRIC_FUZZ_CORPUS_SIZE => Direction::HigherBetter,
            _ => Direction::LowerBetter,
        }
    }
}

impl Runner for StressRunner {
    fn describe(&self) -> RunnerCapabilities {
        RunnerCapabilities {
            name: "stress".into(),
            version: STRESS_VERSION.into(),
            generation: STRESS_GENERATION,
            kinds: vec![STRESS_KIND.into()],
            hermetic: false,
        }
    }

    fn prepare(&self, ctx: &SandboxContext, spec: &CheckSpec) -> Result<PreparedRun, RunnerError> {
        if spec.kind != STRESS_KIND {
            return Err(RunnerError::UnsupportedKind {
                kind: spec.kind.clone(),
                runner: "stress".into(),
            });
        }
        let command = spec
            .config
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| RunnerError::Config("stress config requires a string `command`".into()))?
            .to_string();
        // The perf report the run will normalize is carried through, so the
        // metric mapping is exercised offline without a live load tool.
        let metrics = spec
            .config
            .get("metrics")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let prepared = PreparedRun::new(STRESS_PROGRAM, [command])
            .with_env("SPORK_STRESS_ROOT", ctx.workspace.root.to_string_lossy())
            .with_env("SPORK_STRESS_METRICS", metrics.to_string());
        Ok(prepared)
    }

    fn run(
        &self,
        prepared: PreparedRun,
        signal: &CancelToken,
    ) -> Result<RawRunOutput, RunnerError> {
        if signal.is_cancelled() {
            return Err(RunnerError::Exec("stress run cancelled".into()));
        }
        // A live StressRunner spawns the load tool and parses its perf report;
        // offline, the metrics report is carried through and emitted as stdout —
        // the same bytes `normalize` reads.
        let metrics = prepared
            .env
            .get("SPORK_STRESS_METRICS")
            .cloned()
            .unwrap_or_else(|| "{}".to_string());
        Ok(RawRunOutput::new(Some(0), metrics.into_bytes(), Vec::new()))
    }

    fn normalize(
        &self,
        raw: RawRunOutput,
        spec: &CheckSpec,
    ) -> Result<ResultEnvelope, RunnerError> {
        if spec.kind != STRESS_KIND {
            return Err(RunnerError::UnsupportedKind {
                kind: spec.kind.clone(),
                runner: "stress".into(),
            });
        }
        let report: Value = serde_json::from_slice(&raw.stdout)
            .map_err(|e| RunnerError::Config(format!("stress report was not valid JSON: {e}")))?;
        let obj = report.as_object().ok_or_else(|| {
            RunnerError::Config("stress report must be a JSON object of metric -> integer".into())
        })?;

        let mut envelope = ResultEnvelope::new(
            Outcome::Passed,
            ulid::Ulid::new(),
            spork_hash::Hash::from_bytes([0; 32]),
        );
        // Map every reported metric to a typed, registry-validated Metric. Iterate
        // in sorted id order so the envelope is byte-stable across runs.
        let mut ids: Vec<&String> = obj.keys().collect();
        ids.sort();
        for id in ids {
            let value = obj.get(id).and_then(Value::as_i64).ok_or_else(|| {
                RunnerError::Config(format!("stress metric {id:?} must be an integer"))
            })?;
            let metric = self.registry.canonicalize_metric(&Metric::new(
                id.clone(),
                value,
                Self::direction_for(id),
            ))?;
            envelope = envelope.with_metric(metric);
        }
        Ok(envelope)
    }

    fn collect_artifacts(&self, raw: &RawRunOutput) -> ArtifactManifest {
        // A live runner content-addresses the fuzz corpus + perf traces here;
        // offline there are none. Reading `raw` keeps the signature honest about
        // what a live impl consumes.
        let _ = raw.stdout.len();
        ArtifactManifest::new()
    }
}

/// Build the Stress-Test [`NodeTypeDescriptor`] (DESIGN.md §7.1).
///
/// A [`Family::Observing`] type owning no snapshot, originating a
/// [`EdgeType::Stresses`] edge to the Edit it observes (DESIGN.md §6.3), going
/// stale when the observed snapshot changes (DESIGN.md §7.3).
#[must_use]
pub fn descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: STRESS_KIND.to_string(),
        type_version: type_version(),
        family: Family::Observing,
        owns_snapshot: false,
        payload_schema: json!({
            "type": "object",
            "properties": {
                "schema_version": { "type": "integer", "minimum": 1 },
                "target_node_id": { "type": "string" },
                "command": { "type": "string" },
                "duration_ms": { "type": "integer", "minimum": 0 },
                "concurrency": { "type": "integer", "minimum": 1 },
                "seed": { "type": "integer", "minimum": 0 }
            },
            "required": ["schema_version", "target_node_id", "command", "duration_ms", "concurrency", "seed"]
        }),
        result_schema: Some(json!({
            "type": "object",
            "description": "ResultEnvelope with perf metrics (p50/p95/p99/throughput/peak_mem), DESIGN §8.1"
        })),
        allowed_edges: vec![EdgeType::Stresses],
        ports: vec![],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec!["snapshot.read".to_string(), "process.spawn".to_string()],
        ui_contributions: json!({ "color": "#a855f7", "icon": "activity", "displayName": "Stress" }),
        revoked_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_registry::NodeTypeRegistry;

    fn report() -> Value {
        json!({
            METRIC_P50_LATENCY_US: 40_000,
            METRIC_P95_LATENCY_US: 90_000,
            METRIC_P99_LATENCY_US: 120_000,
            METRIC_THROUGHPUT_RPS: 9000,
            METRIC_PEAK_MEM_BYTES: 524_288_000_i64,
        })
    }

    fn spec_with(report: Value) -> CheckSpec {
        CheckSpec::new(
            STRESS_KIND,
            json!({ "command": "k6 run load.js", "metrics": report }),
        )
    }

    #[test]
    fn descriptor_is_observing_and_owns_no_snapshot() {
        let d = descriptor();
        assert_eq!(d.id, STRESS_KIND);
        assert_eq!(d.family, Family::Observing);
        assert!(!d.owns_snapshot);
        let mut reg = NodeTypeRegistry::new();
        reg.register(d).unwrap();
    }

    #[test]
    fn metric_registry_has_correct_directions() {
        let reg = stress_metric_registry();
        assert_eq!(
            reg.descriptor(METRIC_P99_LATENCY_US).unwrap().direction,
            Direction::LowerBetter
        );
        assert_eq!(
            reg.descriptor(METRIC_THROUGHPUT_RPS).unwrap().direction,
            Direction::HigherBetter
        );
        assert_eq!(
            reg.descriptor(METRIC_PEAK_MEM_BYTES).unwrap().direction,
            Direction::LowerBetter
        );
    }

    #[test]
    fn normalize_maps_report_to_typed_metrics() {
        let runner = StressRunner::new();
        let raw = RawRunOutput::new(Some(0), serde_json::to_vec(&report()).unwrap(), Vec::new());
        let env = runner.normalize(raw, &spec_with(report())).unwrap();
        assert_eq!(env.outcome, Outcome::Passed);
        assert_eq!(env.metrics.len(), 5);
        let p99 = env
            .metrics
            .iter()
            .find(|m| m.id.as_str() == METRIC_P99_LATENCY_US)
            .unwrap();
        assert_eq!(p99.value, 120_000);
        assert_eq!(p99.direction, Direction::LowerBetter);
        let tput = env
            .metrics
            .iter()
            .find(|m| m.id.as_str() == METRIC_THROUGHPUT_RPS)
            .unwrap();
        assert_eq!(tput.direction, Direction::HigherBetter);
    }

    #[test]
    fn normalize_refuses_unknown_metric_id() {
        let runner = StressRunner::new();
        let report = json!({ "made_up_metric": 1 });
        let raw = RawRunOutput::new(Some(0), serde_json::to_vec(&report).unwrap(), Vec::new());
        let err = runner.normalize(raw, &spec_with(report)).unwrap_err();
        assert!(matches!(err, RunnerError::UnknownMetric(_)));
    }

    #[test]
    fn normalize_refuses_non_integer_metric() {
        let runner = StressRunner::new();
        let report = json!({ METRIC_THROUGHPUT_RPS: 9000.5 });
        let raw = RawRunOutput::new(Some(0), serde_json::to_vec(&report).unwrap(), Vec::new());
        assert!(matches!(
            runner.normalize(raw, &spec_with(report)).unwrap_err(),
            RunnerError::Config(_)
        ));
    }

    #[test]
    fn fuzz_corpus_size_is_a_higher_better_metric() {
        let runner = StressRunner::new();
        let report = json!({ METRIC_FUZZ_CORPUS_SIZE: 256 });
        let raw = RawRunOutput::new(Some(0), serde_json::to_vec(&report).unwrap(), Vec::new());
        let env = runner.normalize(raw, &spec_with(report)).unwrap();
        let m = env
            .metrics
            .iter()
            .find(|m| m.id.as_str() == METRIC_FUZZ_CORPUS_SIZE)
            .unwrap();
        assert_eq!(m.value, 256);
        assert_eq!(m.direction, Direction::HigherBetter);
    }

    #[test]
    fn payload_is_versioned_and_round_trips() {
        let p = StressPayload::new("01H...", "wrk").with_profile(5000, 16, 42);
        assert_eq!(p.schema_version, STRESS_PAYLOAD_VERSION);
        let v = p.to_value().unwrap();
        let back: StressPayload = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
        assert_eq!(back.concurrency, 16);
        assert_eq!(back.seed, 42);
    }
}
