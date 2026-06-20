//! Property tests for the frozen ignore-profile invariants.
//!
//! These exercise the identity and matching guarantees over many random inputs,
//! complementing the example-based unit tests in `src/`:
//!
//! 1. **Identity is order/duplicate invariant.** Two profiles built from the
//!    same *set* of patterns hash identically regardless of input order or
//!    repetition — the property snapshot identity (`ignore_profile_hash`) leans
//!    on (DESIGN.md §6.1, §10.5).
//! 2. **Identity is injective over distinct sets.** Two profiles whose
//!    normalized pattern sets differ have different canonical bytes (and so,
//!    barring a BLAKE3 collision, different hashes).
//! 3. **Matching is a pure function of the profile** and stable across
//!    construction order.
//! 4. **A directory pattern always excludes its own subtree.**

use proptest::collection::vec;
use proptest::prelude::*;
use spork_ignore::{IgnoreMatcher, IgnoreProfile};
use std::path::Path;

/// A strategy producing simple, well-formed pattern fragments (a single path
/// component of lowercase letters/digits, length 1..=8). Kept deliberately
/// glob-free so the focus is on identity/normalization, not glob parsing.
fn component() -> impl Strategy<Value = String> {
    "[a-z0-9]{1,8}"
}

/// A strategy producing a directory pattern like `name/`.
fn dir_pattern() -> impl Strategy<Value = String> {
    component().prop_map(|c| format!("{c}/"))
}

proptest! {
    /// Shuffling and duplicating the pattern list never changes identity.
    #[test]
    fn identity_is_order_and_duplicate_invariant(
        mut pats in vec(dir_pattern(), 1..12),
        rotate in 0usize..12,
    ) {
        let base = IgnoreProfile::from_patterns(pats.clone()).unwrap();

        // Rotate (reorder) and duplicate every entry.
        if !pats.is_empty() {
            let r = rotate % pats.len();
            pats.rotate_left(r);
        }
        let mut with_dupes = pats.clone();
        with_dupes.extend(pats.iter().cloned());

        let permuted = IgnoreProfile::from_patterns(with_dupes).unwrap();

        prop_assert_eq!(base.patterns(), permuted.patterns());
        prop_assert_eq!(base.canonical_bytes().unwrap(), permuted.canonical_bytes().unwrap());
        prop_assert_eq!(base.hash(), permuted.hash());
    }

    /// Distinct normalized pattern sets produce distinct canonical bytes.
    #[test]
    fn distinct_sets_have_distinct_bytes(
        a in vec(dir_pattern(), 1..8),
        b in vec(dir_pattern(), 1..8),
    ) {
        let pa = IgnoreProfile::from_patterns(a).unwrap();
        let pb = IgnoreProfile::from_patterns(b).unwrap();
        if pa.patterns() == pb.patterns() {
            prop_assert_eq!(pa.canonical_bytes().unwrap(), pb.canonical_bytes().unwrap());
            prop_assert_eq!(pa.hash(), pb.hash());
        } else {
            prop_assert_ne!(pa.canonical_bytes().unwrap(), pb.canonical_bytes().unwrap());
        }
    }

    /// The matcher is a pure function of the profile: rebuilding it yields the
    /// same decision for any path.
    #[test]
    fn matcher_is_deterministic(
        pats in vec(dir_pattern(), 1..10),
        comps in vec(component(), 1..5),
        is_dir in any::<bool>(),
    ) {
        let profile = IgnoreProfile::from_patterns(pats).unwrap();
        let m1 = IgnoreMatcher::new(&profile);
        let m2 = IgnoreMatcher::new(&profile);
        let path_str = comps.join("/");
        let path = Path::new(&path_str);
        prop_assert_eq!(m1.is_ignored(path, is_dir), m2.is_ignored(path, is_dir));
    }

    /// A directory pattern `name/` excludes the directory entry (when it is a
    /// directory) and every path nested beneath it, at any depth.
    #[test]
    fn directory_pattern_excludes_its_subtree(
        name in component(),
        prefix in vec(component(), 0..3),
        suffix in vec(component(), 1..4),
    ) {
        let profile = IgnoreProfile::from_patterns([format!("{name}/")]).unwrap();
        let m = IgnoreMatcher::new(&profile);

        // The directory itself (floated to any depth) is ignored as a dir.
        let mut dir_parts = prefix.clone();
        dir_parts.push(name.clone());
        let dir_path = dir_parts.join("/");
        prop_assert!(m.is_ignored(Path::new(&dir_path), true));

        // Anything beneath it is ignored regardless of kind.
        let mut nested_parts = dir_parts.clone();
        nested_parts.extend(suffix);
        let nested_path = nested_parts.join("/");
        prop_assert!(m.is_ignored(Path::new(&nested_path), false));
    }
}
