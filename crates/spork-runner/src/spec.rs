//! The check request: [`CheckSpec`].
//!
//! A `CheckSpec` is the reusable, version-controlled *what/how to run* of a
//! check, separate from any one execution of it (DESIGN.md §8.1). It names a
//! [`kind`](CheckSpec::kind) (which selects a [`Runner`](crate::Runner)
//! adapter), carries an opaque-to-the-core [`config`](CheckSpec::config) JSON
//! payload the chosen runner interprets, and declares whether the check is
//! [`impure`](CheckSpec::impure).
//!
//! The `impure` flag is the cache-soundness lever (DESIGN.md §9.2): a check that
//! reads wall-clock, network, or randomness cannot be cached without poisoning
//! it, so it self-declares `impure = true` to opt out of the derivation cache
//! and always re-run. Deterministic checks (`impure = false`) participate in the
//! content-addressed cache so an identical `(spec, input_tree, runner_version)`
//! re-run is a hit and is not re-executed.
//!
//! The struct carries its own [`schema_version`](CheckSpec::schema_version)
//! (constraint C5). Both the `kind` and the *canonicalized* `config` feed the
//! [`input_digest`](crate::input_digest), so two specs that differ in config are
//! distinct cache entries while two specs whose config differs only in JSON key
//! order or whitespace collapse to the same entry.
//!
//! Design references: DESIGN.md §8.1 (`CheckSpec` vs `CheckNode`, one Runner
//! SPI), §9.2 (impure self-declaration opts out of caching), §4.4
//! (`inputDigest` / `derivationKey`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The current schema version stamped on a freshly constructed [`CheckSpec`].
pub const CHECK_SPEC_VERSION: u16 = 1;

/// A reusable, version-controlled description of a check to run.
///
/// See the [module docs](crate::spec) for the full rationale.
///
/// # Example
/// ```
/// use spork_runner::CheckSpec;
/// use serde_json::json;
///
/// // A deterministic sanity check that forbids the pattern `FIXME`.
/// let spec = CheckSpec::new("sanity", json!({ "forbid": ["FIXME"] }));
/// assert!(!spec.impure); // deterministic => cacheable
///
/// // A check that reads the clock declares itself impure (never cached).
/// let clock = CheckSpec::new("custom-clock", json!({})).impure();
/// assert!(clock.impure);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    /// The schema version of this spec ([`CHECK_SPEC_VERSION`] for fresh
    /// values).
    pub schema_version: u16,
    /// The check kind, selecting which [`Runner`](crate::Runner) adapter handles
    /// it (e.g. `"sanity"`, `"test"`, `"stress"`). The core never branches on
    /// this beyond dispatch.
    pub kind: String,
    /// An opaque-to-the-core JSON config the chosen runner interprets. Its
    /// *canonical* encoding feeds the cache key, so key order and whitespace do
    /// not create spurious cache misses.
    pub config: Value,
    /// Whether this check is non-deterministic and must therefore never be
    /// cached (DESIGN.md §9.2). Deterministic checks leave this `false` and
    /// participate in the content-addressed derivation cache.
    pub impure: bool,
}

impl CheckSpec {
    /// Construct a deterministic (cacheable) spec for `kind` with `config`.
    ///
    /// Stamps the current [`CHECK_SPEC_VERSION`] and leaves `impure = false`;
    /// mark a non-deterministic check with [`impure`](CheckSpec::impure).
    #[must_use]
    pub fn new(kind: impl Into<String>, config: Value) -> Self {
        CheckSpec {
            schema_version: CHECK_SPEC_VERSION,
            kind: kind.into(),
            config,
            impure: false,
        }
    }

    /// Mark this spec as impure (non-deterministic), opting it out of caching
    /// (builder style).
    #[must_use]
    pub fn impure(mut self) -> Self {
        self.impure = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_stamps_version_and_is_pure() {
        let spec = CheckSpec::new("sanity", json!({ "forbid": ["FIXME"] }));
        assert_eq!(spec.schema_version, CHECK_SPEC_VERSION);
        assert_eq!(spec.kind, "sanity");
        assert!(!spec.impure);
    }

    #[test]
    fn impure_builder_sets_flag() {
        let spec = CheckSpec::new("custom", json!({})).impure();
        assert!(spec.impure);
    }

    #[test]
    fn round_trips_through_serde() {
        let spec = CheckSpec::new("sanity", json!({ "max_line_length": 100 })).impure();
        let s = serde_json::to_string(&spec).unwrap();
        let back: CheckSpec = serde_json::from_str(&s).unwrap();
        assert_eq!(spec, back);
    }
}
