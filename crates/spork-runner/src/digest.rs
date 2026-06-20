//! The content-addressed cache key: [`input_digest`] and [`DerivationKey`].
//!
//! A check result is a *replayable derivation*: re-running the same check over
//! the same inputs with the same runner must be either a cache hit (skipped) or
//! a byte-identical recomputation (DESIGN.md §9.2). The key that makes that
//! sound is the **input digest** — a deterministic hash of everything a result
//! depends on:
//!
//! ```text
//! input_digest = blake3(canon(parent_content_ref ++ canonical(config)
//!                             ++ runner_version ++ change_scope))
//! ```
//!
//! (DESIGN.md §4.4, §8.1). The cache-key soundness subtlety (DESIGN.md §8.1) is
//! real: a check must honestly declare the inputs it reads, or under-declaration
//! yields a stale green. The `parent_content_ref` is the content hash of the
//! materialized snapshot the worktree was built from. A *change-scoped* run does
//! not read that whole tree — it reads only the changed subset — so the scope is
//! part of the input the check actually reads and **must** be in the key: two
//! runs over the same snapshot but a different change scope are different
//! derivations and must not collide (otherwise a narrow change-scoped pass would
//! be a stale hit for a full scan, the DESIGN.md §8.1 under-declaration hazard).
//! The [`ChangeScope`] folds the scope in so the declared input and the read
//! input are the same thing by construction.
//!
//! ## The derivation-key generation (no cache poisoning)
//!
//! The *formula* that turns inputs into a result can itself change (a runner
//! fixes a bug in how it scans, changing what a green means). If the new formula
//! reused the old keys, every old cached result would silently become wrong. So
//! every input digest is paired with a [`DerivationKey`] that stamps a
//! generation: [`DerivationKey::generation`]. A formula change bumps the
//! generation, which changes the derivation key for the *same* inputs, so new
//! runs land in a fresh cache generation and **never poison or read the old
//! one** — the old generation's entries remain valid for anyone still on the old
//! formula (DESIGN.md §9.2, replayable derivations + cache soundness).
//!
//! Design references: DESIGN.md §4.4 (`inputDigest` / `derivationKey`), §8.1
//! (cache-key soundness), §9.2 (results are content-addressed replayable
//! derivations; impure opts out).

use serde::{Deserialize, Serialize};

use spork_canon::{canonicalize, canonicalize_value};
use spork_hash::{hash_bytes, Hash};

use crate::error::Result;
use crate::spec::CheckSpec;

/// The current generation of every shipped runner's derivation formula.
///
/// Bump the *per-runner* generation in [`RunnerCapabilities`](crate::RunnerCapabilities)
/// when a runner's normalization formula changes; this constant is the default
/// starting generation for a freshly authored runner.
pub const DERIVATION_GENERATION_V1: u32 = 1;

/// The set of paths a change-scoped run actually reads, folded into the
/// [`input_digest`] so the scope is part of the cache key.
///
/// A check that runs over the *whole* materialized tree and a check that runs
/// over only the paths an edit touched are different derivations even when the
/// parent snapshot is identical: the change-scoped run reads less, so it could
/// legitimately produce a different result (fewer scanned files, fewer
/// violations). Keying both off the same digest would let a narrow change-scoped
/// pass be served as a stale hit for a full scan — the under-declared-input
/// hazard of DESIGN.md §8.1. This type makes the scope explicit in the key:
///
/// - [`ChangeScope::Full`] — no change scope; the check reads the whole tree.
/// - [`ChangeScope::Paths`] — the check reads only these paths (deduplicated and
///   sorted, so the key is order-insensitive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeScope {
    /// The check reads the whole materialized tree (no change scope).
    Full,
    /// The check reads only this deduplicated, sorted set of paths.
    Paths(Vec<String>),
}

impl ChangeScope {
    /// Build a change scope from a raw list of changed paths.
    ///
    /// An empty list means "scan everything" ([`ChangeScope::Full`]); a non-empty
    /// list is normalized (deduplicated and sorted) so two runs with the same
    /// scope in a different order produce the same key.
    #[must_use]
    pub fn from_paths(paths: &[String]) -> Self {
        if paths.is_empty() {
            ChangeScope::Full
        } else {
            let mut sorted: Vec<String> = paths.to_vec();
            sorted.sort();
            sorted.dedup();
            ChangeScope::Paths(sorted)
        }
    }
}

/// Compute the cache `input_digest` for a check.
///
/// `parent_content_ref` is the content hash of the materialized snapshot the
/// worktree was built from. `spec` contributes its `kind` and its **canonical**
/// config encoding (so config key order / whitespace do not create spurious
/// misses). `runner_version` identifies the runner formula (matching the
/// `(checkSpecId@version, inputTreeHash, runnerImageDigest)` tuple of DESIGN.md
/// §8.1). `scope` records *which subset of the tree the check actually reads* —
/// [`ChangeScope::Full`] for a whole-tree scan or [`ChangeScope::Paths`] for a
/// change-scoped run — so a narrow change-scoped run and a full scan over the
/// same snapshot are distinct cache entries (DESIGN.md §8.1, under-declaration
/// hazard).
///
/// The digest is `blake3(canon(domain ++ parent ++ kind ++ canonical(config) ++
/// runner_version ++ scope))`, built from a canonical struct so it is byte-stable
/// on every machine.
///
/// # Errors
/// [`RunnerError::Canon`](crate::RunnerError::Canon) if the config cannot be
/// canonicalized (only possible if a float entered it).
pub fn input_digest(
    parent_content_ref: &Hash,
    spec: &CheckSpec,
    runner_version: &str,
    scope: &ChangeScope,
) -> Result<Hash> {
    // Canonicalize the config to its own byte string first, then hash *that*
    // into the key. Hashing the canonical config bytes (rather than embedding
    // the raw `Value`) makes the dependency on config explicit and keeps the
    // outer canonical struct float-free regardless of the config's contents
    // (the config is reduced to an opaque hash).
    let config_bytes = canonicalize_value(&spec.config)?;
    let config_hash = hash_bytes(&config_bytes);

    #[derive(Serialize)]
    struct DigestInputs<'a> {
        /// Domain separation so this hash can never collide with another
        /// canonical-struct hash in the workspace.
        domain: &'a str,
        parent_content_ref: &'a Hash,
        kind: &'a str,
        config_hash: Hash,
        runner_version: &'a str,
        scope: &'a ChangeScope,
    }

    let inputs = DigestInputs {
        domain: "spork.runner.input_digest.v1",
        parent_content_ref,
        kind: &spec.kind,
        config_hash,
        runner_version,
        scope,
    };
    let bytes = canonicalize(&inputs)?;
    Ok(hash_bytes(&bytes))
}

/// The full content-addressed key a cached result is stored under: an
/// [`input_digest`] paired with a formula [`generation`](DerivationKey::generation).
///
/// Storing the generation *in the key* is what makes a formula change additive
/// rather than destructive: bumping the generation changes the
/// [`storage_key`](DerivationKey::storage_key) for the same inputs, so a new
/// formula writes to a fresh cache generation and never reads or overwrites the
/// old one (DESIGN.md §9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivationKey {
    /// The schema version of this key record ([`DERIVATION_KEY_VERSION`] for
    /// fresh values).
    pub schema_version: u16,
    /// The input digest (hash of input tree + config + runner version).
    pub input_digest: Hash,
    /// The derivation-formula generation. A formula change bumps this so old and
    /// new results live in distinct cache generations.
    pub generation: u32,
}

/// The current schema version stamped on a freshly constructed [`DerivationKey`].
pub const DERIVATION_KEY_VERSION: u16 = 1;

impl DerivationKey {
    /// Construct a derivation key from an input digest and a formula generation.
    #[must_use]
    pub fn new(input_digest: Hash, generation: u32) -> Self {
        DerivationKey {
            schema_version: DERIVATION_KEY_VERSION,
            input_digest,
            generation,
        }
    }

    /// Compute the content-addressed storage key this derivation is filed under.
    ///
    /// This folds the input digest *and* the generation (and a domain tag) into
    /// a single hash, so two derivations that share an input digest but differ
    /// in generation land at distinct storage keys — the no-poisoning property.
    ///
    /// # Errors
    /// [`RunnerError::Canon`](crate::RunnerError::Canon) if canonicalization
    /// fails (not possible for this integer-only struct in practice).
    pub fn storage_key(&self) -> Result<Hash> {
        let bytes = canonicalize(self)?;
        Ok(hash_bytes(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parent(b: u8) -> Hash {
        Hash::from_bytes([b; 32])
    }

    #[test]
    fn identical_inputs_yield_identical_digest() {
        let p = parent(1);
        let spec = CheckSpec::new("sanity", json!({ "forbid": ["FIXME"] }));
        let a = input_digest(&p, &spec, "sanity@1", &ChangeScope::Full).unwrap();
        let b = input_digest(&p, &spec, "sanity@1", &ChangeScope::Full).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn config_is_order_insensitive() {
        // The same config in two key orders must produce the same digest
        // (canonicalization), so cosmetic config edits do not bust the cache.
        let p = parent(1);
        let spec_a = CheckSpec::new("sanity", json!({ "a": 1, "b": 2 }));
        let spec_b = CheckSpec::new("sanity", json!({ "b": 2, "a": 1 }));
        assert_eq!(
            input_digest(&p, &spec_a, "v", &ChangeScope::Full).unwrap(),
            input_digest(&p, &spec_b, "v", &ChangeScope::Full).unwrap()
        );
    }

    #[test]
    fn different_parent_tree_changes_digest() {
        let spec = CheckSpec::new("sanity", json!({}));
        assert_ne!(
            input_digest(&parent(1), &spec, "v", &ChangeScope::Full).unwrap(),
            input_digest(&parent(2), &spec, "v", &ChangeScope::Full).unwrap()
        );
    }

    #[test]
    fn different_config_changes_digest() {
        let p = parent(1);
        let a = CheckSpec::new("sanity", json!({ "forbid": ["FIXME"] }));
        let b = CheckSpec::new("sanity", json!({ "forbid": ["TODO"] }));
        assert_ne!(
            input_digest(&p, &a, "v", &ChangeScope::Full).unwrap(),
            input_digest(&p, &b, "v", &ChangeScope::Full).unwrap()
        );
    }

    #[test]
    fn different_kind_changes_digest() {
        let p = parent(1);
        let a = CheckSpec::new("sanity", json!({}));
        let b = CheckSpec::new("test", json!({}));
        assert_ne!(
            input_digest(&p, &a, "v", &ChangeScope::Full).unwrap(),
            input_digest(&p, &b, "v", &ChangeScope::Full).unwrap()
        );
    }

    #[test]
    fn different_runner_version_changes_digest() {
        let p = parent(1);
        let spec = CheckSpec::new("sanity", json!({}));
        assert_ne!(
            input_digest(&p, &spec, "sanity@1", &ChangeScope::Full).unwrap(),
            input_digest(&p, &spec, "sanity@2", &ChangeScope::Full).unwrap()
        );
    }

    #[test]
    fn change_scope_changes_digest() {
        // The crux soundness property: a full scan and a change-scoped run over
        // the SAME snapshot are distinct cache entries, so a narrow change-scoped
        // pass can never be served as a stale hit for a full scan (DESIGN.md §8.1
        // under-declaration hazard).
        let p = parent(1);
        let spec = CheckSpec::new("sanity", json!({ "forbid": ["FIXME"] }));
        let full = input_digest(&p, &spec, "v", &ChangeScope::Full).unwrap();
        let scoped = input_digest(
            &p,
            &spec,
            "v",
            &ChangeScope::from_paths(&["src/a.rs".to_string()]),
        )
        .unwrap();
        assert_ne!(full, scoped);

        // A different scoped set is again distinct.
        let scoped_b = input_digest(
            &p,
            &spec,
            "v",
            &ChangeScope::from_paths(&["src/b.rs".to_string()]),
        )
        .unwrap();
        assert_ne!(scoped, scoped_b);
    }

    #[test]
    fn change_scope_is_order_insensitive() {
        // The same scope in a different order (or with duplicates) is the same
        // derivation: scope is normalized before it enters the key.
        let p = parent(1);
        let spec = CheckSpec::new("sanity", json!({}));
        let a = input_digest(
            &p,
            &spec,
            "v",
            &ChangeScope::from_paths(&["b.rs".to_string(), "a.rs".to_string()]),
        )
        .unwrap();
        let b = input_digest(
            &p,
            &spec,
            "v",
            &ChangeScope::from_paths(&["a.rs".to_string(), "b.rs".to_string(), "a.rs".to_string()]),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_scope_is_full() {
        assert_eq!(ChangeScope::from_paths(&[]), ChangeScope::Full);
    }

    #[test]
    fn generation_bump_changes_storage_key_for_same_inputs() {
        // The no-poisoning property: same input digest, different generation =>
        // different storage key, so a formula change opens a new generation.
        let digest = parent(9);
        let gen1 = DerivationKey::new(digest, 1).storage_key().unwrap();
        let gen2 = DerivationKey::new(digest, 2).storage_key().unwrap();
        assert_ne!(gen1, gen2);
    }

    #[test]
    fn same_key_yields_same_storage_key() {
        let digest = parent(9);
        let a = DerivationKey::new(digest, 1).storage_key().unwrap();
        let b = DerivationKey::new(digest, 1).storage_key().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn derivation_key_round_trips() {
        let k = DerivationKey::new(parent(3), 5);
        let json = serde_json::to_string(&k).unwrap();
        let back: DerivationKey = serde_json::from_str(&json).unwrap();
        assert_eq!(k, back);
        assert_eq!(back.schema_version, DERIVATION_KEY_VERSION);
    }
}
