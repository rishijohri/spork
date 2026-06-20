//! Spork P5 built-in node types — Edit, Validation, Stress, Sanity, Merge, and
//! Snapshot.
//!
//! This crate hosts the six built-in
//! [`NodeTypeDescriptor`](spork_registry::NodeTypeDescriptor)s, their
//! schema-versioned payloads, and the observing Runners. Every built-in
//! registers through the *same* public [`spork_registry`] `NodeTypeRegistry`
//! (F2) and runs through the *same* [`spork_runner`] Runner SPI (F4) that a
//! third-party plugin will use — **there is no built-in-only code path**. That
//! dogfooding is what makes the P8 plugin phase purely additive (DESIGN.md §7.1,
//! §8.1, §9).
//!
//! Import is *not* a distinct kind: it is a [`Snapshot`](snapshot) node with
//! `origin = import` (DESIGN.md A.7 C-2, §6.2).
//!
//! # The six built-ins
//!
//! | Module | Kind | Family | Owns snapshot | Role |
//! |--------|------|--------|---------------|------|
//! | [`edit`] | `codebase-edit` | Mutating | yes | the headline edit; binds a snapshot + conversation |
//! | [`validation`] | `validation` | Observing | no | runs a test command; junit-xml → per-unit results |
//! | [`stress`] | `stress` | Observing | no | runs a load command; perf metrics with a direction |
//! | [`sanity`] | `sanity` | Observing | no | the deterministic auto-run check; emits violations |
//! | [`merge`] | `merge` | Mutating | yes (≥2 parents) | 3-way reconciliation → clean Merge node or conflict set |
//! | [`snapshot`] | `snapshot` | Mutating | yes | drift reconcile / manual / **import** |
//!
//! # Registration (the dogfood rule)
//!
//! [`register_builtins`] registers all six descriptors through the *public*
//! [`NodeTypeRegistry::register`](spork_registry::NodeTypeRegistry::register)
//! path — exactly the call a third party makes (DESIGN.md §7.1, §9). There is no
//! separate "built-in" entry point on the registry, so a test can assert that the
//! built-ins are subject to the same contract (the `owns_snapshot` ⇒ `SnapshotRef`
//! out-port rule, the duplicate-version rule) as any plugin.
//!
//! # Runners (the one SPI)
//!
//! The three observing kinds run behind the one F4 [`Runner`](spork_runner::Runner)
//! SPI: [`validation::ValidationRunner`] (junit-xml → units),
//! [`stress::StressRunner`] (perf report → metrics), and the *reused* F4
//! [`SanityRunner`](spork_runner::SanityRunner) for the deterministic auto-run
//! sanity check ([`sanity::runner`]). [`runner_for`] routes a check kind to the
//! right runner so the daemon dispatches with no special-casing.
//!
//! # Frozen seams this crate builds behind
//!
//! - The versioned node envelope + payload split (DESIGN.md §6.2).
//! - Three-way merge against the nearest common ancestor, recorded as a
//!   [`spork_merge`] `ConflictResolution` (DESIGN.md §6.5, A.4).
//! - The built-in node taxonomy (DESIGN.md §7.1).
//! - One Runner SPI shared by every check kind, and the change-scoped,
//!   debounced, hermetic auto-run loop (DESIGN.md §8.1, §8.2).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod descriptor;
pub mod edit;
mod error;
mod junit;
pub mod merge;
pub mod sanity;
pub mod snapshot;
pub mod stress;
pub mod validation;

pub use descriptor::{type_version, SNAPSHOT_OUT_PORT_NAME};
pub use error::{NodesError, Result};

// Re-export the payload/descriptor surface of each built-in at the crate root so
// a consumer (the daemon, a test, a future plugin author reading the built-ins
// as a template) binds to one crate.
pub use edit::{
    descriptor as edit_descriptor, ContextSource, EditPayload, ToolCall, EDIT_KIND,
    EDIT_PAYLOAD_VERSION,
};
pub use merge::{
    descriptor as merge_descriptor, three_way_merge, three_way_merge_with_resolution, FileConflict,
    MergeOutcome, MergePayload, MERGE_KIND, MERGE_PAYLOAD_VERSION,
};
pub use sanity::{
    descriptor as sanity_descriptor, runner as sanity_runner, SanityPayload, SANITY_NODE_KIND,
    SANITY_PAYLOAD_VERSION,
};
pub use snapshot::{
    descriptor as snapshot_descriptor, ImportSource, SnapshotOrigin, SnapshotPayload,
    SNAPSHOT_KIND, SNAPSHOT_PAYLOAD_VERSION,
};
pub use stress::{
    descriptor as stress_descriptor, stress_metric_registry, StressPayload, StressRunner,
    STRESS_KIND, STRESS_PAYLOAD_VERSION, STRESS_VERSION,
};
pub use validation::{
    descriptor as validation_descriptor, ValidationPayload, ValidationRunner, VALIDATION_KIND,
    VALIDATION_PAYLOAD_VERSION, VALIDATION_VERSION,
};

use spork_registry::{NodeTypeDescriptor, NodeTypeRegistry};

/// Every built-in descriptor, in a stable order.
///
/// The six built-ins (DESIGN.md §7.1). Returning them as a slice lets a caller
/// register them, render a legend, or assert the set is complete without
/// hardcoding the list — and proves there is exactly one registration surface
/// (no built-in-only side door).
#[must_use]
pub fn builtin_descriptors() -> Vec<NodeTypeDescriptor> {
    vec![
        edit::descriptor(),
        validation::descriptor(),
        stress::descriptor(),
        sanity::descriptor(),
        merge::descriptor(),
        snapshot::descriptor(),
    ]
}

/// Register all six built-in node types through the **public** registry path.
///
/// This is the dogfooding rule made executable: every built-in goes through
/// [`NodeTypeRegistry::register`](spork_registry::NodeTypeRegistry::register) —
/// the *same* call a P8 plugin uses (DESIGN.md §7.1, §9) — so they are subject to
/// the identical contract (the `owns_snapshot` ⇒ `SnapshotRef` out-port rule, the
/// duplicate-version rule). There is no special-case path; this function is the
/// only difference between "no built-ins" and "the six built-ins available out of
/// the box."
///
/// Registration is all-or-nothing in intent but applied in order; on the first
/// rejection the error is returned and the registry holds whatever registered
/// before it. Callers that need exactly-once semantics should register into a
/// fresh registry (the common case at daemon construction).
///
/// # Errors
/// [`NodesError::Registry`] if the registry rejects any descriptor — e.g. a
/// `(id, version)` already registered ([`DuplicateVersion`](spork_registry::RegistryError::DuplicateVersion)),
/// or (impossible for the built-ins, which are well-formed) an
/// [`OwnsSnapshotMismatch`](spork_registry::RegistryError::OwnsSnapshotMismatch).
pub fn register_builtins(registry: &mut NodeTypeRegistry) -> Result<()> {
    for descriptor in builtin_descriptors() {
        registry.register(descriptor)?;
    }
    Ok(())
}

/// Resolve a built-in observing check `kind` to its [`Runner`](spork_runner::Runner).
///
/// Routes the three observing kinds to their runners behind the one F4 SPI:
/// `"validation"` → [`ValidationRunner`], `"stress"` → [`StressRunner`],
/// `"sanity"` → the reused F4 [`SanityRunner`](spork_runner::SanityRunner). A kind
/// with no built-in runner (a mutating kind, or an unknown kind) yields `None`,
/// so the caller can fall back to a plugin-provided runner — additively, with no
/// built-in special case (DESIGN.md §8.1).
#[must_use]
pub fn runner_for(kind: &str) -> Option<Box<dyn spork_runner::Runner>> {
    match kind {
        validation::VALIDATION_KIND => Some(Box::new(validation::ValidationRunner::new())),
        stress::STRESS_KIND => Some(Box::new(stress::StressRunner::new())),
        sanity::SANITY_NODE_KIND => Some(Box::new(sanity::runner())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_registry::Family;

    /// The six built-in kinds, for assertions.
    const BUILTIN_KINDS: [&str; 6] = [
        edit::EDIT_KIND,
        validation::VALIDATION_KIND,
        stress::STRESS_KIND,
        sanity::SANITY_NODE_KIND,
        merge::MERGE_KIND,
        snapshot::SNAPSHOT_KIND,
    ];

    #[test]
    fn register_builtins_registers_all_six_through_the_public_registry() {
        let mut reg = NodeTypeRegistry::new();
        register_builtins(&mut reg).unwrap();
        // Exactly six (id, version) descriptors, one per built-in.
        assert_eq!(reg.len(), 6);
        for kind in BUILTIN_KINDS {
            assert!(reg.resolve(kind, None).is_some(), "missing {kind}");
        }
    }

    #[test]
    fn there_is_no_builtin_only_registration_path() {
        // The built-ins use the SAME public `register` a third party uses: build
        // them through `register_builtins` and, separately, through the public
        // `register` call directly, and assert the registries are equivalent.
        let mut via_helper = NodeTypeRegistry::new();
        register_builtins(&mut via_helper).unwrap();

        let mut via_public = NodeTypeRegistry::new();
        for d in builtin_descriptors() {
            // This is the exact call a P8 plugin makes — no special casing.
            via_public.register(d).unwrap();
        }
        // Same count and same resolvable set.
        assert_eq!(via_helper.len(), via_public.len());
        for kind in BUILTIN_KINDS {
            assert_eq!(
                via_helper.resolve(kind, None).map(|d| d.id.clone()),
                via_public.resolve(kind, None).map(|d| d.id.clone()),
            );
        }
    }

    #[test]
    fn every_mutating_builtin_satisfies_the_contentref_rule() {
        // The §7.2 rule the registry enforces for ALL types, built-in or not: a
        // mutating owns_snapshot type must expose a SnapshotRef out-port. If any
        // built-in violated it, `register_builtins` would have errored — but
        // assert it directly too.
        for d in builtin_descriptors() {
            if d.family == Family::Mutating {
                assert!(d.owns_snapshot, "{} should own a snapshot", d.id);
                assert!(
                    d.has_snapshot_out_port(),
                    "{} must expose a SnapshotRef out-port",
                    d.id
                );
            } else {
                assert!(!d.owns_snapshot, "{} should not own a snapshot", d.id);
            }
        }
    }

    #[test]
    fn families_match_the_taxonomy() {
        let by_kind = |k: &str| {
            builtin_descriptors()
                .into_iter()
                .find(|d| d.id == k)
                .unwrap()
                .family
        };
        assert_eq!(by_kind(edit::EDIT_KIND), Family::Mutating);
        assert_eq!(by_kind(snapshot::SNAPSHOT_KIND), Family::Mutating);
        assert_eq!(by_kind(merge::MERGE_KIND), Family::Mutating);
        assert_eq!(by_kind(validation::VALIDATION_KIND), Family::Observing);
        assert_eq!(by_kind(stress::STRESS_KIND), Family::Observing);
        assert_eq!(by_kind(sanity::SANITY_NODE_KIND), Family::Observing);
    }

    #[test]
    fn duplicate_registration_is_rejected_like_any_plugin() {
        let mut reg = NodeTypeRegistry::new();
        register_builtins(&mut reg).unwrap();
        // Registering the built-ins again hits the registry's duplicate-version
        // rule — the SAME rule a plugin is held to (DESIGN §9.2).
        let err = register_builtins(&mut reg).unwrap_err();
        assert!(matches!(err, NodesError::Registry(_)));
    }

    #[test]
    fn runner_for_routes_observing_kinds_and_skips_mutating() {
        assert!(runner_for(validation::VALIDATION_KIND).is_some());
        assert!(runner_for(stress::STRESS_KIND).is_some());
        assert!(runner_for(sanity::SANITY_NODE_KIND).is_some());
        // Mutating kinds have no observing runner; an unknown kind has none.
        assert!(runner_for(edit::EDIT_KIND).is_none());
        assert!(runner_for(merge::MERGE_KIND).is_none());
        assert!(runner_for("totally-unknown").is_none());
    }

    #[test]
    fn runner_for_returns_a_runner_that_handles_its_kind() {
        for kind in [
            validation::VALIDATION_KIND,
            stress::STRESS_KIND,
            sanity::SANITY_NODE_KIND,
        ] {
            let runner = runner_for(kind).unwrap();
            assert!(runner.describe().handles(kind), "{kind} runner mismatch");
        }
    }
}
