//! Spork forward-migration registry — the C5 schema-evolution mechanism.
//!
//! Persisted event payloads carry a `schema_version`. As the system grows, a
//! payload's shape changes, but stored bytes are **never** edited (the event
//! log is append-only and hash-chained). Instead, an older payload is upgraded
//! to the current schema *at read/replay time* by applying a chain of
//! registered forward migrations — "lazy upgrade-on-read". This crate owns that
//! registry: migrations are keyed by `(event_type, from_version)`, registered
//! once per first commit of a schema, and composed step by step to bring any
//! older payload up to the current version. It is the *only* sanctioned way an
//! event schema is allowed to grow.
//!
//! This mirrors the project's evolution-safety rule (CLAUDE.md C5) and the
//! event/projection model described in DESIGN.md §6.1 ("The two layers" — every
//! event carries a `schema_version` and the graph is a replayable projection),
//! with the per-`(type, version)` registry applied at replay per DESIGN.md §7.2
//! ("payload schemas evolve independently of the envelope, so every node carries
//! `payloadSchemaVersion`"). Migrations transform an in-memory
//! [`serde_json::Value`]; they never rewrite the canonical bytes on disk.
//!
//! # Model
//!
//! - An [`EventMigration`] is one **forward, one-version step** for one event
//!   type (`from_version -> to_version`, adjacent). It is the C3 extension seam:
//!   growing a schema means registering a new step, never editing the registry's
//!   composition logic.
//! - A [`MigrationRegistry`] indexes steps by `(event_type, from_version)` and
//!   composes them at replay. Registration rejects a duplicate / ambiguous
//!   `(type, from)` key so the composed chain is always a deterministic
//!   function; replay rejects a missing or overshooting step with
//!   [`MigrationError::NoPathToVersion`].
//!
//! # Example
//!
//! ```
//! use serde_json::{json, Value};
//! use spork_migrate::{EventMigration, MigrationError, MigrationRegistry};
//!
//! /// `v1 -> v2`: rename `title` to `name`.
//! struct RenameTitle;
//! impl EventMigration for RenameTitle {
//!     fn event_type(&self) -> &str { "doc.created" }
//!     fn from_version(&self) -> u16 { 1 }
//!     fn to_version(&self) -> u16 { 2 }
//!     fn upgrade(&self, mut p: Value) -> Result<Value, MigrationError> {
//!         let obj = p
//!             .as_object_mut()
//!             .ok_or_else(|| MigrationError::Upgrade("expected object".into()))?;
//!         if let Some(title) = obj.remove("title") {
//!             obj.insert("name".into(), title);
//!         }
//!         Ok(p)
//!     }
//! }
//!
//! let mut registry = MigrationRegistry::new();
//! registry.register(Box::new(RenameTitle)).unwrap();
//!
//! let stored = json!({ "title": "hello" }); // written at schema_version 1
//! let (current, version) =
//!     registry.upgrade_to_current("doc.created", 1, 2, stored).unwrap();
//! assert_eq!(version, 2);
//! assert_eq!(current, json!({ "name": "hello" }));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod migration;
mod registry;

pub use error::MigrationError;
pub use migration::EventMigration;
pub use registry::MigrationRegistry;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// A small, configurable test migration: from `from` to `to`, inserting a
    /// marker key so we can observe exactly which steps ran and in what order.
    struct Bump {
        event_type: &'static str,
        from: u16,
        to: u16,
    }

    impl Bump {
        fn boxed(event_type: &'static str, from: u16, to: u16) -> Box<dyn EventMigration> {
            Box::new(Self {
                event_type,
                from,
                to,
            })
        }
    }

    impl EventMigration for Bump {
        fn event_type(&self) -> &str {
            self.event_type
        }
        fn from_version(&self) -> u16 {
            self.from
        }
        fn to_version(&self) -> u16 {
            self.to
        }
        fn upgrade(&self, mut payload: Value) -> Result<Value, MigrationError> {
            let obj = payload
                .as_object_mut()
                .ok_or_else(|| MigrationError::Upgrade("expected object payload".into()))?;
            // Record the version we are upgrading *to*; appending lets us read
            // the full applied chain back out of the final payload.
            let steps = obj
                .entry("steps")
                .or_insert_with(|| Value::Array(Vec::new()));
            steps
                .as_array_mut()
                .expect("steps is an array")
                .push(json!(self.to));
            obj.insert("v".into(), json!(self.to));
            Ok(payload)
        }
    }

    /// A migration that always fails, to exercise error propagation.
    struct AlwaysFails;
    impl EventMigration for AlwaysFails {
        fn event_type(&self) -> &str {
            "boom"
        }
        fn from_version(&self) -> u16 {
            1
        }
        fn to_version(&self) -> u16 {
            2
        }
        fn upgrade(&self, _payload: Value) -> Result<Value, MigrationError> {
            Err(MigrationError::Upgrade("deliberate failure".into()))
        }
    }

    #[test]
    fn empty_registry_basics() {
        let reg = MigrationRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        // Default matches new().
        let def = MigrationRegistry::default();
        assert!(def.is_empty());
    }

    #[test]
    fn chain_v1_to_v3_upgrades_v1_payload() {
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 2)).unwrap();
        reg.register(Bump::boxed("node", 2, 3)).unwrap();
        assert_eq!(reg.len(), 2);

        let (out, version) = reg
            .upgrade_to_current("node", 1, 3, json!({ "id": "a" }))
            .unwrap();

        assert_eq!(version, 3);
        // Both steps ran, in order 1->2 then 2->3.
        assert_eq!(out["steps"], json!([2, 3]));
        assert_eq!(out["v"], json!(3));
        assert_eq!(out["id"], json!("a"));
    }

    #[test]
    fn longer_chain_v1_to_v5() {
        let mut reg = MigrationRegistry::new();
        for v in 1..5u16 {
            reg.register(Bump::boxed("node", v, v + 1)).unwrap();
        }
        let (out, version) = reg.upgrade_to_current("node", 1, 5, json!({})).unwrap();
        assert_eq!(version, 5);
        assert_eq!(out["steps"], json!([2, 3, 4, 5]));
    }

    #[test]
    fn partial_upgrade_starting_midchain() {
        // A payload stored at v2 only needs the 2->3 step to reach v3.
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 2)).unwrap();
        reg.register(Bump::boxed("node", 2, 3)).unwrap();

        let (out, version) = reg.upgrade_to_current("node", 2, 3, json!({})).unwrap();
        assert_eq!(version, 3);
        assert_eq!(out["steps"], json!([3]));
    }

    #[test]
    fn already_current_is_noop() {
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 2)).unwrap();

        let original = json!({ "untouched": true });
        let (out, version) = reg
            .upgrade_to_current("node", 2, 2, original.clone())
            .unwrap();
        assert_eq!(version, 2);
        // Bit-for-bit identical: no migration ran, no marker keys added.
        assert_eq!(out, original);
    }

    #[test]
    fn future_version_is_noop_not_error() {
        // A reader pinned to current=2 reading a stored event at v3 must not
        // attempt a downgrade; it returns the payload as-is at its own version.
        let reg = MigrationRegistry::new();
        let original = json!({ "from_the_future": 1 });
        let (out, version) = reg
            .upgrade_to_current("node", 3, 2, original.clone())
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(out, original);
    }

    #[test]
    fn no_migrations_but_current_means_noop() {
        let reg = MigrationRegistry::new();
        let (out, version) = reg
            .upgrade_to_current("node", 4, 4, json!({ "ok": 1 }))
            .unwrap();
        assert_eq!(version, 4);
        assert_eq!(out, json!({ "ok": 1 }));
    }

    #[test]
    fn missing_step_yields_no_path() {
        // Register 1->2 but not 2->3; asking for v3 must fail.
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 2)).unwrap();

        let err = reg.upgrade_to_current("node", 1, 3, json!({})).unwrap_err();
        assert_eq!(
            err,
            MigrationError::NoPathToVersion {
                event_type: "node".into(),
                from: 1,
                to: 3,
            }
        );
    }

    #[test]
    fn no_migrations_at_all_yields_no_path() {
        let reg = MigrationRegistry::new();
        let err = reg.upgrade_to_current("node", 1, 2, json!({})).unwrap_err();
        assert_eq!(
            err,
            MigrationError::NoPathToVersion {
                event_type: "node".into(),
                from: 1,
                to: 2,
            }
        );
    }

    #[test]
    fn overshooting_step_yields_no_path() {
        // A 1->3 step exists but the caller wants exactly v2; there is no way to
        // land on v2, so this is a missing path, not a silent overshoot.
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 3)).unwrap();

        let err = reg.upgrade_to_current("node", 1, 2, json!({})).unwrap_err();
        assert_eq!(
            err,
            MigrationError::NoPathToVersion {
                event_type: "node".into(),
                from: 1,
                to: 2,
            }
        );
    }

    #[test]
    fn multi_step_jump_is_allowed() {
        // A registered 1->3 step is fine when the target is exactly 3.
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 3)).unwrap();
        let (out, version) = reg.upgrade_to_current("node", 1, 3, json!({})).unwrap();
        assert_eq!(version, 3);
        assert_eq!(out["steps"], json!([3]));
    }

    #[test]
    fn duplicate_registration_rejected() {
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 2)).unwrap();
        let err = reg.register(Bump::boxed("node", 1, 9)).unwrap_err();
        assert_eq!(
            err,
            MigrationError::DuplicateMigration {
                event_type: "node".into(),
                from: 1,
            }
        );
        // The first registration survived intact.
        assert_eq!(reg.len(), 1);
        let (_, version) = reg.upgrade_to_current("node", 1, 2, json!({})).unwrap();
        assert_eq!(version, 2);
    }

    #[test]
    fn same_from_different_event_types_coexist() {
        // The key is (event_type, from), so a `from=1` step is allowed once per
        // event type.
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("alpha", 1, 2)).unwrap();
        reg.register(Bump::boxed("beta", 1, 2)).unwrap();
        assert_eq!(reg.len(), 2);

        let (a, _) = reg.upgrade_to_current("alpha", 1, 2, json!({})).unwrap();
        let (b, _) = reg.upgrade_to_current("beta", 1, 2, json!({})).unwrap();
        assert_eq!(a["v"], json!(2));
        assert_eq!(b["v"], json!(2));
    }

    #[test]
    fn unknown_event_type_yields_no_path() {
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("known", 1, 2)).unwrap();
        let err = reg
            .upgrade_to_current("unknown", 1, 2, json!({}))
            .unwrap_err();
        assert_eq!(
            err,
            MigrationError::NoPathToVersion {
                event_type: "unknown".into(),
                from: 1,
                to: 2,
            }
        );
    }

    #[test]
    fn non_forward_migration_rejected_at_registration() {
        let mut reg = MigrationRegistry::new();
        // from == to
        let err = reg.register(Bump::boxed("node", 2, 2)).unwrap_err();
        assert!(matches!(err, MigrationError::Upgrade(_)));
        // from > to
        let err = reg.register(Bump::boxed("node", 3, 1)).unwrap_err();
        assert!(matches!(err, MigrationError::Upgrade(_)));
        assert!(reg.is_empty());
    }

    #[test]
    fn step_failure_propagates() {
        let mut reg = MigrationRegistry::new();
        reg.register(Box::new(AlwaysFails)).unwrap();
        let err = reg.upgrade_to_current("boom", 1, 2, json!({})).unwrap_err();
        assert_eq!(err, MigrationError::Upgrade("deliberate failure".into()));
    }

    #[test]
    fn non_object_payload_surfaces_step_error() {
        let mut reg = MigrationRegistry::new();
        reg.register(Bump::boxed("node", 1, 2)).unwrap();
        let err = reg
            .upgrade_to_current("node", 1, 2, json!("not an object"))
            .unwrap_err();
        assert!(matches!(err, MigrationError::Upgrade(_)));
    }

    #[test]
    fn realistic_rename_then_add_field_chain() {
        // v1 -> v2 renames `title` to `name`; v2 -> v3 adds a defaulted flag.
        struct Rename;
        impl EventMigration for Rename {
            fn event_type(&self) -> &str {
                "doc"
            }
            fn from_version(&self) -> u16 {
                1
            }
            fn to_version(&self) -> u16 {
                2
            }
            fn upgrade(&self, mut p: Value) -> Result<Value, MigrationError> {
                let obj = p
                    .as_object_mut()
                    .ok_or_else(|| MigrationError::Upgrade("expected object".into()))?;
                if let Some(title) = obj.remove("title") {
                    obj.insert("name".into(), title);
                }
                Ok(p)
            }
        }
        struct AddFlag;
        impl EventMigration for AddFlag {
            fn event_type(&self) -> &str {
                "doc"
            }
            fn from_version(&self) -> u16 {
                2
            }
            fn to_version(&self) -> u16 {
                3
            }
            fn upgrade(&self, mut p: Value) -> Result<Value, MigrationError> {
                p.as_object_mut()
                    .ok_or_else(|| MigrationError::Upgrade("expected object".into()))?
                    .insert("archived".into(), json!(false));
                Ok(p)
            }
        }

        let mut reg = MigrationRegistry::new();
        reg.register(Box::new(Rename)).unwrap();
        reg.register(Box::new(AddFlag)).unwrap();

        let (out, version) = reg
            .upgrade_to_current("doc", 1, 3, json!({ "title": "Spork" }))
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(out, json!({ "name": "Spork", "archived": false }));
    }

    #[test]
    fn error_messages_are_descriptive() {
        let dup = MigrationError::DuplicateMigration {
            event_type: "node".into(),
            from: 1,
        };
        assert!(dup.to_string().contains("duplicate migration"));
        assert!(dup.to_string().contains("node"));

        let no_path = MigrationError::NoPathToVersion {
            event_type: "node".into(),
            from: 1,
            to: 3,
        };
        assert!(no_path.to_string().contains("no migration path"));
        assert!(no_path.to_string().contains('3'));

        let up = MigrationError::Upgrade("bad field".into());
        assert!(up.to_string().contains("bad field"));
    }
}
