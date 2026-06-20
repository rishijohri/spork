//! The v1 runner: [`SanityRunner`] — a deterministic pattern / lint check.
//!
//! This is the single F4 implementation of the [`Runner`](crate::Runner) SPI: a
//! **Deterministic Sanity / Pattern-Check** runner (DESIGN.md §8.1, the
//! `SanityRunner` row). It is hermetic by contract — it reads the materialized
//! CoW worktree and nothing else (no network, read-only base) — and fully
//! deterministic, so its results are cacheable replayable derivations (DESIGN.md
//! §8.2, §9.2).
//!
//! ## What it checks
//!
//! Two configurable, deterministic rules, driven by the
//! [`CheckSpec::config`](crate::CheckSpec::config) JSON:
//!
//! - **`forbid`**: a list of literal substrings that must not appear in file
//!   contents (e.g. `["FIXME", "XXX"]`). Each occurrence emits a
//!   [`Violation`](crate::Violation) with `rule_id =
//!   "forbid-pattern:<pattern>"`, the file, and the 1-based line.
//! - **`max_line_length`**: an integer maximum line length (in characters).
//!   Each over-long line emits a `max-line-length` violation.
//!
//! Two optional glob lists scope which files are scanned:
//!
//! - **`include`**: if present, only files matching one of these globs are
//!   scanned (default: all files).
//! - **`exclude`**: files matching one of these globs are skipped.
//!
//! ## Change-scoping
//!
//! When the [`SandboxContext`](crate::SandboxContext) carries
//! [`changed_paths`](crate::SandboxContext::changed_paths), only those files are
//! scanned — the change-scoped auto-run of DESIGN.md §8.2
//! (`onEditNodeCommitted` schedules sanity specs whose declared inputs intersect
//! the changed paths). With no change scope it scans the whole worktree.
//!
//! ## How it maps onto the SPI
//!
//! `prepare` validates the config and packs the scan parameters (worktree root,
//! input-tree hash, change scope) into a marker [`PreparedRun`](spork_exec::PreparedRun);
//! `run` performs the deterministic scan and serializes its findings into the
//! [`RawRunOutput`](spork_exec::RawRunOutput) stdout; `normalize` turns those
//! findings into the one [`ResultEnvelope`](crate::ResultEnvelope), validating
//! every metric id against the registry. The work is in-process (no child
//! process is spawned) but flows through the same SPI as a tool-shelling runner.
//!
//! Design references: DESIGN.md §8.1 (the `SanityRunner` behind the one Runner
//! SPI), §8.2 (hermetic, change-scoped execution against a materialized tree),
//! §9.2 (deterministic => cacheable replayable derivation), §4.4 (`inputDigest`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use spork_exec::{CancelToken, PreparedRun, RawRunOutput};
use spork_hash::Hash;

use crate::envelope::{ArtifactManifest, Outcome, ResultEnvelope, Violation};
use crate::error::{Result, RunnerError};
use crate::metric::{
    Direction, Metric, MetricRegistry, METRIC_FILES_SCANNED, METRIC_VIOLATION_COUNT,
};
use crate::runner::{Runner, RunnerCapabilities, SandboxContext};
use crate::spec::CheckSpec;

/// The check kind the sanity runner handles.
pub const SANITY_KIND: &str = "sanity";

/// The sanity runner's version string, folded into the cache key.
pub const SANITY_VERSION: &str = "sanity@1";

/// The sanity runner's derivation-formula generation. Bump on a formula change
/// to open a fresh cache generation (DESIGN.md §9.2).
pub const SANITY_GENERATION: u32 = 1;

/// The marker program name a sanity [`PreparedRun`] carries (it is never spawned
/// — the scan is in-process — but the SPI's `PreparedRun` still names it).
const SANITY_PROGRAM: &str = "spork-sanity";

/// The deterministic pattern / lint runner — the one F4 [`Runner`].
///
/// Carries a [`MetricRegistry`] so every metric it emits is validated against
/// the registry: a metric id the registry does not know is a typed
/// [`RunnerError::UnknownMetric`](crate::RunnerError::UnknownMetric), never a
/// silent gate break (DESIGN.md §8.3). Construct with
/// [`SanityRunner::new`] (built-in registry) or
/// [`SanityRunner::with_registry`] (a custom one, e.g. carrying renamed-metric
/// aliases).
#[derive(Debug)]
pub struct SanityRunner {
    registry: MetricRegistry,
}

impl Default for SanityRunner {
    fn default() -> Self {
        SanityRunner::new()
    }
}

impl SanityRunner {
    /// Construct a sanity runner with the built-in metric registry.
    #[must_use]
    pub fn new() -> Self {
        SanityRunner {
            registry: MetricRegistry::with_builtins(),
        }
    }

    /// Construct a sanity runner with an explicit metric registry.
    ///
    /// The registry must know [`METRIC_VIOLATION_COUNT`] and
    /// [`METRIC_FILES_SCANNED`] (or aliases of them); the built-in registry
    /// does. This is how a deployment that renamed a metric supplies the
    /// alias-carrying registry.
    #[must_use]
    pub fn with_registry(registry: MetricRegistry) -> Self {
        SanityRunner { registry }
    }

    /// Borrow the runner's metric registry.
    #[must_use]
    pub fn registry(&self) -> &MetricRegistry {
        &self.registry
    }
}

/// The parsed, validated sanity-check configuration.
///
/// Built from a [`CheckSpec::config`](crate::CheckSpec::config) by
/// [`SanityConfig::parse`], which rejects malformed configs loudly rather than
/// silently producing a wrong result (DESIGN.md §8.1, cache-key soundness
/// depends on an honest config).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SanityConfig {
    /// Literal substrings forbidden in file contents.
    forbid: Vec<String>,
    /// Optional maximum line length in characters.
    max_line_length: Option<u32>,
    /// Optional include globs (if non-empty, only matching files are scanned).
    include: Vec<String>,
    /// Optional exclude globs (matching files are skipped).
    exclude: Vec<String>,
}

impl SanityConfig {
    /// Parse and validate a sanity config from its JSON payload.
    fn parse(config: &Value) -> Result<Self> {
        let obj = match config {
            Value::Object(map) => map,
            Value::Null => {
                // An empty config is valid: a no-op check that finds nothing.
                return Ok(SanityConfig {
                    forbid: Vec::new(),
                    max_line_length: None,
                    include: Vec::new(),
                    exclude: Vec::new(),
                });
            }
            other => {
                return Err(RunnerError::config(format!(
                    "sanity config must be a JSON object, got {}",
                    json_type(other)
                )))
            }
        };

        let forbid = parse_string_list(obj.get("forbid"), "forbid")?;
        let include = parse_string_list(obj.get("include"), "include")?;
        let exclude = parse_string_list(obj.get("exclude"), "exclude")?;

        let max_line_length = match obj.get("max_line_length") {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) => {
                let v = n.as_u64().ok_or_else(|| {
                    RunnerError::config("`max_line_length` must be a non-negative integer")
                })?;
                if v == 0 {
                    return Err(RunnerError::config(
                        "`max_line_length` must be greater than zero",
                    ));
                }
                Some(u32::try_from(v).map_err(|_| {
                    RunnerError::config("`max_line_length` is too large (must fit in u32)")
                })?)
            }
            Some(other) => {
                return Err(RunnerError::config(format!(
                    "`max_line_length` must be an integer, got {}",
                    json_type(other)
                )))
            }
        };

        Ok(SanityConfig {
            forbid,
            max_line_length,
            include,
            exclude,
        })
    }

    /// Whether this config scans for anything at all.
    fn is_noop(&self) -> bool {
        self.forbid.is_empty() && self.max_line_length.is_none()
    }

    /// Build the include glob set, or `None` if no include globs were configured
    /// (meaning "match everything").
    fn include_set(&self) -> Result<Option<GlobSet>> {
        if self.include.is_empty() {
            Ok(None)
        } else {
            Ok(Some(build_glob_set(&self.include)?))
        }
    }

    /// Build the exclude glob set (empty set if none configured).
    fn exclude_set(&self) -> Result<GlobSet> {
        build_glob_set(&self.exclude)
    }
}

/// The serialized intermediate the sanity scan produces in `run` and `normalize`
/// consumes. Carries the input-tree hash so the envelope's `input_digest` is the
/// honestly-declared input the scan read (DESIGN.md §4.4, §8.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SanityFindings {
    /// The content hash of the input tree the scan read.
    input_tree: Hash,
    /// The number of files scanned.
    files_scanned: u64,
    /// The violations found, in a deterministic (sorted) order.
    violations: Vec<Violation>,
}

impl Runner for SanityRunner {
    fn describe(&self) -> RunnerCapabilities {
        RunnerCapabilities {
            name: "sanity".into(),
            version: SANITY_VERSION.into(),
            generation: SANITY_GENERATION,
            kinds: vec![SANITY_KIND.into()],
            hermetic: true,
        }
    }

    fn prepare(&self, ctx: &SandboxContext, spec: &CheckSpec) -> Result<PreparedRun> {
        if spec.kind != SANITY_KIND {
            return Err(RunnerError::UnsupportedKind {
                kind: spec.kind.clone(),
                runner: "sanity".into(),
            });
        }
        // Validate the config now so a malformed check fails at prepare time.
        let _ = SanityConfig::parse(&spec.config)?;

        // Pack the scan parameters into a marker PreparedRun. The runner reads
        // the worktree in-process during `run`; the program is never spawned.
        let changed = serde_json::to_string(&ctx.changed_paths)?;
        let prepared = PreparedRun::new(SANITY_PROGRAM, [])
            .with_env("SPORK_SANITY_ROOT", ctx.workspace.root.to_string_lossy())
            .with_env("SPORK_SANITY_INPUT_TREE", ctx.input_tree.to_hex())
            .with_env("SPORK_SANITY_CHANGED", changed)
            .with_env("SPORK_SANITY_CONFIG", spec.config.to_string());
        Ok(prepared)
    }

    fn run(&self, prepared: PreparedRun, signal: &CancelToken) -> Result<RawRunOutput> {
        if signal.is_cancelled() {
            return Err(RunnerError::Exec("sanity scan cancelled".into()));
        }
        let root = prepared
            .env
            .get("SPORK_SANITY_ROOT")
            .ok_or_else(|| RunnerError::config("prepared run missing SPORK_SANITY_ROOT"))?;
        let input_tree_hex = prepared
            .env
            .get("SPORK_SANITY_INPUT_TREE")
            .ok_or_else(|| RunnerError::config("prepared run missing SPORK_SANITY_INPUT_TREE"))?;
        let input_tree = Hash::from_hex(input_tree_hex)
            .map_err(|e| RunnerError::config(format!("bad input-tree hash: {e}")))?;
        let changed: Vec<String> = prepared
            .env
            .get("SPORK_SANITY_CHANGED")
            .map(|s| serde_json::from_str(s))
            .transpose()?
            .unwrap_or_default();
        let config_value: Value = prepared
            .env
            .get("SPORK_SANITY_CONFIG")
            .map(|s| serde_json::from_str(s))
            .transpose()?
            .unwrap_or(Value::Null);
        let config = SanityConfig::parse(&config_value)?;

        let findings = scan(Path::new(root), &input_tree, &changed, &config, signal)?;
        let stdout = serde_json::to_vec(&findings)?;
        Ok(RawRunOutput::new(
            Some(if findings.violations.is_empty() { 0 } else { 1 }),
            stdout,
            Vec::new(),
        ))
    }

    fn normalize(&self, raw: RawRunOutput, spec: &CheckSpec) -> Result<ResultEnvelope> {
        if spec.kind != SANITY_KIND {
            return Err(RunnerError::UnsupportedKind {
                kind: spec.kind.clone(),
                runner: "sanity".into(),
            });
        }
        let findings: SanityFindings = serde_json::from_slice(&raw.stdout).map_err(|e| {
            RunnerError::Serialize(format!("sanity findings were not valid JSON: {e}"))
        })?;

        let outcome = if findings.violations.is_empty() {
            Outcome::Passed
        } else {
            Outcome::Failed
        };

        // Every metric is validated through the registry: an unknown id is a
        // typed error, and an aliased id is rewritten to its canonical form.
        let violation_metric = self.registry.canonicalize_metric(&Metric::new(
            METRIC_VIOLATION_COUNT,
            i64::try_from(findings.violations.len()).unwrap_or(i64::MAX),
            Direction::LowerBetter,
        ))?;
        let files_metric = self.registry.canonicalize_metric(&Metric::new(
            METRIC_FILES_SCANNED,
            i64::try_from(findings.files_scanned).unwrap_or(i64::MAX),
            Direction::HigherBetter,
        ))?;

        let mut envelope = ResultEnvelope::new(outcome, ulid::Ulid::new(), findings.input_tree)
            .with_metric(violation_metric)
            .with_metric(files_metric);
        for v in findings.violations {
            envelope = envelope.with_violation(v);
        }
        Ok(envelope)
    }

    fn collect_artifacts(&self, _raw: &RawRunOutput) -> ArtifactManifest {
        // A hermetic sanity check produces no bulky artifacts — its findings are
        // the small queryable envelope. An empty manifest is correct, not a stub.
        ArtifactManifest::new()
    }
}

/// Build a [`GlobSet`] from a list of patterns, reporting a bad pattern as a
/// config error.
fn build_glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob =
            Glob::new(p).map_err(|e| RunnerError::config(format!("invalid glob {p:?}: {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| RunnerError::config(format!("failed to build glob set: {e}")))
}

/// Parse an optional JSON array-of-strings field, erroring on the wrong shape.
fn parse_string_list(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    other => {
                        return Err(RunnerError::config(format!(
                            "`{field}` entries must be strings, got {}",
                            json_type(other)
                        )))
                    }
                }
            }
            Ok(out)
        }
        Some(other) => Err(RunnerError::config(format!(
            "`{field}` must be an array of strings, got {}",
            json_type(other)
        ))),
    }
}

/// A short label for a JSON value's type, for error messages.
fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Perform the deterministic worktree scan and collect findings.
///
/// Files are enumerated deterministically (sorted), scoped to `changed` when
/// non-empty, and filtered by the include/exclude globs. The result is fully
/// determined by the worktree bytes and the config, so re-running it yields a
/// byte-identical findings set — the property the cache relies on.
fn scan(
    root: &Path,
    input_tree: &Hash,
    changed: &[String],
    config: &SanityConfig,
    signal: &CancelToken,
) -> Result<SanityFindings> {
    if config.is_noop() {
        return Ok(SanityFindings {
            input_tree: *input_tree,
            files_scanned: 0,
            violations: Vec::new(),
        });
    }

    let include = config.include_set()?;
    let exclude = config.exclude_set()?;

    // Determine the candidate file set: the change scope if given, else a full
    // deterministic walk of the worktree.
    let candidates: Vec<PathBuf> = if changed.is_empty() {
        let mut all = Vec::new();
        walk(root, root, &mut all, signal)?;
        all
    } else {
        // Deduplicate and sort the declared change scope for determinism.
        let set: BTreeSet<String> = changed.iter().cloned().collect();
        set.into_iter().map(PathBuf::from).collect()
    };

    let mut violations = Vec::new();
    let mut files_scanned = 0u64;

    for rel in candidates {
        if signal.is_cancelled() {
            return Err(RunnerError::Exec("sanity scan cancelled".into()));
        }
        // Apply include/exclude globs against the relative path.
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if let Some(set) = &include {
            if !set.is_match(&rel_str) {
                continue;
            }
        }
        if exclude.is_match(&rel_str) {
            continue;
        }

        let abs = root.join(&rel);
        // Skip non-files (a changed path may name a deleted file or a dir).
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let bytes = std::fs::read(&abs).map_err(|e| RunnerError::io(abs.display(), e))?;
        // Only scan valid UTF-8 text; binary files are skipped (a sanity check
        // is about source text, and a non-UTF-8 file has no meaningful "lines").
        let text = match std::str::from_utf8(&bytes) {
            Ok(t) => t,
            Err(_) => {
                files_scanned += 1;
                continue;
            }
        };
        files_scanned += 1;
        scan_text(&rel_str, text, config, &mut violations);
    }

    // Deterministic ordering so the findings (and thus the envelope) are
    // byte-identical across runs and machines.
    violations.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.rule_id.cmp(&b.rule_id))
    });

    Ok(SanityFindings {
        input_tree: *input_tree,
        files_scanned,
        violations,
    })
}

/// Apply every rule to one file's text, appending violations.
fn scan_text(file: &str, text: &str, config: &SanityConfig, out: &mut Vec<Violation>) {
    for (idx, line) in text.lines().enumerate() {
        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        for pattern in &config.forbid {
            if !pattern.is_empty() && line.contains(pattern.as_str()) {
                out.push(Violation::new(
                    format!("forbid-pattern:{pattern}"),
                    file,
                    line_no,
                    false,
                ));
            }
        }
        if let Some(max) = config.max_line_length {
            let len = line.chars().count();
            if len > max as usize {
                out.push(Violation::new("max-line-length", file, line_no, false));
            }
        }
    }
}

/// Recursively collect regular files under `dir`, as paths relative to `base`,
/// skipping the usual non-source directories so a worktree scan never drags in
/// VCS metadata or vendored deps.
fn walk(base: &Path, dir: &Path, out: &mut Vec<PathBuf>, signal: &CancelToken) -> Result<()> {
    if signal.is_cancelled() {
        return Err(RunnerError::Exec("sanity scan cancelled".into()));
    }
    let entries = std::fs::read_dir(dir).map_err(|e| RunnerError::io(dir.display(), e))?;
    // Collect and sort entries for a deterministic traversal order.
    let mut names: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| RunnerError::io(dir.display(), e))?;
        names.push(entry.path());
    }
    names.sort();
    for path in names {
        let meta =
            std::fs::symlink_metadata(&path).map_err(|e| RunnerError::io(path.display(), e))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if meta.is_dir() {
            if is_skipped_dir(&name) {
                continue;
            }
            walk(base, &path, out, signal)?;
        } else if meta.is_file() {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_path_buf());
            }
        }
        // Symlinks are intentionally not followed (hermetic; avoid escaping the
        // worktree).
    }
    Ok(())
}

/// Whether a directory name should be skipped during a full worktree walk.
fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".hg" | ".svn" | "node_modules" | "target" | ".spork"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_parses_all_fields() {
        let cfg = SanityConfig::parse(&json!({
            "forbid": ["FIXME", "XXX"],
            "max_line_length": 100,
            "include": ["**/*.rs"],
            "exclude": ["**/generated.rs"],
        }))
        .unwrap();
        assert_eq!(cfg.forbid, vec!["FIXME".to_string(), "XXX".to_string()]);
        assert_eq!(cfg.max_line_length, Some(100));
        assert_eq!(cfg.include, vec!["**/*.rs".to_string()]);
        assert_eq!(cfg.exclude, vec!["**/generated.rs".to_string()]);
        assert!(!cfg.is_noop());
    }

    #[test]
    fn empty_config_is_noop() {
        assert!(SanityConfig::parse(&json!({})).unwrap().is_noop());
        assert!(SanityConfig::parse(&Value::Null).unwrap().is_noop());
    }

    #[test]
    fn config_rejects_zero_max_line_length() {
        let err = SanityConfig::parse(&json!({ "max_line_length": 0 })).unwrap_err();
        assert!(matches!(err, RunnerError::Config(_)));
    }

    #[test]
    fn config_rejects_non_object() {
        assert!(SanityConfig::parse(&json!(["forbid"])).is_err());
        assert!(SanityConfig::parse(&json!("nope")).is_err());
    }

    #[test]
    fn config_rejects_non_string_forbid() {
        assert!(SanityConfig::parse(&json!({ "forbid": [1, 2] })).is_err());
    }

    #[test]
    fn config_rejects_bad_max_line_length_type() {
        assert!(SanityConfig::parse(&json!({ "max_line_length": "100" })).is_err());
    }

    #[test]
    fn scan_text_finds_forbidden_patterns() {
        let cfg = SanityConfig {
            forbid: vec!["FIXME".to_string()],
            max_line_length: None,
            include: Vec::new(),
            exclude: Vec::new(),
        };
        let mut out = Vec::new();
        scan_text("a.rs", "ok\n// FIXME later\nok\n", &cfg, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule_id, "forbid-pattern:FIXME");
        assert_eq!(out[0].line, Some(2));
        assert!(!out[0].fixable);
    }

    #[test]
    fn scan_text_finds_long_lines() {
        let cfg = SanityConfig {
            forbid: Vec::new(),
            max_line_length: Some(5),
            include: Vec::new(),
            exclude: Vec::new(),
        };
        let mut out = Vec::new();
        scan_text("a.rs", "short\nthis is too long\n", &cfg, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rule_id, "max-line-length");
        assert_eq!(out[0].line, Some(2));
    }

    #[test]
    fn scan_text_counts_chars_not_bytes() {
        // A line of 5 multi-byte chars is length 5, not 5*bytes.
        let cfg = SanityConfig {
            forbid: Vec::new(),
            max_line_length: Some(5),
            include: Vec::new(),
            exclude: Vec::new(),
        };
        let mut out = Vec::new();
        scan_text("a.rs", "café!\n", &cfg, &mut out); // 5 chars
        assert!(out.is_empty(), "5 chars must not exceed a max of 5");
    }

    #[test]
    fn describe_is_stable() {
        let caps = SanityRunner::new().describe();
        assert_eq!(caps.name, "sanity");
        assert_eq!(caps.version, SANITY_VERSION);
        assert_eq!(caps.generation, SANITY_GENERATION);
        assert!(caps.handles(SANITY_KIND));
        assert!(caps.hermetic);
    }

    #[test]
    fn prepare_rejects_wrong_kind() {
        // We only need the spec branch; construct a minimal context via a temp
        // worktree in the integration tests. Here just exercise the kind guard.
        let runner = SanityRunner::new();
        let spec = CheckSpec::new("test", json!({}));
        // `prepare` needs a context; build one cheaply from a fake workspace is
        // avoided — instead `normalize` shares the same guard and is testable
        // without a workspace.
        let raw = RawRunOutput::new(Some(0), b"{}".to_vec(), Vec::new());
        let err = runner.normalize(raw, &spec).unwrap_err();
        assert!(matches!(err, RunnerError::UnsupportedKind { .. }));
    }

    #[test]
    fn normalize_unknown_metric_is_typed_error() {
        // A registry missing the violation-count metric makes normalize refuse,
        // proving the metric-id registry contains a rename/removal (no silent
        // gate break).
        let runner = SanityRunner::with_registry(MetricRegistry::new()); // empty
        let findings = SanityFindings {
            input_tree: Hash::from_bytes([0; 32]),
            files_scanned: 1,
            violations: Vec::new(),
        };
        let raw = RawRunOutput::new(Some(0), serde_json::to_vec(&findings).unwrap(), Vec::new());
        let spec = CheckSpec::new(SANITY_KIND, json!({}));
        let err = runner.normalize(raw, &spec).unwrap_err();
        assert!(matches!(err, RunnerError::UnknownMetric(_)));
    }

    #[test]
    fn normalize_builds_passed_envelope_with_metrics() {
        let runner = SanityRunner::new();
        let findings = SanityFindings {
            input_tree: Hash::from_bytes([3; 32]),
            files_scanned: 4,
            violations: Vec::new(),
        };
        let raw = RawRunOutput::new(Some(0), serde_json::to_vec(&findings).unwrap(), Vec::new());
        let spec = CheckSpec::new(SANITY_KIND, json!({ "forbid": ["FIXME"] }));
        let env = runner.normalize(raw, &spec).unwrap();
        assert_eq!(env.outcome, Outcome::Passed);
        assert_eq!(env.input_digest, Hash::from_bytes([3; 32]));
        assert_eq!(env.metrics.len(), 2);
        let files = env
            .metrics
            .iter()
            .find(|m| m.id.as_str() == METRIC_FILES_SCANNED)
            .unwrap();
        assert_eq!(files.value, 4);
    }

    #[test]
    fn normalize_builds_failed_envelope_from_violations() {
        let runner = SanityRunner::new();
        let findings = SanityFindings {
            input_tree: Hash::from_bytes([3; 32]),
            files_scanned: 1,
            violations: vec![Violation::new("forbid-pattern:FIXME", "a.rs", 2, false)],
        };
        let raw = RawRunOutput::new(Some(1), serde_json::to_vec(&findings).unwrap(), Vec::new());
        let spec = CheckSpec::new(SANITY_KIND, json!({ "forbid": ["FIXME"] }));
        let env = runner.normalize(raw, &spec).unwrap();
        assert_eq!(env.outcome, Outcome::Failed);
        assert_eq!(env.violations.len(), 1);
        let vc = env
            .metrics
            .iter()
            .find(|m| m.id.as_str() == METRIC_VIOLATION_COUNT)
            .unwrap();
        assert_eq!(vc.value, 1);
    }

    #[test]
    fn collect_artifacts_is_empty_for_hermetic_check() {
        let runner = SanityRunner::new();
        let raw = RawRunOutput::new(Some(0), Vec::new(), Vec::new());
        assert!(runner.collect_artifacts(&raw).is_empty());
    }
}
