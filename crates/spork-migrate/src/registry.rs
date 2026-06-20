//! The [`MigrationRegistry`]: a per-`(event_type, from_version)` index of
//! forward migrations, composed at replay time.
//!
//! The registry is a lookup table keyed by `(event_type, from_version)`. Each
//! key maps to exactly one migration step. To bring a payload from version `f`
//! to the current version `c`, the registry repeatedly looks up the step out of
//! the version it is currently holding, applies it, and advances — `f -> f+1 ->
//! ... -> c` — failing loudly if any step is missing or if a step overshoots the
//! target. This is the "lazy upgrade-on-read" mechanism of DESIGN.md §6.1 / §7.2:
//! stored bytes are never edited; the upgrade happens on the in-memory payload as
//! it is read back.

use crate::error::MigrationError;
use crate::migration::EventMigration;
use std::collections::HashMap;

/// A forward-migration registry, keyed by `(event_type, from_version)`.
///
/// Built once at startup by [`register`](MigrationRegistry::register)ing every
/// known migration, then consulted (read-only) on every event read via
/// [`upgrade_to_current`](MigrationRegistry::upgrade_to_current). Registration
/// rejects an ambiguous schema history — two steps out of the same source
/// version — so the composed chain is always a deterministic function.
///
/// # Example
///
/// ```
/// use serde_json::{json, Value};
/// use spork_migrate::{EventMigration, MigrationError, MigrationRegistry};
///
/// struct AddField; // v1 -> v2: introduce a defaulted `enabled` flag.
/// impl EventMigration for AddField {
///     fn event_type(&self) -> &str { "feature.toggled" }
///     fn from_version(&self) -> u16 { 1 }
///     fn to_version(&self) -> u16 { 2 }
///     fn upgrade(&self, mut p: Value) -> Result<Value, MigrationError> {
///         p.as_object_mut()
///             .ok_or_else(|| MigrationError::Upgrade("expected object".into()))?
///             .insert("enabled".into(), json!(true));
///         Ok(p)
///     }
/// }
///
/// let mut reg = MigrationRegistry::new();
/// reg.register(Box::new(AddField)).unwrap();
///
/// let (upgraded, version) =
///     reg.upgrade_to_current("feature.toggled", 1, 2, json!({"name": "x"})).unwrap();
/// assert_eq!(version, 2);
/// assert_eq!(upgraded, json!({"name": "x", "enabled": true}));
/// ```
#[derive(Default)]
pub struct MigrationRegistry {
    /// `(event_type, from_version) -> step`. The invariant enforced by
    /// [`register`](MigrationRegistry::register) is that each key holds at most
    /// one migration whose `from_version` equals the key's version.
    steps: HashMap<(String, u16), Box<dyn EventMigration>>,
}

impl MigrationRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            steps: HashMap::new(),
        }
    }

    /// Register a single forward migration step.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::DuplicateMigration`] if a step with the same
    ///   `(event_type, from_version)` is already registered. The registry never
    ///   silently overwrites an existing step — an ambiguous schema history is a
    ///   bug to be surfaced at startup, not at replay.
    /// - [`MigrationError::Upgrade`] if the migration is internally inconsistent,
    ///   i.e. its `from_version` is not strictly less than its `to_version`. A
    ///   non-forward or zero-width step can never make progress and would loop the
    ///   composer, so it is rejected eagerly.
    pub fn register(&mut self, m: Box<dyn EventMigration>) -> Result<(), MigrationError> {
        let event_type = m.event_type().to_owned();
        let from = m.from_version();
        let to = m.to_version();

        if from >= to {
            return Err(MigrationError::Upgrade(format!(
                "migration for event_type {event_type:?} is not forward: from {from} >= to {to}"
            )));
        }

        let key = (event_type.clone(), from);
        if self.steps.contains_key(&key) {
            return Err(MigrationError::DuplicateMigration { event_type, from });
        }
        self.steps.insert(key, m);
        Ok(())
    }

    /// Apply the registered chain to bring `payload` from `from_version` up to
    /// `current_version`, one step at a time.
    ///
    /// Returns the upgraded payload paired with the version it now conforms to
    /// (always `current_version` on success). If the payload is already at (or
    /// somehow ahead of) the current version this is a no-op and the payload is
    /// returned untouched — reading an already-current event costs nothing.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::NoPathToVersion`] if a step out of the version the
    ///   composer is currently holding is missing, or if a step lands *past*
    ///   `current_version` without ever hitting it (a malformed chain).
    /// - Whatever a step's [`EventMigration::upgrade`] returns (typically
    ///   [`MigrationError::Upgrade`]) if a step rejects its input.
    pub fn upgrade_to_current(
        &self,
        event_type: &str,
        from_version: u16,
        current_version: u16,
        payload: serde_json::Value,
    ) -> Result<(serde_json::Value, u16), MigrationError> {
        // Already current (or ahead): nothing to do. Treat "ahead" as a no-op
        // rather than an error so a reader pinned to an older `current_version`
        // never corrupts a newer stored event by trying to "downgrade" it.
        if from_version >= current_version {
            return Ok((payload, from_version));
        }

        let mut version = from_version;
        let mut value = payload;

        while version < current_version {
            let key = (event_type.to_owned(), version);
            let step = self
                .steps
                .get(&key)
                .ok_or_else(|| MigrationError::NoPathToVersion {
                    event_type: event_type.to_owned(),
                    from: from_version,
                    to: current_version,
                })?;

            let next = step.to_version();
            // A step that overshoots the target leaves no way to land exactly on
            // `current_version`: there is, by definition, no step registered out
            // of the versions it skipped. Surface that as a missing path.
            if next > current_version {
                return Err(MigrationError::NoPathToVersion {
                    event_type: event_type.to_owned(),
                    from: from_version,
                    to: current_version,
                });
            }

            value = step.upgrade(value)?;
            version = next;
        }

        Ok((value, version))
    }

    /// Number of registered migration steps across all event types.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether no migrations are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}
