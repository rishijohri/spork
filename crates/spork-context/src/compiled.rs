//! The compiled-context artifact [`CompiledContext`] and the `prefix_hash`.
//!
//! A [`CompiledContext`] is the output of a compile: the ordered [`ContextLayer`]
//! list (stable→volatile), the `prefix_hash` over the stable prefix, and the
//! [`SelectionDecision`] audit trace. The `prefix_hash` is the cache-reuse key
//! of DESIGN.md §13.2 — it is computed over the canonical bytes of *only* the
//! stable-prefix layers (ranks 0..=`[PREFIX_MAX_RANK]`), so:
//!
//! - two sibling compilations with the **same** stable prefix produce the
//!   **same** `prefix_hash` (the warm provider cache is reused), and
//! - changing only the **volatile tail** (current diff, tool results, the latest
//!   user message) does **not** change the `prefix_hash`.
//!
//! Because the hash folds the layers' canonical bytes — which include the
//! `kind`, `content`, `source_ref`, and `token_estimate` of each prefix layer in
//! emitted order — it captures exactly the prefix the provider will cache, and
//! nothing else.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;

use crate::error::ContextError;
use crate::layer::{ContextLayer, PREFIX_MAX_RANK};
use crate::trace::SelectionDecision;

/// The frozen schema version of the [`CompiledContext`] shape (CLAUDE.md C5).
pub const COMPILED_CONTEXT_SCHEMA_VERSION: u16 = 1;

/// The output of a context compile.
///
/// The `layers` are ordered stable→volatile by [`crate::volatility_rank`]; the
/// `prefix_hash` keys the stable prefix for warm-cache reuse (DESIGN.md §13.2);
/// the `selection_trace` explains every inclusion / summary / drop for the
/// auditable "expand context" panel (DESIGN.md §13.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledContext {
    /// The schema version of this compiled-context shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// The assembled layers, in ascending volatility rank (stable first).
    pub layers: Vec<ContextLayer>,
    /// The content hash of the stable prefix (layers of rank
    /// `0..=`[`PREFIX_MAX_RANK`]). Identical across siblings/turns whose stable
    /// prefix matches; unaffected by the volatile tail (DESIGN.md §13.2).
    pub prefix_hash: Hash,
    /// The audit trace: why each layer / ancestor was included, summarized, or
    /// dropped (DESIGN.md §13.6).
    pub selection_trace: Vec<SelectionDecision>,
}

impl CompiledContext {
    /// Assemble a [`CompiledContext`] from already-ordered `layers` and a
    /// `selection_trace`, computing the `prefix_hash` over the stable prefix.
    ///
    /// The caller is responsible for emitting `layers` in stable→volatile order;
    /// [`prefix_hash_of`] folds the contiguous stable-prefix layers (rank
    /// `<=`[`PREFIX_MAX_RANK`]) at the front of `layers`.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::Canon`] if a prefix layer cannot be
    /// canonicalized (which cannot happen for the float-free layer shape, but the
    /// fallible signature keeps the canonical-encoder contract explicit).
    pub fn assemble(
        layers: Vec<ContextLayer>,
        selection_trace: Vec<SelectionDecision>,
    ) -> Result<Self, ContextError> {
        let prefix_hash = prefix_hash_of(&layers)?;
        Ok(CompiledContext {
            schema_version: COMPILED_CONTEXT_SCHEMA_VERSION,
            layers,
            prefix_hash,
            selection_trace,
        })
    }

    /// The stable-prefix layers (rank `<=`[`PREFIX_MAX_RANK`]) of this context.
    ///
    /// These are exactly the layers folded into [`prefix_hash`](CompiledContext::prefix_hash).
    #[must_use]
    pub fn stable_prefix(&self) -> &[ContextLayer] {
        let n = stable_prefix_len(&self.layers);
        &self.layers[..n]
    }

    /// The volatile tail (rank `>`[`PREFIX_MAX_RANK`]) of this context.
    ///
    /// These layers are deliberately excluded from the `prefix_hash` so a new
    /// turn reuses the warm cache (DESIGN.md §13.2).
    #[must_use]
    pub fn volatile_tail(&self) -> &[ContextLayer] {
        let n = stable_prefix_len(&self.layers);
        &self.layers[n..]
    }

    /// The total token estimate across all layers (prefix + tail).
    #[must_use]
    pub fn total_token_estimate(&self) -> u64 {
        self.layers.iter().map(|l| l.token_estimate).sum()
    }
}

/// The number of leading layers that belong to the stable prefix.
///
/// Because the compiler emits layers in ascending rank, the stable-prefix layers
/// are a contiguous block at the front; this counts them.
fn stable_prefix_len(layers: &[ContextLayer]) -> usize {
    layers.iter().take_while(|l| l.is_stable_prefix()).count()
}

/// Compute the `prefix_hash` over the stable prefix of an ordered layer list.
///
/// The hash is BLAKE3 over the **canonical bytes** of a small, schema-versioned
/// document containing exactly the stable-prefix layers (rank
/// `<=`[`PREFIX_MAX_RANK`]) in emitted order. Using the canonical encoder makes
/// the value byte-stable across machines and reproducible on replay (the same
/// encoder the whole substrate hashes through, DESIGN.md §6.1). Excluding the
/// volatile tail is what makes the hash reusable across turns (DESIGN.md §13.2).
///
/// # Errors
///
/// Returns [`ContextError::Canon`] only if a layer cannot be canonicalized.
pub fn prefix_hash_of(layers: &[ContextLayer]) -> Result<Hash, ContextError> {
    let n = stable_prefix_len(layers);
    let prefix = &layers[..n];
    // A schema-versioned wrapper so the hashed bytes are self-describing and the
    // hash domain is distinct from any other canonical document (CLAUDE.md C5).
    let doc = PrefixHashDoc {
        schema_version: PREFIX_HASH_SCHEMA_VERSION,
        prefix_max_rank: PREFIX_MAX_RANK,
        layers: prefix,
    };
    let bytes = spork_canon::canonicalize(&doc)?;
    Ok(spork_hash::hash_bytes(&bytes))
}

/// The frozen schema version of the `prefix_hash` input document (CLAUDE.md C5).
///
/// This is part of the hashed bytes, so the prefix-hash *formula* is itself
/// versioned: a future change to what the prefix hash folds is a new generation
/// that opens fresh caches, never an in-place re-key of existing warm caches
/// (DESIGN.md §13.2, A.7).
pub const PREFIX_HASH_SCHEMA_VERSION: u16 = 1;

/// The exact, self-describing document the `prefix_hash` is computed over.
///
/// Serializing through the canonical encoder yields the byte-stable identity of
/// the stable prefix. `prefix_max_rank` is folded in so the prefix-cutoff
/// convention is part of the hash's identity.
#[derive(Serialize)]
struct PrefixHashDoc<'a> {
    schema_version: u16,
    prefix_max_rank: u8,
    layers: &'a [ContextLayer],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{ContextLayer, ContextLayerKind};
    use crate::trace::{Disposition, SelectionDecision};

    fn sys(content: &str) -> ContextLayer {
        ContextLayer::new(ContextLayerKind::System, content, "sys")
    }
    fn repo(content: &str) -> ContextLayer {
        ContextLayer::new(ContextLayerKind::RepoMap, content, "repo")
    }
    fn user(content: &str) -> ContextLayer {
        ContextLayer::new(ContextLayerKind::UserMsg, content, "user")
    }
    fn diff(content: &str) -> ContextLayer {
        ContextLayer::new(ContextLayerKind::CurrentDiff, content, "diff")
    }

    #[test]
    fn schema_versions_are_frozen_at_one() {
        assert_eq!(COMPILED_CONTEXT_SCHEMA_VERSION, 1);
        assert_eq!(PREFIX_HASH_SCHEMA_VERSION, 1);
    }

    #[test]
    fn prefix_hash_ignores_volatile_tail() {
        // Same stable prefix, different volatile tails -> identical prefix_hash.
        let a = vec![sys("S"), repo("R"), diff("diff A"), user("hi A")];
        let b = vec![sys("S"), repo("R"), diff("totally different"), user("hi B")];
        assert_eq!(prefix_hash_of(&a).unwrap(), prefix_hash_of(&b).unwrap());
    }

    #[test]
    fn prefix_hash_changes_with_stable_prefix() {
        let a = vec![sys("S"), repo("R"), user("hi")];
        let b = vec![sys("S DIFFERENT"), repo("R"), user("hi")];
        assert_ne!(prefix_hash_of(&a).unwrap(), prefix_hash_of(&b).unwrap());
    }

    #[test]
    fn prefix_hash_is_order_sensitive_within_prefix() {
        // Two stable layers in different order are a different prefix.
        let pm = ContextLayer::new(ContextLayerKind::ProjectMemory, "M", "pm");
        let a = vec![sys("S"), pm.clone()];
        let b = vec![pm, sys("S")];
        assert_ne!(prefix_hash_of(&a).unwrap(), prefix_hash_of(&b).unwrap());
    }

    #[test]
    fn empty_prefix_hashes_stably() {
        // No stable layers at all (only a volatile tail) still hashes (the empty
        // prefix), and identically for any tail.
        let a = vec![user("a")];
        let b = vec![user("b"), diff("d")];
        assert_eq!(prefix_hash_of(&a).unwrap(), prefix_hash_of(&b).unwrap());
    }

    #[test]
    fn stable_prefix_and_tail_partition_layers() {
        let layers = vec![sys("S"), repo("R"), diff("D"), user("U")];
        let cc = CompiledContext::assemble(layers, vec![]).unwrap();
        assert_eq!(cc.stable_prefix().len(), 2);
        assert_eq!(cc.volatile_tail().len(), 2);
        assert_eq!(cc.stable_prefix()[0].kind, ContextLayerKind::System);
        assert_eq!(cc.volatile_tail()[0].kind, ContextLayerKind::CurrentDiff);
    }

    #[test]
    fn assemble_matches_standalone_prefix_hash() {
        let layers = vec![sys("S"), repo("R"), user("U")];
        let expect = prefix_hash_of(&layers).unwrap();
        let cc = CompiledContext::assemble(layers, vec![]).unwrap();
        assert_eq!(cc.prefix_hash, expect);
    }

    #[test]
    fn compiled_context_round_trips_through_serde() {
        let layers = vec![sys("S"), repo("R"), user("U")];
        let trace = vec![SelectionDecision::new(
            ContextLayerKind::System,
            "sys",
            Disposition::Included,
            "system prompt",
            1,
        )];
        let cc = CompiledContext::assemble(layers, trace).unwrap();
        let v = serde_json::to_value(&cc).unwrap();
        let back: CompiledContext = serde_json::from_value(v).unwrap();
        assert_eq!(cc, back);
    }

    #[test]
    fn total_token_estimate_sums_all_layers() {
        let layers = vec![sys("aaaa"), user("bbbbbbbb")];
        let cc = CompiledContext::assemble(layers, vec![]).unwrap();
        // "aaaa" -> ceil(4/4)=1, "bbbbbbbb" -> ceil(8/4)=2.
        assert_eq!(cc.total_token_estimate(), 3);
    }
}
