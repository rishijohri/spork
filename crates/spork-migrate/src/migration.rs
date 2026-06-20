//! The [`EventMigration`] seam.
//!
//! A migration is a single, forward, one-version step that transforms an event
//! payload from one schema version to the next. It is the extension point (C3)
//! through which a schema is allowed to grow: each step is registered once, at
//! the commit that introduces the new version, and the registry composes the
//! steps into a chain at replay time.

use crate::error::MigrationError;
use serde_json::Value;

/// A single forward, one-step schema migration for one event type.
///
/// An implementation upgrades a payload written at [`from_version`] to the shape
/// expected at [`to_version`]. The two versions **must** be adjacent in the
/// upgrade chain — `to_version == from_version + 1` is the canonical case, and
/// [`MigrationRegistry`](crate::MigrationRegistry) walks one step at a time so a
/// `v1 -> v3` jump is expressed as two registered migrations, not one. This keeps
/// every step small, individually testable, and replayable in isolation.
///
/// Migrations never touch stored bytes. They run against an in-memory
/// [`Value`] obtained by deserializing the canonical payload at read time, and
/// their output is fed to the next step (or returned to the caller). Stored
/// events are immutable (C5).
///
/// # Contract
///
/// - [`from_version`] strictly precedes [`to_version`]; the registry validates
///   `from < to` at registration time.
/// - [`upgrade`] is a pure function of its input payload — the same input always
///   yields the same output (or the same error). It must not depend on external
///   state, the wall clock, or randomness, because replay must be deterministic.
/// - [`upgrade`] returns [`MigrationError::Upgrade`] (typically) when the input
///   does not match the shape it expects for [`from_version`].
///
/// [`from_version`]: EventMigration::from_version
/// [`to_version`]: EventMigration::to_version
/// [`upgrade`]: EventMigration::upgrade
pub trait EventMigration {
    /// The event type this migration applies to (matched verbatim against the
    /// stored `event_type`).
    fn event_type(&self) -> &str;

    /// The schema version this migration upgrades *from*.
    ///
    /// (The `from_*` name is a fixed part of the public contract and is a plain
    /// getter, not a constructor — hence the `wrong_self_convention` allow.)
    #[allow(clippy::wrong_self_convention)]
    fn from_version(&self) -> u16;

    /// The schema version this migration upgrades *to* (must be greater than
    /// [`from_version`](EventMigration::from_version)).
    fn to_version(&self) -> u16;

    /// Transform a payload written at
    /// [`from_version`](EventMigration::from_version) into the shape expected at
    /// [`to_version`](EventMigration::to_version).
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::Upgrade`] (or another [`MigrationError`]) if the
    /// payload cannot be upgraded — for example a required field is absent or has
    /// an unexpected type.
    fn upgrade(&self, payload: Value) -> Result<Value, MigrationError>;
}
