//! The canonical [`IgnoreProfile`] and its content-derived `ignore_profile_hash`.
//!
//! An [`IgnoreProfile`] is the *frozen* description of which paths are excluded
//! from a snapshot. Because the exclusion set changes what a snapshot contains,
//! its identity must itself be content-addressed and baked into snapshot
//! identity: the [`IgnoreProfile::hash`] is the `ignore_profile_hash` field of a
//! Snapshot (DESIGN.md §6.1, §10.4, §10.5). Two profiles describing the same
//! exclusion set — regardless of how the caller ordered or duplicated the
//! patterns — must hash identically, so the profile stores its patterns
//! **sorted and de-duplicated** and is encoded through the frozen canonical
//! encoder ([`spork_canon`]).
//!
//! Design references: DESIGN.md §10.4 (exclusion lists / gitignore-aware
//! walking), §10.5 (deps-excluded-by-default; `ignore_profile_hash` baked into
//! snapshot identity and frozen at F0), §6.1 (content identity via the canonical
//! encoder + BLAKE3).

use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use thiserror::Error;

/// The schema version of the [`IgnoreProfile`] wire/identity format.
///
/// This is a persisted, *hashed* schema version (constraint C5): it travels in
/// the canonical bytes and therefore participates in `ignore_profile_hash`. A
/// change to the meaning of the profile encoding is a deliberate version bump
/// (and a loud hash change), never an in-place reinterpretation.
pub const IGNORE_PROFILE_VERSION: u16 = 1;

/// Errors that can arise while constructing or validating an [`IgnoreProfile`].
///
/// Canonical encoding of a profile cannot fail in practice — the profile is a
/// version number and an array of strings, none of which can be a float or an
/// otherwise un-encodable value — but the fallible surface is kept explicit so
/// callers never have to reach for a panic, and so a future schema change that
/// introduces a richer (potentially un-encodable) field has a place to report.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IgnoreError {
    /// The profile could not be reduced to canonical bytes.
    ///
    /// Wraps the underlying [`spork_canon::CanonError`] message.
    #[error("failed to canonicalize ignore profile: {0}")]
    Canon(String),

    /// A pattern was empty (after trimming), which carries no meaning and would
    /// silently widen or void the exclusion set.
    #[error("ignore pattern at index {0} is empty")]
    EmptyPattern(usize),
}

/// A frozen, content-addressed description of which paths a snapshot excludes.
///
/// The patterns are gitignore-flavored globs (see [`crate::IgnoreMatcher`] for
/// the exact, frozen matching semantics). For identity to be stable the
/// [`patterns`](IgnoreProfile::patterns) are always held **sorted ascending by
/// UTF-8 bytes and de-duplicated**; every constructor enforces this invariant,
/// so two profiles with the same *set* of patterns are byte-identical and hash
/// identically no matter how the caller supplied them.
///
/// # Identity
/// The canonical bytes are `canonicalize({ "patterns": [...], "version": N })`
/// (keys byte-sorted by the encoder), and `ignore_profile_hash =
/// BLAKE3(canonical_bytes)`. See [`IgnoreProfile::canonical_bytes`] and
/// [`IgnoreProfile::hash`].
///
/// # Example
/// ```
/// use spork_ignore::IgnoreProfile;
///
/// // Construction order and duplicates never affect identity.
/// let a = IgnoreProfile::from_patterns(["target/", "node_modules/", "target/"]).unwrap();
/// let b = IgnoreProfile::from_patterns(["node_modules/", "target/"]).unwrap();
/// assert_eq!(a.patterns(), b.patterns());
/// assert_eq!(a.hash(), b.hash());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IgnoreProfile {
    /// The schema version of this profile (hashed; see [`IGNORE_PROFILE_VERSION`]).
    pub version: u16,
    /// The exclusion patterns, stored **sorted and de-duplicated** for canonical
    /// identity. Treat this as read-only; use the constructors to build a
    /// profile so the invariant is never violated.
    pub patterns: Vec<String>,
}

impl IgnoreProfile {
    /// Build a profile from an iterator of patterns, normalizing for identity.
    ///
    /// Each pattern is trimmed of surrounding ASCII whitespace; the resulting
    /// patterns are sorted ascending by UTF-8 bytes and de-duplicated, so the
    /// stored [`patterns`](IgnoreProfile::patterns) are canonical regardless of
    /// input order or repetition. The [`version`](IgnoreProfile::version) is set
    /// to [`IGNORE_PROFILE_VERSION`].
    ///
    /// # Errors
    /// Returns [`IgnoreError::EmptyPattern`] if any pattern is empty after
    /// trimming — an empty pattern is meaningless and would only obscure the
    /// exclusion set.
    ///
    /// # Example
    /// ```
    /// use spork_ignore::IgnoreProfile;
    /// let p = IgnoreProfile::from_patterns(["  *.pyc ", "node_modules/"]).unwrap();
    /// assert_eq!(p.patterns(), &["*.pyc".to_string(), "node_modules/".to_string()]);
    /// ```
    pub fn from_patterns<I, S>(patterns: I) -> Result<Self, IgnoreError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut normalized: Vec<String> = Vec::new();
        for (idx, pat) in patterns.into_iter().enumerate() {
            let pat = pat.into();
            let trimmed = pat.trim();
            if trimmed.is_empty() {
                return Err(IgnoreError::EmptyPattern(idx));
            }
            normalized.push(trimmed.to_string());
        }
        normalized.sort_unstable();
        normalized.dedup();
        Ok(IgnoreProfile {
            version: IGNORE_PROFILE_VERSION,
            patterns: normalized,
        })
    }

    /// The Spork default profile: the **deps-excluded-by-default** policy.
    ///
    /// This is the v1 exclusion set frozen at Foundation (F0): the heavy,
    /// *derived* trees that must never enter the per-node content store —
    /// dependency directories, build outputs, language caches — plus common
    /// editor/OS detritus and compiled artifacts. Lockfiles and manifests are
    /// deliberately **not** excluded, because they are exactly what a snapshot
    /// must keep so the dependency layer can be reconstructed (DESIGN.md §10.5).
    ///
    /// The returned profile is normalized (sorted + de-duped) like any other, so
    /// its [`hash`](IgnoreProfile::hash) is the stable, frozen default
    /// `ignore_profile_hash`.
    ///
    /// # Example
    /// ```
    /// use spork_ignore::{IgnoreProfile, IgnoreMatcher};
    /// use std::path::Path;
    ///
    /// let m = IgnoreMatcher::new(&IgnoreProfile::default_profile());
    /// assert!(m.is_ignored(Path::new("node_modules"), true));
    /// assert!(m.is_ignored(Path::new("a/b/__pycache__"), true));
    /// assert!(!m.is_ignored(Path::new("Cargo.lock"), false)); // lockfiles stay
    /// ```
    #[must_use]
    pub fn default_profile() -> Self {
        // Constructed from a static, deterministic list. The list is the frozen
        // F0 deps-excluded-by-default policy; `from_patterns` re-sorts and
        // de-dupes so the source order here is irrelevant to identity.
        Self::from_patterns(DEFAULT_PATTERNS.iter().copied())
            .expect("default patterns are non-empty by construction")
    }

    /// Borrow the normalized (sorted + de-duped) patterns.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Produce the canonical bytes of this profile via [`spork_canon`].
    ///
    /// This is the exact byte sequence whose BLAKE3 digest is the
    /// `ignore_profile_hash`. Because the encoder is frozen and the patterns are
    /// stored normalized, these bytes are reproducible on any machine.
    ///
    /// # Errors
    /// Returns [`IgnoreError::Canon`] if canonical encoding fails. With the v1
    /// schema (a `u16` and an array of strings) this cannot occur, but the
    /// fallible signature keeps the contract honest across future schema
    /// evolution.
    ///
    /// # Example
    /// ```
    /// use spork_ignore::IgnoreProfile;
    /// let p = IgnoreProfile::from_patterns(["b/", "a/"]).unwrap();
    /// // Patterns are sorted; the encoder byte-sorts the object keys.
    /// assert_eq!(
    ///     p.canonical_bytes().unwrap(),
    ///     br#"{"patterns":["a/","b/"],"version":1}"#
    /// );
    /// ```
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, IgnoreError> {
        spork_canon::canonicalize(self).map_err(|e| IgnoreError::Canon(e.to_string()))
    }

    /// Compute `ignore_profile_hash = BLAKE3(canonical_bytes)`.
    ///
    /// This is the value that a Snapshot records so that *which exclusion set
    /// produced it* is part of the snapshot's content identity (DESIGN.md §6.1,
    /// §10.5).
    ///
    /// # Panics
    /// Panics only if [`canonical_bytes`](IgnoreProfile::canonical_bytes) fails,
    /// which is impossible for the v1 schema (see that method's docs). The
    /// non-panicking form is [`IgnoreProfile::try_hash`].
    ///
    /// # Example
    /// ```
    /// use spork_ignore::IgnoreProfile;
    /// // Same set of patterns => same hash, independent of order/duplicates.
    /// let a = IgnoreProfile::from_patterns(["x/", "y/", "x/"]).unwrap();
    /// let b = IgnoreProfile::from_patterns(["y/", "x/"]).unwrap();
    /// assert_eq!(a.hash(), b.hash());
    /// ```
    #[must_use]
    pub fn hash(&self) -> Hash {
        self.try_hash()
            .expect("v1 ignore profile is always canonically encodable")
    }

    /// Fallible form of [`IgnoreProfile::hash`].
    ///
    /// # Errors
    /// Returns [`IgnoreError::Canon`] if canonical encoding fails (impossible
    /// for the v1 schema; present for evolution safety).
    pub fn try_hash(&self) -> Result<Hash, IgnoreError> {
        Ok(spork_hash::hash_bytes(&self.canonical_bytes()?))
    }
}

/// The frozen F0 deps-excluded-by-default pattern list (DESIGN.md §10.5).
///
/// Grouped by intent for readability; the order here is irrelevant to identity
/// because [`IgnoreProfile::from_patterns`] sorts and de-dupes. Lockfiles and
/// manifests are intentionally absent — they must be snapshotted.
const DEFAULT_PATTERNS: &[&str] = &[
    // --- Dependency / package directories ---
    "node_modules/",
    ".venv/",
    "venv/",
    "env/",
    "vendor/",
    // --- Build / output directories ---
    "target/",
    "dist/",
    "build/",
    "out/",
    ".next/",
    ".nuxt/",
    // --- Language / tool caches ---
    "__pycache__/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    ".gradle/",
    ".cargo/",
    // --- Compiled / generated artifacts ---
    "*.pyc",
    "*.pyo",
    "*.o",
    "*.obj",
    "*.a",
    "*.so",
    "*.dylib",
    "*.class",
    // --- Editor / OS detritus ---
    ".DS_Store",
    "Thumbs.db",
    "*.swp",
    "*.swo",
    "*~",
    // --- VCS / nested-repo metadata (snapshot the tree, not the VCS db) ---
    ".git/",
    ".hg/",
    ".svn/",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_frozen_at_one() {
        assert_eq!(IGNORE_PROFILE_VERSION, 1);
        assert_eq!(IgnoreProfile::default_profile().version, 1);
    }

    #[test]
    fn patterns_are_sorted_and_deduped() {
        let p = IgnoreProfile::from_patterns(["c/", "a/", "b/", "a/", "c/"]).unwrap();
        assert_eq!(
            p.patterns(),
            &["a/".to_string(), "b/".to_string(), "c/".to_string()]
        );
    }

    #[test]
    fn patterns_are_trimmed() {
        let p = IgnoreProfile::from_patterns(["  *.pyc\t", " node_modules/ "]).unwrap();
        assert_eq!(
            p.patterns(),
            &["*.pyc".to_string(), "node_modules/".to_string()]
        );
    }

    #[test]
    fn empty_pattern_is_rejected() {
        let err = IgnoreProfile::from_patterns(["a/", "   ", "b/"]).unwrap_err();
        assert!(matches!(err, IgnoreError::EmptyPattern(1)));
    }

    #[test]
    fn construction_order_is_identity_invariant() {
        let a = IgnoreProfile::from_patterns(["target/", "node_modules/", "*.pyc"]).unwrap();
        let b = IgnoreProfile::from_patterns(["*.pyc", "target/", "node_modules/"]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.canonical_bytes().unwrap(), b.canonical_bytes().unwrap());
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn duplicates_do_not_change_identity() {
        let a = IgnoreProfile::from_patterns(["x/", "y/"]).unwrap();
        let b = IgnoreProfile::from_patterns(["x/", "x/", "y/", "y/", "x/"]).unwrap();
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn canonical_bytes_are_exact_and_stable() {
        let p = IgnoreProfile::from_patterns(["b/", "a/"]).unwrap();
        // Encoder byte-sorts object keys: "patterns" < "version".
        assert_eq!(
            p.canonical_bytes().unwrap(),
            br#"{"patterns":["a/","b/"],"version":1}"#
        );
        // ...and re-encoding is identical.
        assert_eq!(p.canonical_bytes().unwrap(), p.canonical_bytes().unwrap());
    }

    #[test]
    fn hash_matches_blake3_of_canonical_bytes() {
        let p = IgnoreProfile::default_profile();
        let expected = spork_hash::hash_bytes(&p.canonical_bytes().unwrap());
        assert_eq!(p.hash(), expected);
        assert_eq!(p.try_hash().unwrap(), expected);
    }

    #[test]
    fn default_profile_is_stable_across_constructions() {
        // The frozen `ignore_profile_hash` must not drift between calls.
        assert_eq!(
            IgnoreProfile::default_profile().hash(),
            IgnoreProfile::default_profile().hash()
        );
    }

    #[test]
    fn default_profile_excludes_deps_and_keeps_lockfiles() {
        let p = IgnoreProfile::default_profile();
        // Representative deps/artifacts are present...
        for needed in [
            "node_modules/",
            ".git/",
            "target/",
            "__pycache__/",
            "dist/",
            "build/",
            "*.pyc",
            ".DS_Store",
            "*.o",
            "*.class",
            ".venv/",
            "venv/",
        ] {
            assert!(
                p.patterns().contains(&needed.to_string()),
                "default profile must contain {needed}"
            );
        }
        // ...and lockfiles/manifests are deliberately NOT excluded.
        for kept in [
            "Cargo.lock",
            "package-lock.json",
            "poetry.lock",
            "Cargo.toml",
            "package.json",
        ] {
            assert!(
                !p.patterns().contains(&kept.to_string()),
                "default profile must not exclude {kept}"
            );
        }
    }

    #[test]
    fn profile_serde_round_trips() {
        let p = IgnoreProfile::default_profile();
        let json = serde_json::to_string(&p).unwrap();
        let back: IgnoreProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
        assert_eq!(p.hash(), back.hash());
    }
}
