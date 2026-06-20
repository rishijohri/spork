//! Error type for the migration registry.
//!
//! See the crate root for the model. The taxonomy is intentionally small: a
//! migration either cannot be *registered* (a duplicate `(type, from)` key), an
//! upgrade chain cannot be *found* (a gap between a stored `from_version` and the
//! requested current version), or an individual migration step *fails* while
//! transforming a payload.

use thiserror::Error;

/// Errors produced by [`MigrationRegistry`](crate::MigrationRegistry) and
/// [`EventMigration`](crate::EventMigration) implementations.
///
/// `#[non_exhaustive]` so additional, additive variants can be introduced in a
/// later generation without breaking downstream `match` arms (C2 / C3).
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationError {
    /// Two migrations claim the same `(event_type, from_version)` key.
    ///
    /// The registry requires a single, unambiguous step out of every source
    /// version: the upgrade chain must be a function, not a relation. Attempting
    /// to register a second step from the same source version is rejected here
    /// rather than silently overwriting the first (C5: the registry is the only
    /// sanctioned way a schema grows, so its contents must be deterministic).
    #[error("duplicate migration for event_type {event_type:?} from schema version {from}")]
    DuplicateMigration {
        /// The event type whose migration collided.
        event_type: String,
        /// The source schema version with two registered steps.
        from: u16,
    },

    /// No registered chain of migrations reaches `to` from `from`.
    ///
    /// Either a step is missing (e.g. a `v1 -> v2` migration was registered but
    /// the `v2 -> v3` step was forgotten), or a step advanced *past* the target
    /// version without landing on it. Replay must fail loudly: reading an event
    /// at a version that cannot be brought current is a programming error, not a
    /// recoverable condition, because the stored bytes are never edited and the
    /// only path forward is a correctly registered migration.
    #[error("no migration path for event_type {event_type:?} from schema version {from} to {to}")]
    NoPathToVersion {
        /// The event type being upgraded.
        event_type: String,
        /// The version the stored payload was written at.
        from: u16,
        /// The current version the payload must reach.
        to: u16,
    },

    /// An individual [`EventMigration::upgrade`](crate::EventMigration::upgrade)
    /// step rejected its input payload.
    ///
    /// Carries the migration author's human-readable reason (a missing required
    /// field, an out-of-range value, an unconvertible shape, …). Because the
    /// message is author-supplied it is kept as an opaque `String` rather than a
    /// structured cause, so the trait stays simple for the common case.
    #[error("migration step failed: {0}")]
    Upgrade(String),
}
