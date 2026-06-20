//! The metric-id registry: [`MetricId`], [`Metric`], [`Direction`], and
//! [`MetricRegistry`].
//!
//! Gates compare metrics across nodes and branches by **id** —
//! `metric('p99_latency_ms').deltaVsBaseline <= 0.10` (DESIGN.md §8.3). That id
//! is therefore load-bearing identity: if a runner emits a metric under a fresh
//! name while a gate still keys off the old one, the gate silently stops
//! matching and a regression slips through green. To make that failure
//! *impossible to do silently*, every metric id is funnelled through a single
//! [`MetricRegistry`]. A rename is expressed as a registry entry that lists the
//! new canonical id together with its prior aliases, so the old id keeps
//! resolving to the same metric and a gate written against either name continues
//! to work. An id that is neither a canonical id nor a registered alias is a
//! typed [`RunnerError::UnknownMetric`](crate::RunnerError::UnknownMetric), never
//! a quiet miss.
//!
//! Each metric also carries an optimization [`Direction`] —
//! [`HigherBetter`](Direction::HigherBetter) (coverage, throughput) or
//! [`LowerBetter`](Direction::LowerBetter) (latency, violation count) — so the
//! diff/gate engine knows which way a delta is an improvement without a per-id
//! lookup table baked into the core (DESIGN.md §8.1, the normalized
//! `ResultEnvelope`).
//!
//! Design references: DESIGN.md §8.1 (the load-bearing `ResultEnvelope` with
//! typed `metrics[]`), §8.3 (gates/baselines comparing metric deltas by id),
//! §4.4 (extensibility — new metrics register additively).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Result, RunnerError};

/// The canonical, stable identifier of a metric (e.g. `"p99_latency_ms"`,
/// `"line_coverage_pct"`, `"violation_count"`).
///
/// A newtype over [`String`] so a metric id is never confused with an arbitrary
/// string at a call site. Ids are compared and serialized as their inner text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MetricId(pub String);

impl MetricId {
    /// Construct a metric id from any string-like value.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        MetricId(id.into())
    }

    /// Borrow the id's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MetricId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for MetricId {
    fn from(s: &str) -> Self {
        MetricId(s.to_string())
    }
}

impl From<String> for MetricId {
    fn from(s: String) -> Self {
        MetricId(s)
    }
}

/// Which way a metric improves.
///
/// The diff and gate engines need to know whether a positive delta is good or
/// bad without hard-coding a per-metric table; the direction travels with the
/// metric in the [`ResultEnvelope`](crate::ResultEnvelope) (DESIGN.md §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Larger values are better (coverage, throughput, pass rate).
    HigherBetter,
    /// Smaller values are better (latency, memory, violation count).
    LowerBetter,
}

/// A single typed measurement in a normalized result.
///
/// Carries the metric [`id`](Metric::id) (which must be registered in the
/// [`MetricRegistry`]), its integer [`value`](Metric::value), and the
/// optimization [`direction`](Metric::direction). Values are integers, never
/// floats: identity-bearing data must be byte-stable, so fractional quantities
/// are encoded losslessly (for example a latency in *microseconds* or a coverage
/// in *basis points*) rather than as a non-portable `f64` (DESIGN.md §6.1,
/// matching [`spork_canon`]'s float refusal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metric {
    /// The registered canonical (or alias-resolvable) metric id.
    pub id: MetricId,
    /// The measured value, encoded as an integer with a metric-defined scale.
    pub value: i64,
    /// Which way this metric improves.
    pub direction: Direction,
}

impl Metric {
    /// Construct a metric from an id, value, and direction.
    #[must_use]
    pub fn new(id: impl Into<MetricId>, value: i64, direction: Direction) -> Self {
        Metric {
            id: id.into(),
            value,
            direction,
        }
    }
}

/// A registered metric's descriptor: its canonical id, optimization direction,
/// and any prior aliases that must keep resolving to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricDescriptor {
    /// The canonical, current id of the metric.
    pub canonical: MetricId,
    /// The default optimization direction for this metric.
    pub direction: Direction,
    /// Prior ids that were renamed *into* [`canonical`](MetricDescriptor::canonical).
    ///
    /// A gate written against any of these keeps working after a rename: the
    /// registry resolves the old id to the same descriptor (DESIGN.md §8.3).
    pub aliases: Vec<MetricId>,
}

impl MetricDescriptor {
    /// Construct a descriptor with no aliases.
    #[must_use]
    pub fn new(canonical: impl Into<MetricId>, direction: Direction) -> Self {
        MetricDescriptor {
            canonical: canonical.into(),
            direction,
            aliases: Vec::new(),
        }
    }

    /// Add a prior id that must keep resolving to this metric (builder style).
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<MetricId>) -> Self {
        self.aliases.push(alias.into());
        self
    }
}

/// The single place every metric id is resolved.
///
/// Maps every canonical id *and* every registered alias to the metric's
/// [`MetricDescriptor`]. A rename is contained here: register the new canonical
/// id with the old id as an [`alias`](MetricDescriptor::aliases) and both names
/// resolve to one descriptor, so no gate that keyed off the old id silently
/// breaks (DESIGN.md §8.3). Resolving an id that is neither canonical nor an
/// alias is [`RunnerError::UnknownMetric`](crate::RunnerError::UnknownMetric).
///
/// The registry ships with the built-in metric vocabulary the shipped runners
/// emit ([`MetricRegistry::with_builtins`]); new metrics are added additively.
///
/// # Example
/// ```
/// use spork_runner::{MetricRegistry, MetricDescriptor, Direction};
///
/// // A rename: the old id `"p99_ms"` keeps resolving to the new canonical id.
/// let mut reg = MetricRegistry::new();
/// reg.register(
///     MetricDescriptor::new("p99_latency_ms", Direction::LowerBetter)
///         .with_alias("p99_ms"),
/// );
///
/// // Both the new id and the renamed-from id resolve to the same canonical id.
/// assert_eq!(reg.canonicalize("p99_ms").unwrap().as_str(), "p99_latency_ms");
/// assert_eq!(
///     reg.canonicalize("p99_latency_ms").unwrap().as_str(),
///     "p99_latency_ms"
/// );
///
/// // An unregistered id is a typed error, never a silent miss.
/// assert!(reg.canonicalize("made_up").is_err());
/// ```
#[derive(Debug, Clone, Default)]
pub struct MetricRegistry {
    /// Indexed by canonical id.
    by_canonical: BTreeMap<MetricId, MetricDescriptor>,
    /// Maps every alias (and every canonical id) to its canonical id.
    resolve: BTreeMap<MetricId, MetricId>,
}

/// The metric id the [`SanityRunner`](crate::SanityRunner) emits for the number
/// of violations it found (lower is better).
pub const METRIC_VIOLATION_COUNT: &str = "violation_count";

/// The metric id for the number of files a check scanned (informational; the
/// registry treats more-scanned as "higher coverage" so it is `HigherBetter`).
pub const METRIC_FILES_SCANNED: &str = "files_scanned";

impl MetricRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        MetricRegistry::default()
    }

    /// Construct a registry pre-populated with the built-in metric vocabulary
    /// the shipped runners emit.
    ///
    /// Currently the sanity-runner metrics ([`METRIC_VIOLATION_COUNT`],
    /// [`METRIC_FILES_SCANNED`]); test/stress metrics register additively when
    /// those runners ship (P phases).
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut reg = MetricRegistry::new();
        reg.register(MetricDescriptor::new(
            METRIC_VIOLATION_COUNT,
            Direction::LowerBetter,
        ));
        reg.register(MetricDescriptor::new(
            METRIC_FILES_SCANNED,
            Direction::HigherBetter,
        ));
        reg
    }

    /// Register a metric descriptor, indexing its canonical id and every alias.
    ///
    /// Re-registering the same canonical id replaces the prior descriptor (the
    /// supported way to add an alias to an existing metric). Registering aliases
    /// that collide with another metric's canonical id last-writer-wins on the
    /// alias mapping, which is intentional — a deliberate re-home of an id.
    pub fn register(&mut self, desc: MetricDescriptor) {
        self.resolve
            .insert(desc.canonical.clone(), desc.canonical.clone());
        for alias in &desc.aliases {
            self.resolve.insert(alias.clone(), desc.canonical.clone());
        }
        self.by_canonical.insert(desc.canonical.clone(), desc);
    }

    /// Resolve any id (canonical or alias) to its canonical id.
    ///
    /// # Errors
    /// [`RunnerError::UnknownMetric`](crate::RunnerError::UnknownMetric) if the
    /// id is neither a canonical id nor a registered alias.
    pub fn canonicalize(&self, id: impl Into<MetricId>) -> Result<MetricId> {
        let id = id.into();
        match self.resolve.get(&id).cloned() {
            Some(canonical) => Ok(canonical),
            None => Err(RunnerError::UnknownMetric(id.0)),
        }
    }

    /// Look up the descriptor for any id (canonical or alias).
    ///
    /// # Errors
    /// [`RunnerError::UnknownMetric`](crate::RunnerError::UnknownMetric) if the
    /// id is not registered.
    pub fn descriptor(&self, id: impl Into<MetricId>) -> Result<&MetricDescriptor> {
        let canonical = self.canonicalize(id)?;
        match self.by_canonical.get(&canonical) {
            Some(desc) => Ok(desc),
            None => Err(RunnerError::UnknownMetric(canonical.0)),
        }
    }

    /// Whether an id (canonical or alias) is registered.
    #[must_use]
    pub fn is_registered(&self, id: impl Into<MetricId>) -> bool {
        self.resolve.contains_key(&id.into())
    }

    /// Validate a metric and return a copy with its id canonicalized.
    ///
    /// This is the gate every emitted metric passes through: an unregistered id
    /// is refused, and a metric emitted under an alias is rewritten to its
    /// canonical id so downstream comparison is alias-insensitive.
    ///
    /// # Errors
    /// [`RunnerError::UnknownMetric`](crate::RunnerError::UnknownMetric) if the
    /// metric's id is not registered.
    pub fn canonicalize_metric(&self, metric: &Metric) -> Result<Metric> {
        let id = self.canonicalize(metric.id.clone())?;
        Ok(Metric {
            id,
            value: metric.value,
            direction: metric.direction,
        })
    }

    /// The number of registered canonical metrics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_canonical.len()
    }

    /// Whether the registry has no canonical metrics.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_canonical.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_registered() {
        let reg = MetricRegistry::with_builtins();
        assert!(reg.is_registered(METRIC_VIOLATION_COUNT));
        assert!(reg.is_registered(METRIC_FILES_SCANNED));
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
    }

    #[test]
    fn builtin_directions_are_correct() {
        let reg = MetricRegistry::with_builtins();
        assert_eq!(
            reg.descriptor(METRIC_VIOLATION_COUNT).unwrap().direction,
            Direction::LowerBetter
        );
        assert_eq!(
            reg.descriptor(METRIC_FILES_SCANNED).unwrap().direction,
            Direction::HigherBetter
        );
    }

    #[test]
    fn rename_is_contained_by_alias() {
        // The crux test: a metric-id rename must not silently break a gate that
        // keys off the old id (DESIGN.md §8.3).
        let mut reg = MetricRegistry::new();
        reg.register(
            MetricDescriptor::new("p99_latency_ms", Direction::LowerBetter).with_alias("p99_ms"),
        );

        // The old id still resolves — a gate written against `p99_ms` keeps
        // working.
        assert_eq!(
            reg.canonicalize("p99_ms").unwrap().as_str(),
            "p99_latency_ms"
        );
        assert_eq!(
            reg.canonicalize("p99_latency_ms").unwrap().as_str(),
            "p99_latency_ms"
        );
        // Both resolve to the *same* descriptor.
        assert_eq!(
            reg.descriptor("p99_ms").unwrap(),
            reg.descriptor("p99_latency_ms").unwrap()
        );
    }

    #[test]
    fn unknown_id_is_typed_error() {
        let reg = MetricRegistry::with_builtins();
        let err = reg.canonicalize("not_a_real_metric").unwrap_err();
        assert!(matches!(err, RunnerError::UnknownMetric(_)));
    }

    #[test]
    fn canonicalize_metric_rewrites_alias() {
        let mut reg = MetricRegistry::new();
        reg.register(
            MetricDescriptor::new("p99_latency_ms", Direction::LowerBetter).with_alias("p99_ms"),
        );
        let emitted = Metric::new("p99_ms", 1200, Direction::LowerBetter);
        let canon = reg.canonicalize_metric(&emitted).unwrap();
        assert_eq!(canon.id.as_str(), "p99_latency_ms");
        assert_eq!(canon.value, 1200);
    }

    #[test]
    fn canonicalize_metric_refuses_unknown() {
        let reg = MetricRegistry::with_builtins();
        let emitted = Metric::new("bogus", 1, Direction::LowerBetter);
        assert!(reg.canonicalize_metric(&emitted).is_err());
    }

    #[test]
    fn metric_id_round_trips_through_serde_transparently() {
        let id = MetricId::new("p99_latency_ms");
        let json = serde_json::to_string(&id).unwrap();
        // `transparent` => serializes as a bare string, not a wrapper object.
        assert_eq!(json, "\"p99_latency_ms\"");
        let back: MetricId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn metric_round_trips_through_serde() {
        let m = Metric::new("violation_count", 3, Direction::LowerBetter);
        let json = serde_json::to_string(&m).unwrap();
        let back: Metric = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
