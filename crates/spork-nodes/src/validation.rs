//! The **Validation / Test** built-in node type and its Runner (DESIGN.md §7.1,
//! §8.1, §8.2).
//!
//! A Validation node is an *observing* kind (`owns_snapshot = false`): it runs a
//! configured test command inside a worktree materialized from the target node's
//! snapshot and attaches an append-only [`ResultEnvelope`](spork_runner::ResultEnvelope)
//! to that node **without mutating it** (DESIGN.md §6.2, §8.1). It owns no
//! snapshot, so it never restores state — clicking it overlays its results on the
//! parent's snapshot.
//!
//! # The Runner
//!
//! [`ValidationRunner`] is a [`Runner`](spork_runner::Runner) adapter behind the
//! one F4 SPI (DESIGN.md §8.1, the `TestRunner` row): `prepare` builds the test
//! command against the materialized worktree, `run` executes it (here, offline,
//! the JUnit-XML report it would emit is fed in through the spec config so the
//! parser is exercised end-to-end without a network or a live tool), and
//! `normalize` parses the **JUnit-XML** into per-unit
//! [`UnitResult`](spork_runner::UnitResult)s (pass / fail / skip) on the one
//! envelope. The hard, load-bearing surface is exactly this normalization
//! (DESIGN.md §8.1).
//!
//! The JUnit-XML parser is intentionally dependency-free: it understands the
//! `<testsuite>/<testcase>` shape every junit emitter produces, mapping a
//! `<failure>`/`<error>` child to [`UnitStatus::Failed`](spork_runner::UnitStatus::Failed),
//! a `<skipped>` child to [`UnitStatus::Skipped`](spork_runner::UnitStatus::Skipped),
//! and a bare `<testcase>` to [`UnitStatus::Passed`](spork_runner::UnitStatus::Passed).
//!
//! Design references: DESIGN.md §7.1 (taxonomy), §8.1 (one Runner SPI; junit-xml
//! → per-unit results), §8.2 (execution against a materialized tree; append-only
//! results that never mutate the parent).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spork_exec::{CancelToken, PreparedRun, RawRunOutput};
use spork_graph::EdgeType;
use spork_registry::{Family, NodeTypeDescriptor, StalenessRule};
use spork_runner::{
    ArtifactManifest, Outcome, ResultEnvelope, Runner, RunnerCapabilities, SandboxContext,
    UnitStatus,
};
use spork_runner::{CheckSpec, RunnerError};

use crate::descriptor::type_version;
use crate::junit::parse_junit;

/// The check kind the validation runner handles (selects the runner via the SPI).
pub const VALIDATION_KIND: &str = "validation";

/// The validation runner's version string, folded into the cache key.
pub const VALIDATION_VERSION: &str = "validation@1";

/// The validation runner's derivation-formula generation (DESIGN.md §9.2).
pub const VALIDATION_GENERATION: u32 = 1;

/// The schema version stamped on a freshly built [`ValidationPayload`].
pub const VALIDATION_PAYLOAD_VERSION: u16 = 1;

/// The marker program name a validation [`PreparedRun`] carries.
const VALIDATION_PROGRAM: &str = "spork-validation";

/// The env key a prepared validation run carries its junit-xml report under.
const ENV_JUNIT: &str = "SPORK_VALIDATION_JUNIT_XML";

/// The schema-versioned payload of a Validation node (DESIGN.md §7.1).
///
/// `target_node_id` is the node whose snapshot the check observes; `command` is
/// the configured test command; `input_digest` (filled by the runner) ties the
/// result to exactly what it validated (DESIGN.md §7, §8.2). The payload is the
/// node's *config*; the per-run results live on the append-only
/// [`ResultEnvelope`](spork_runner::ResultEnvelope), not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationPayload {
    /// The schema version of this payload ([`VALIDATION_PAYLOAD_VERSION`]).
    pub schema_version: u16,
    /// The node this validation observes (its snapshot is never mutated).
    pub target_node_id: String,
    /// The configured test command (e.g. `"cargo test"`, `"pytest -q"`).
    pub command: String,
}

impl ValidationPayload {
    /// Construct a validation payload targeting `target_node_id` with `command`.
    #[must_use]
    pub fn new(target_node_id: impl Into<String>, command: impl Into<String>) -> Self {
        ValidationPayload {
            schema_version: VALIDATION_PAYLOAD_VERSION,
            target_node_id: target_node_id.into(),
            command: command.into(),
        }
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

/// The Validation/Test runner — an observing [`Runner`] behind the F4 SPI.
///
/// It parses JUnit-XML into the per-unit results of the one
/// [`ResultEnvelope`](spork_runner::ResultEnvelope) (DESIGN.md §8.1). The test
/// command's stdout/stderr would carry the report from a live tool; offline, the
/// report is supplied through the spec config (`junit_xml`) so the normalization
/// — the load-bearing surface — is exercised deterministically.
#[derive(Debug, Default)]
pub struct ValidationRunner;

impl ValidationRunner {
    /// Construct a validation runner.
    #[must_use]
    pub fn new() -> Self {
        ValidationRunner
    }

    /// Read the configured command from a spec's config, erroring if absent.
    fn command_of(spec: &CheckSpec) -> Result<String, RunnerError> {
        spec.config
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                RunnerError::Config("validation config requires a string `command`".into())
            })
    }
}

impl Runner for ValidationRunner {
    fn describe(&self) -> RunnerCapabilities {
        RunnerCapabilities {
            name: "validation".into(),
            version: VALIDATION_VERSION.into(),
            generation: VALIDATION_GENERATION,
            kinds: vec![VALIDATION_KIND.into()],
            // A test run may need ports/DBs/processes; it is NOT hermetic the way
            // a sanity check is (DESIGN.md §8.2).
            hermetic: false,
        }
    }

    fn prepare(&self, ctx: &SandboxContext, spec: &CheckSpec) -> Result<PreparedRun, RunnerError> {
        if spec.kind != VALIDATION_KIND {
            return Err(RunnerError::UnsupportedKind {
                kind: spec.kind.clone(),
                runner: "validation".into(),
            });
        }
        let command = Self::command_of(spec)?;
        // The test runs inside the materialized worktree, never the live dir
        // (DESIGN.md §8.2). The command is split into program + args by the
        // backend; here we record it as a single program token plus the worktree
        // root, and carry the junit-xml report the run will normalize.
        let junit = spec
            .config
            .get("junit_xml")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let prepared = PreparedRun::new(VALIDATION_PROGRAM, [command])
            .with_env(
                "SPORK_VALIDATION_ROOT",
                ctx.workspace.root.to_string_lossy(),
            )
            .with_env(ENV_JUNIT, junit);
        Ok(prepared)
    }

    fn run(
        &self,
        prepared: PreparedRun,
        signal: &CancelToken,
    ) -> Result<RawRunOutput, RunnerError> {
        if signal.is_cancelled() {
            return Err(RunnerError::Exec("validation run cancelled".into()));
        }
        // A live TestRunner would spawn the command in the worktree and capture
        // the junit-xml it wrote. Offline, the report was supplied to `prepare`
        // and is carried here as the run's stdout — the same bytes `normalize`
        // parses, so the JUnit→units mapping is exercised end-to-end.
        let junit = prepared.env.get(ENV_JUNIT).cloned().unwrap_or_default();
        let report = parse_junit(&junit).map_err(RunnerError::Config)?;
        // A non-zero exit mirrors a failing suite so a downstream consumer of the
        // raw output sees the right shape; `normalize` is authoritative.
        let exit = if report.iter().any(|u| u.status == UnitStatus::Failed) {
            1
        } else {
            0
        };
        Ok(RawRunOutput::new(
            Some(exit),
            junit.into_bytes(),
            Vec::new(),
        ))
    }

    fn normalize(
        &self,
        raw: RawRunOutput,
        spec: &CheckSpec,
    ) -> Result<ResultEnvelope, RunnerError> {
        if spec.kind != VALIDATION_KIND {
            return Err(RunnerError::UnsupportedKind {
                kind: spec.kind.clone(),
                runner: "validation".into(),
            });
        }
        let junit = String::from_utf8_lossy(&raw.stdout);
        let units = parse_junit(&junit).map_err(RunnerError::Config)?;
        // The outcome is the gate-facing summary: any failing unit fails the
        // check; an empty suite that ran cleanly passes (DESIGN.md §8.1).
        let outcome = if units.iter().any(|u| u.status == UnitStatus::Failed) {
            Outcome::Failed
        } else {
            Outcome::Passed
        };
        // The caller (CachingRunner) stamps the real input_digest; a placeholder
        // here keeps the envelope well-formed.
        let mut envelope = ResultEnvelope::new(
            outcome,
            ulid::Ulid::new(),
            spork_hash::Hash::from_bytes([0; 32]),
        );
        for unit in units {
            envelope = envelope.with_unit(unit);
        }
        Ok(envelope)
    }

    fn collect_artifacts(&self, raw: &RawRunOutput) -> ArtifactManifest {
        // A live runner would content-address the junit-xml + coverage as bulky
        // artifacts; offline, the report is small and already in the envelope's
        // units, so the manifest is empty. (Reading `raw` keeps the signature
        // honest about what a live impl would consume.)
        let _ = raw.stdout.len();
        ArtifactManifest::new()
    }
}

/// Build the Validation/Test [`NodeTypeDescriptor`] (DESIGN.md §7.1).
///
/// A [`Family::Observing`] type that owns **no** snapshot (it never restores
/// state) and declares a result schema for its attached per-unit results. It may
/// originate a [`EdgeType::Validates`] edge to the Edit it observes (DESIGN.md
/// §6.3). Its staleness rule is [`StalenessRule::WhenAncestorChanges`]: a result
/// goes stale when the observed snapshot changes (DESIGN.md §7.3).
#[must_use]
pub fn descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: VALIDATION_KIND.to_string(),
        type_version: type_version(),
        family: Family::Observing,
        owns_snapshot: false,
        payload_schema: json!({
            "type": "object",
            "properties": {
                "schema_version": { "type": "integer", "minimum": 1 },
                "target_node_id": { "type": "string" },
                "command": { "type": "string" }
            },
            "required": ["schema_version", "target_node_id", "command"]
        }),
        result_schema: Some(json!({
            "type": "object",
            "description": "ResultEnvelope with per-unit test results (DESIGN §8.1)"
        })),
        allowed_edges: vec![EdgeType::Validates],
        ports: vec![],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec!["snapshot.read".to_string(), "process.spawn".to_string()],
        ui_contributions: json!({ "color": "#3b82f6", "icon": "check-circle", "displayName": "Validation" }),
        revoked_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_registry::NodeTypeRegistry;

    const SAMPLE_JUNIT: &str = r#"<?xml version="1.0"?>
        <testsuite name="suite" tests="3">
            <testcase name="passes"/>
            <testcase name="fails"><failure message="boom">stack</failure></testcase>
            <testcase name="skips"><skipped/></testcase>
        </testsuite>"#;

    fn spec_with(junit: &str) -> CheckSpec {
        CheckSpec::new(
            VALIDATION_KIND,
            json!({ "command": "cargo test", "junit_xml": junit }),
        )
    }

    #[test]
    fn descriptor_is_observing_and_owns_no_snapshot() {
        let d = descriptor();
        assert_eq!(d.id, VALIDATION_KIND);
        assert_eq!(d.family, Family::Observing);
        assert!(!d.owns_snapshot);
        assert!(d.result_schema.is_some());
        // An observing type needs no SnapshotRef out-port; registration accepts it.
        let mut reg = NodeTypeRegistry::new();
        reg.register(d).unwrap();
    }

    #[test]
    fn payload_is_versioned_and_round_trips() {
        let p = ValidationPayload::new("01H...", "cargo test");
        assert_eq!(p.schema_version, VALIDATION_PAYLOAD_VERSION);
        let v = p.to_value().unwrap();
        let back: ValidationPayload = serde_json::from_value(v).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn normalize_maps_junit_to_per_unit_results() {
        let runner = ValidationRunner::new();
        let raw = RawRunOutput::new(Some(1), SAMPLE_JUNIT.as_bytes().to_vec(), Vec::new());
        let env = runner.normalize(raw, &spec_with(SAMPLE_JUNIT)).unwrap();
        assert_eq!(env.outcome, Outcome::Failed); // one failing case
        assert_eq!(env.units.len(), 3);
        let statuses: Vec<UnitStatus> = env.units.iter().map(|u| u.status).collect();
        assert!(statuses.contains(&UnitStatus::Passed));
        assert!(statuses.contains(&UnitStatus::Failed));
        assert!(statuses.contains(&UnitStatus::Skipped));
        // The failure message is carried as the unit's detail.
        let failed = env.units.iter().find(|u| u.name == "fails").unwrap();
        assert_eq!(failed.detail.as_deref(), Some("boom"));
    }

    #[test]
    fn all_passing_suite_passes() {
        let junit = r#"<testsuite><testcase name="a"/><testcase name="b"/></testsuite>"#;
        let runner = ValidationRunner::new();
        let raw = RawRunOutput::new(Some(0), junit.as_bytes().to_vec(), Vec::new());
        let env = runner.normalize(raw, &spec_with(junit)).unwrap();
        assert_eq!(env.outcome, Outcome::Passed);
        assert_eq!(env.units.len(), 2);
    }

    #[test]
    fn prepare_rejects_wrong_kind() {
        let runner = ValidationRunner::new();
        let spec = CheckSpec::new("sanity", json!({ "command": "x" }));
        let raw = RawRunOutput::new(Some(0), Vec::new(), Vec::new());
        assert!(matches!(
            runner.normalize(raw, &spec).unwrap_err(),
            RunnerError::UnsupportedKind { .. }
        ));
    }

    #[test]
    fn describe_declares_non_hermetic_test_runner() {
        let caps = ValidationRunner::new().describe();
        assert!(caps.handles(VALIDATION_KIND));
        assert!(!caps.hermetic);
        assert_eq!(caps.version, VALIDATION_VERSION);
    }
}
