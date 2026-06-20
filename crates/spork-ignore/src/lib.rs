//! Spork ignore profiles — the frozen, content-addressed exclusion layer.
//!
//! This crate defines *what a snapshot leaves out*, and makes that choice part
//! of the snapshot's content identity. It has two halves:
//!
//! - [`IgnoreProfile`] — the canonical, hashable description of an exclusion set.
//!   Its [`hash`](IgnoreProfile::hash) is the `ignore_profile_hash` recorded on
//!   every Snapshot (DESIGN.md §6.1, §10.4), so two snapshots taken under
//!   different exclusion policies are *distinct objects*, never silently
//!   conflated. The profile is encoded through the frozen canonical encoder
//!   ([`spork_canon`]) and digested with BLAKE3 ([`spork_hash`]), so the same
//!   exclusion set hashes identically on any machine.
//! - [`IgnoreMatcher`] — the compiled predicate the directory walk consults to
//!   decide, per entry, whether a path is excluded.
//!
//! # Deps excluded by default
//!
//! [`IgnoreProfile::default_profile`] is the v1 **deps-excluded-by-default**
//! policy frozen at Foundation (F0): the heavy, *derived* trees
//! (`node_modules`, `target/`, `.venv/`, `__pycache__/`, build outputs,
//! compiled artifacts, editor/OS detritus, nested VCS metadata) never enter the
//! per-node content store, while lockfiles and manifests are deliberately kept
//! so the dependency layer can be reconstructed read-only from the shared asset
//! cache (DESIGN.md §10.5). Because excluding deps *changes snapshot identity*
//! via `ignore_profile_hash`, this is a no-domino-safe (C2) decision locked in
//! F0; richer profiles (e.g. gitignore `!` negation) are added additively behind
//! a future [`IGNORE_PROFILE_VERSION`].
//!
//! # Example
//! ```
//! use spork_ignore::{IgnoreProfile, IgnoreMatcher};
//! use std::path::Path;
//!
//! let profile = IgnoreProfile::default_profile();
//! // The default exclusion set has a stable, content-addressed identity.
//! let ignore_profile_hash = profile.hash();
//! assert_eq!(ignore_profile_hash, IgnoreProfile::default_profile().hash());
//!
//! let matcher = IgnoreMatcher::new(&profile);
//! assert!(matcher.is_ignored(Path::new("node_modules"), true));
//! assert!(matcher.is_ignored(Path::new("a/b/target/debug/app"), false));
//! assert!(!matcher.is_ignored(Path::new("src/main.rs"), false));
//! assert!(!matcher.is_ignored(Path::new("Cargo.lock"), false)); // lockfiles kept
//! ```
//!
//! Design references: DESIGN.md §10.4 (gitignore-aware walking / exclusion
//! lists), §10.5 (deps excluded by default; `ignore_profile_hash` baked into
//! snapshot identity and frozen at F0), §6.1 (content identity via the canonical
//! encoder + BLAKE3).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod matcher;
mod profile;

pub use matcher::IgnoreMatcher;
pub use profile::{IgnoreError, IgnoreProfile, IGNORE_PROFILE_VERSION};

#[cfg(test)]
mod integration_tests {
    //! Cross-module checks that the profile and matcher agree on the frozen
    //! contract: identity is stable and the default policy behaves end-to-end.

    use super::*;
    use std::path::Path;

    #[test]
    fn default_profile_hash_is_deterministic_end_to_end() {
        // Build the default profile two independent ways and confirm both the
        // hash and the resulting predicate agree — the property the snapshot
        // layer relies on for `ignore_profile_hash`.
        let p1 = IgnoreProfile::default_profile();
        let p2 = IgnoreProfile::from_patterns(p1.patterns().iter().cloned()).unwrap();
        assert_eq!(p1.hash(), p2.hash());

        let m1 = IgnoreMatcher::new(&p1);
        let m2 = IgnoreMatcher::new(&p2);
        for (path, is_dir) in [
            ("node_modules/lib/x.js", false),
            ("src/lib.rs", false),
            ("target", true),
            ("venv", true),
        ] {
            assert_eq!(
                m1.is_ignored(Path::new(path), is_dir),
                m2.is_ignored(Path::new(path), is_dir),
            );
        }
    }

    #[test]
    fn a_custom_profile_has_a_distinct_hash_from_default() {
        // A different exclusion set must be a different object.
        let default = IgnoreProfile::default_profile();
        let mut patterns: Vec<String> = default.patterns().to_vec();
        patterns.push("custom_artifacts/".to_string());
        let custom = IgnoreProfile::from_patterns(patterns).unwrap();
        assert_ne!(default.hash(), custom.hash());
    }
}
