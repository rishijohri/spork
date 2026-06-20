//! The [`IgnoreMatcher`]: turning an [`IgnoreProfile`] into a path predicate.
//!
//! The matcher compiles a profile's gitignore-flavored patterns into a
//! [`globset::GlobSet`] and answers a single question:
//! *is this relative path excluded from the snapshot?* It is the read side of
//! the exclusion machinery DESIGN.md §10.4/§10.5 describe; the directory walk in
//! `spork-cas` consults it per entry so dependency/artifact trees never enter
//! the content store.
//!
//! # Frozen matching semantics
//!
//! Patterns are a deterministic, gitignore-flavored subset. The rules below are
//! frozen alongside [`crate::IGNORE_PROFILE_VERSION`]; changing them is a
//! version bump, not an in-place tweak (a different exclusion set yields a
//! different `ignore_profile_hash`).
//!
//! Let a *pattern* be one entry of [`IgnoreProfile::patterns`]:
//!
//! - **Trailing `/` ⇒ directory-only.** `node_modules/` matches a path that is
//!   itself a directory named `node_modules`, *and* anything nested beneath such
//!   a directory. It never matches a regular file named `node_modules`.
//! - **No `/` anywhere ⇒ basename match at any depth.** `*.pyc` matches any
//!   path whose final component matches `*.pyc`; `.DS_Store` matches any path
//!   whose final component is `.DS_Store`. Such a pattern also excludes the
//!   entire subtree if the matched component is a directory.
//! - **An interior `/` ⇒ anchored to the snapshot root.** `a/b` matches exactly
//!   the path `a/b` (relative to the root), and its subtree if it is a
//!   directory; it does not match `x/a/b`.
//! - **Wildcards** are standard glob: `*` matches within a single path
//!   component (never a `/`), `?` matches one non-`/` character, `**` matches
//!   across components, and `[...]` is a character class.
//! - **Paths** are matched component-wise with `/` as the separator;
//!   backslashes in an input path are normalized to `/` so the predicate is
//!   identical on every platform. Matching is **case-sensitive**.
//!
//! Membership is *positive-only*: a path is ignored if it matches any pattern.
//! (Gitignore's `!` un-ignore negation is intentionally out of scope for the v1
//! frozen profile; it can be added additively behind a future profile version
//! without changing existing identities.)
//!
//! Design references: DESIGN.md §10.4 (gitignore-aware walking / exclusion
//! lists), §10.5 (deps excluded by default), §6.1 (identity stability).

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::profile::IgnoreProfile;

/// A compiled predicate that answers whether a relative path is excluded.
///
/// Build one with [`IgnoreMatcher::new`] from an [`IgnoreProfile`]; it is then
/// cheap to query many times via [`IgnoreMatcher::is_ignored`]. The matcher is
/// derived purely from the profile's patterns, so the same profile always yields
/// the same predicate.
///
/// # Example
/// ```
/// use spork_ignore::{IgnoreProfile, IgnoreMatcher};
/// use std::path::Path;
///
/// let profile = IgnoreProfile::from_patterns(["build/", "*.log", "src/gen/"]).unwrap();
/// let m = IgnoreMatcher::new(&profile);
///
/// assert!(m.is_ignored(Path::new("build"), true));          // the dir itself
/// assert!(m.is_ignored(Path::new("build/app.js"), false));  // nested under it
/// assert!(m.is_ignored(Path::new("a/b/server.log"), false));// basename glob
/// assert!(m.is_ignored(Path::new("src/gen"), true));        // anchored dir
/// assert!(!m.is_ignored(Path::new("gen"), true));           // anchor respected
/// assert!(!m.is_ignored(Path::new("README.md"), false));    // kept
/// ```
#[derive(Debug, Clone)]
pub struct IgnoreMatcher {
    /// Globs that match a path (or its subtree) regardless of file-vs-dir.
    any_kind: GlobSet,
    /// Globs from directory-only (`trailing-/`) patterns; only consulted for the
    /// *exact path is a directory* case (subtree containment is folded into
    /// `any_kind`, see [`IgnoreMatcher::new`]).
    dir_only: GlobSet,
}

impl IgnoreMatcher {
    /// Compile an [`IgnoreProfile`] into a matcher.
    ///
    /// Every pattern is translated into one or more globs:
    ///
    /// - A directory pattern `p/` becomes a *directory-only* exact glob (for the
    ///   directory entry itself) plus an *any-kind* subtree glob `p/**` (for
    ///   everything inside it).
    /// - A non-directory pattern becomes an *any-kind* exact glob plus an
    ///   *any-kind* subtree glob, so matching a directory also excludes its
    ///   contents.
    /// - A bare-basename pattern (no `/`) is additionally floated with a `**/`
    ///   prefix so it matches at any depth.
    ///
    /// Patterns that globset rejects as malformed are silently skipped rather
    /// than failing construction: an unparsable entry simply contributes no
    /// exclusions, which is the safe (never-over-exclude) default and keeps the
    /// matcher infallible for the frozen profile. The frozen default and all
    /// well-formed user patterns compile cleanly; this fallback only guards
    /// against a hand-rolled profile carrying a syntactically invalid glob.
    #[must_use]
    pub fn new(profile: &IgnoreProfile) -> Self {
        let mut any_kind = GlobSetBuilder::new();
        let mut dir_only = GlobSetBuilder::new();

        for pat in profile.patterns() {
            let is_dir_pattern = pat.ends_with('/');
            // Strip a single trailing slash to get the core pattern.
            let core = pat.strip_suffix('/').unwrap_or(pat);
            if core.is_empty() {
                // A lone "/" carries no meaning; skip it.
                continue;
            }

            // A pattern with no interior separator floats to any depth; one with
            // an interior separator is anchored to the root.
            let has_interior_sep = core.trim_end_matches('/').contains('/');
            let floated: Vec<String> = if has_interior_sep {
                vec![core.to_string()]
            } else {
                // Match at the root AND at any depth (`**/<name>`).
                vec![core.to_string(), format!("**/{core}")]
            };

            for base in &floated {
                // Subtree glob: everything beneath a matched directory. Applies
                // to both dir-patterns and plain patterns (a plain pattern that
                // matches a directory must also exclude its contents).
                add_glob(&mut any_kind, &format!("{base}/**"));

                if is_dir_pattern {
                    // The directory entry itself is only ignored when it IS a
                    // directory, so it goes to the dir-only set.
                    add_glob(&mut dir_only, base);
                } else {
                    // A plain pattern matches the entry regardless of kind.
                    add_glob(&mut any_kind, base);
                }
            }
        }

        IgnoreMatcher {
            // `build()` only fails if a contained glob is invalid, but every
            // glob added above already compiled individually in `add_glob`, so
            // the set build cannot fail. Fall back to an empty set defensively.
            any_kind: any_kind.build().unwrap_or_else(|_| empty_set()),
            dir_only: dir_only.build().unwrap_or_else(|_| empty_set()),
        }
    }

    /// Return whether `rel_path` is excluded by this matcher.
    ///
    /// `rel_path` must be **relative to the snapshot root** and uses the host's
    /// path separators (they are normalized internally). `is_dir` states whether
    /// the entry is a directory; it is what distinguishes a directory-only
    /// pattern (`node_modules/`) from a same-named file.
    ///
    /// An empty path is never ignored (it denotes the root itself).
    ///
    /// # Example
    /// ```
    /// use spork_ignore::{IgnoreProfile, IgnoreMatcher};
    /// use std::path::Path;
    ///
    /// let m = IgnoreMatcher::new(&IgnoreProfile::from_patterns(["logs/"]).unwrap());
    /// assert!(m.is_ignored(Path::new("logs"), true));    // directory matches
    /// assert!(!m.is_ignored(Path::new("logs"), false));  // a *file* named logs does not
    /// ```
    #[must_use]
    pub fn is_ignored(&self, rel_path: &Path, is_dir: bool) -> bool {
        let normalized = normalize(rel_path);
        if normalized.is_empty() {
            return false;
        }
        if self.any_kind.is_match(&normalized) {
            return true;
        }
        is_dir && self.dir_only.is_match(&normalized)
    }
}

/// Build a single-component-aware, case-sensitive glob and add it to `builder`.
///
/// `literal_separator(true)` makes `*`/`?` stop at `/`, giving gitignore's
/// component-wise semantics (so `*.pyc` does not leap across directories), while
/// `**` still spans components. A glob that fails to parse is skipped — see
/// [`IgnoreMatcher::new`] for why that is the safe behavior.
fn add_glob(builder: &mut GlobSetBuilder, pattern: &str) {
    if let Ok(glob) = build_glob(pattern) {
        builder.add(glob);
    }
}

/// Construct a glob with the frozen match options (literal separator, case
/// sensitive). Separated out so the option set lives in exactly one place and
/// the same semantics apply to every pattern the matcher compiles.
fn build_glob(pattern: &str) -> Result<Glob, globset::Error> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .case_insensitive(false)
        .build()
}

/// The empty globset (matches nothing); used as an infallible fallback.
fn empty_set() -> GlobSet {
    GlobSet::empty()
}

/// Normalize a relative path to a forward-slash, component-only string.
///
/// Both `/` and `\` are treated as separators so the predicate is identical on
/// every platform (on Unix, `\` is *not* a path separator to [`Path`], so we
/// normalize on the raw string rather than via [`Path::components`]). Empty,
/// `.`, and `..` segments are dropped — none belongs in a snapshot-relative
/// path's identity. Lossy non-UTF-8 bytes are rendered via
/// [`Path::to_string_lossy`] so a path is always matchable (a non-UTF-8 entry
/// then simply fails to match the UTF-8 patterns, which is the safe default).
fn normalize(rel_path: &Path) -> String {
    let raw = rel_path.to_string_lossy();
    raw.split(['/', '\\'])
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(patterns: &[&str]) -> IgnoreMatcher {
        IgnoreMatcher::new(&IgnoreProfile::from_patterns(patterns.iter().copied()).unwrap())
    }

    #[test]
    fn directory_pattern_matches_dir_and_subtree_only() {
        let m = matcher(&["node_modules/"]);
        // The directory itself, only when it is a directory.
        assert!(m.is_ignored(Path::new("node_modules"), true));
        assert!(!m.is_ignored(Path::new("node_modules"), false));
        // Anything nested under it, regardless of kind.
        assert!(m.is_ignored(Path::new("node_modules/react/index.js"), false));
        assert!(m.is_ignored(Path::new("node_modules/.bin"), true));
        // Floats to any depth (no interior separator in the pattern).
        assert!(m.is_ignored(Path::new("packages/app/node_modules"), true));
        assert!(m.is_ignored(Path::new("packages/app/node_modules/x.js"), false));
    }

    #[test]
    fn basename_glob_matches_at_any_depth() {
        let m = matcher(&["*.pyc"]);
        assert!(m.is_ignored(Path::new("a.pyc"), false));
        assert!(m.is_ignored(Path::new("pkg/mod/a.pyc"), false));
        assert!(!m.is_ignored(Path::new("a.py"), false));
        // Does not leap across a separator: `*` is component-local.
        assert!(!m.is_ignored(Path::new("a.pyc.bak"), false));
    }

    #[test]
    fn exact_basename_matches_at_any_depth() {
        let m = matcher(&[".DS_Store"]);
        assert!(m.is_ignored(Path::new(".DS_Store"), false));
        assert!(m.is_ignored(Path::new("deep/nested/.DS_Store"), false));
        assert!(!m.is_ignored(Path::new("DS_Store"), false));
    }

    #[test]
    fn anchored_pattern_with_interior_sep_is_root_relative() {
        let m = matcher(&["src/generated/"]);
        assert!(m.is_ignored(Path::new("src/generated"), true));
        assert!(m.is_ignored(Path::new("src/generated/out.rs"), false));
        // Not floated: a same-named dir elsewhere is NOT excluded.
        assert!(!m.is_ignored(Path::new("vendor/src/generated"), true));
    }

    #[test]
    fn plain_pattern_excludes_subtree_when_it_is_a_dir() {
        // A non-trailing-slash pattern that happens to match a directory should
        // still exclude its contents.
        let m = matcher(&["coverage"]);
        assert!(m.is_ignored(Path::new("coverage"), true));
        assert!(m.is_ignored(Path::new("coverage"), false));
        assert!(m.is_ignored(Path::new("coverage/lcov.info"), false));
    }

    #[test]
    fn empty_path_is_never_ignored() {
        let m = matcher(&["*"]);
        assert!(!m.is_ignored(Path::new(""), true));
        assert!(!m.is_ignored(Path::new("."), true));
    }

    #[test]
    fn matching_is_case_sensitive() {
        let m = matcher(&["target/"]);
        assert!(m.is_ignored(Path::new("target"), true));
        assert!(!m.is_ignored(Path::new("Target"), true));
        assert!(!m.is_ignored(Path::new("TARGET"), true));
    }

    #[test]
    fn separators_are_normalized() {
        let m = matcher(&["node_modules/"]);
        // A backslash-separated relative path matches the same as forward slash.
        assert!(m.is_ignored(Path::new("pkg\\node_modules\\x.js"), false));
    }

    #[test]
    fn leading_dot_slash_is_tolerated() {
        let m = matcher(&["dist/"]);
        assert!(m.is_ignored(Path::new("./dist"), true));
        assert!(m.is_ignored(Path::new("./dist/bundle.js"), false));
    }

    #[test]
    fn default_profile_matcher_behaves() {
        let m = IgnoreMatcher::new(&IgnoreProfile::default_profile());
        // Deps and artifacts excluded...
        assert!(m.is_ignored(Path::new("node_modules"), true));
        assert!(m.is_ignored(Path::new(".git"), true));
        assert!(m.is_ignored(Path::new("target"), true));
        assert!(m.is_ignored(Path::new("a/b/target/debug/app"), false));
        assert!(m.is_ignored(Path::new("__pycache__/mod.cpython-311.pyc"), false));
        assert!(m.is_ignored(Path::new("src/util.pyc"), false));
        assert!(m.is_ignored(Path::new("build"), true));
        assert!(m.is_ignored(Path::new("dist"), true));
        assert!(m.is_ignored(Path::new("nested/.DS_Store"), false));
        assert!(m.is_ignored(Path::new("obj/main.o"), false));
        assert!(m.is_ignored(Path::new("Main.class"), false));
        // ...source and lockfiles kept.
        assert!(!m.is_ignored(Path::new("src/main.rs"), false));
        assert!(!m.is_ignored(Path::new("Cargo.lock"), false));
        assert!(!m.is_ignored(Path::new("Cargo.toml"), false));
        assert!(!m.is_ignored(Path::new("package-lock.json"), false));
        assert!(!m.is_ignored(Path::new("README.md"), false));
    }

    #[test]
    fn same_profile_yields_same_decisions() {
        // The predicate is a pure function of the profile.
        let p = IgnoreProfile::default_profile();
        let a = IgnoreMatcher::new(&p);
        let b = IgnoreMatcher::new(&p);
        for (path, is_dir) in [
            ("node_modules", true),
            ("src/main.rs", false),
            ("target/debug/x", false),
            ("a.pyc", false),
        ] {
            assert_eq!(
                a.is_ignored(Path::new(path), is_dir),
                b.is_ignored(Path::new(path), is_dir)
            );
        }
    }

    #[test]
    fn invalid_glob_is_skipped_not_panicked() {
        // An unbalanced character class is invalid; the matcher must still build
        // and simply not apply that pattern.
        let profile = IgnoreProfile::from_patterns(["[unterminated", "*.tmp"]).unwrap();
        let m = IgnoreMatcher::new(&profile);
        assert!(m.is_ignored(Path::new("a.tmp"), false));
        // The invalid pattern contributes nothing, so an arbitrary path is kept.
        assert!(!m.is_ignored(Path::new("whatever"), false));
    }
}
