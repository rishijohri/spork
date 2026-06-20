//! Lineage-hash computation (DESIGN §6.3, A.1).
//!
//! Every node carries a `lineage_hash`: a content hash that folds the parents'
//! lineage into the node's own identity. It is used for handoff dedup and as a
//! cache key (DESIGN §6.3 "Edges, acyclicity, and lineage"; A.1 "lineageHash for
//! handoff dedup / cache keys"). The hash is computed over the **canonical
//! bytes** of a small JSON document so it is byte-stable across machines and
//! reproducible on replay — the same encoder (`spork-canon`) the whole substrate
//! hashes through.
//!
//! # Definition (frozen for the F2 generation)
//!
//! `lineage_hash = BLAKE3(canon({ "parents": [<parent lineage hashes, in the
//! order given>], "id": <node id>, "kind": <node kind> }))`.
//!
//! Including the parents' *lineage* hashes (not just their ids) makes the value
//! transitively reflect the whole ancestry: changing any ancestor's identity
//! changes every descendant's lineage hash. The node's own `id` (a unique ULID)
//! guarantees distinct nodes never collide even with identical parents and kind.

use serde_json::json;
use spork_hash::Hash;
use ulid::Ulid;

use crate::error::Result;

/// Compute a node's lineage hash from its parents' lineage hashes and identity.
///
/// `parent_lineage_hashes` are the `lineage_hash` values of the node's direct
/// parents, **in the order the parents are listed** on the node (order is part
/// of the identity, matching the stored `parent_ids` order). `id` and `kind` are
/// the new node's identity.
///
/// # Errors
///
/// Returns [`GraphError::Canon`](crate::GraphError::Canon) if the document
/// cannot be canonicalized — which cannot happen for this fixed, float-free
/// shape, but the fallible signature keeps the canonical-encoder contract
/// explicit at the call site.
pub fn lineage_hash(parent_lineage_hashes: &[Hash], id: Ulid, kind: &str) -> Result<Hash> {
    let parents: Vec<String> = parent_lineage_hashes.iter().map(Hash::to_hex).collect();
    let doc = json!({
        "parents": parents,
        "id": id.to_string(),
        "kind": kind,
    });
    let bytes = spork_canon::canonicalize_value(&doc)?;
    Ok(spork_hash::hash_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_order_sensitive() {
        let id = Ulid::new();
        let p1 = spork_hash::hash_bytes(b"p1");
        let p2 = spork_hash::hash_bytes(b"p2");
        assert_eq!(
            lineage_hash(&[p1, p2], id, "k").unwrap(),
            lineage_hash(&[p1, p2], id, "k").unwrap()
        );
        // Parent order is part of the identity.
        assert_ne!(
            lineage_hash(&[p1, p2], id, "k").unwrap(),
            lineage_hash(&[p2, p1], id, "k").unwrap()
        );
        // Kind is part of the identity.
        assert_ne!(
            lineage_hash(&[p1], id, "a").unwrap(),
            lineage_hash(&[p1], id, "b").unwrap()
        );
    }

    #[test]
    fn distinct_ids_never_collide_even_with_identical_parents() {
        let p = spork_hash::hash_bytes(b"p");
        let a = lineage_hash(&[p], Ulid::new(), "k").unwrap();
        let b = lineage_hash(&[p], Ulid::new(), "k").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn empty_parents_is_a_function_of_id_and_kind() {
        let id = Ulid::new();
        assert_eq!(
            lineage_hash(&[], id, "k").unwrap(),
            lineage_hash(&[], id, "k").unwrap()
        );
    }
}
