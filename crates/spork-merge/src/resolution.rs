//! The [`ConflictResolution`] payload stored on a Merge node.
//!
//! A Merge is an explicit, recorded operation: a standard three-way file merge
//! against the nearest common ancestor snapshot, producing a new Edit node with
//! multiple parents and a **stored conflict-resolution payload** (DESIGN.md
//! §6.5). A Merge node is only ever created once every conflict has a stored
//! resolution, so the node is always in a clean, materializable state — which is
//! what preserves "click a node, see exact state" (DESIGN.md A.4).
//!
//! This module freezes that payload's *shape* so the P5 Merge node and the P9
//! graft persist and replay it behind a versioned, byte-stable schema (CLAUDE.md
//! C5). The actual three-way merge computation is P5; F4 freezes only the record
//! of its outcome.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;

use crate::MergeSchemaError;

/// The frozen schema version of [`ConflictResolution`] (CLAUDE.md C5).
///
/// Persisted and hashed alongside the payload, so the shape can evolve
/// additively in a later generation without silently reinterpreting an existing
/// stored Merge node (the no-domino seam, DESIGN.md A.7).
pub const CONFLICT_RESOLUTION_SCHEMA_VERSION: u16 = 1;

/// How a single conflicting file was resolved in a three-way merge.
///
/// A Merge is computed against the nearest common ancestor; each path that
/// could not be auto-merged is recorded here with the human/agent decision that
/// settled it. `Merged` carries a content-addressed reference to the resolved
/// file bytes (DESIGN.md §6.5, §6.1) rather than inlining them, so the payload
/// stays small and the resolved content dedups in the CAS like any other blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "choice")]
pub enum ResolutionChoice {
    /// Keep our side of the conflict verbatim.
    Ours,
    /// Take their side of the conflict verbatim.
    Theirs,
    /// Use a hand- or agent-merged file, stored content-addressed.
    Merged {
        /// The content address of the merged file bytes.
        content_ref: Hash,
    },
}

/// The resolution chosen for one conflicting file path.
///
/// `path` is the repository-relative path of the conflicting file; `chosen` is
/// the decision that settled it. The set of these on a [`ConflictResolution`] is
/// exactly the set of files that did not auto-merge — an empty set means the
/// merge was clean (DESIGN.md A.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileResolution {
    /// The repository-relative path of the conflicting file.
    pub path: String,
    /// How the conflict at `path` was resolved.
    pub chosen: ResolutionChoice,
}

impl FileResolution {
    /// Construct a `FileResolution` for `path` resolved with `chosen`.
    ///
    /// # Example
    /// ```
    /// use spork_merge::{FileResolution, ResolutionChoice};
    /// let r = FileResolution::new("src/lib.rs", ResolutionChoice::Ours);
    /// assert_eq!(r.path, "src/lib.rs");
    /// assert_eq!(r.chosen, ResolutionChoice::Ours);
    /// ```
    #[must_use]
    pub fn new(path: impl Into<String>, chosen: ResolutionChoice) -> Self {
        FileResolution {
            path: path.into(),
            chosen,
        }
    }
}

/// The conflict-resolution payload stored on a Merge node.
///
/// This is the `conflictResolution{}` of the Merge node's payload core
/// (DESIGN.md §7.1) and the stored conflict-resolution payload of §6.5: a
/// schema-versioned (CLAUDE.md C5), serde round-tripping, content-addressable
/// record of how every conflicting file was settled. Because it is hashed, two
/// byte-identical resolution payloads dedup, and the hash can bind the Merge
/// node's identity to its resolution decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictResolution {
    /// The schema version of this payload shape.
    pub schema_version: u16,
    /// The per-file resolution decisions (empty for a clean merge).
    pub resolutions: Vec<FileResolution>,
}

impl ConflictResolution {
    /// Construct a `ConflictResolution` stamped with the current schema version.
    ///
    /// # Example
    /// ```
    /// use spork_merge::{ConflictResolution, CONFLICT_RESOLUTION_SCHEMA_VERSION};
    /// let cr = ConflictResolution::new(Vec::new());
    /// assert_eq!(cr.schema_version, CONFLICT_RESOLUTION_SCHEMA_VERSION);
    /// assert!(cr.resolutions.is_empty());
    /// ```
    #[must_use]
    pub fn new(resolutions: Vec<FileResolution>) -> Self {
        ConflictResolution {
            schema_version: CONFLICT_RESOLUTION_SCHEMA_VERSION,
            resolutions,
        }
    }

    /// Whether this resolution describes a clean merge (no conflicting files).
    ///
    /// A clean merge stores an empty resolution set; a Merge node is only created
    /// once every conflict has a stored resolution, so a non-empty set is a
    /// complete record of the conflicts that existed (DESIGN.md A.4).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.resolutions.is_empty()
    }

    /// The content-address hash of this payload over its canonical bytes.
    ///
    /// Two byte-identical resolution payloads hash identically and dedup; the
    /// hash is the identity a Merge node can bind to its resolution decisions
    /// (DESIGN.md §6.1, §6.5).
    ///
    /// # Errors
    /// Returns [`MergeSchemaError::Canon`] only if the payload somehow contains a
    /// value the canonical encoder rejects (a float); the typed schema here never
    /// introduces one, so in practice this does not fire.
    pub fn content_hash(&self) -> Result<Hash, MergeSchemaError> {
        let bytes = spork_canon::canonicalize(self)?;
        Ok(spork_hash::hash_bytes(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spork_hash::hash_bytes;

    fn merged(path: &str, content: &[u8]) -> FileResolution {
        FileResolution::new(
            path,
            ResolutionChoice::Merged {
                content_ref: hash_bytes(content),
            },
        )
    }

    #[test]
    fn round_trips_through_serde_and_is_versioned() {
        let cr = ConflictResolution::new(vec![
            FileResolution::new("a.rs", ResolutionChoice::Ours),
            FileResolution::new("b.rs", ResolutionChoice::Theirs),
            merged("c.rs", b"merged bytes"),
        ]);
        assert_eq!(cr.schema_version, CONFLICT_RESOLUTION_SCHEMA_VERSION);

        let s = serde_json::to_string(&cr).unwrap();
        let back: ConflictResolution = serde_json::from_str(&s).unwrap();
        assert_eq!(cr, back);
        // The version survives the round trip rather than being defaulted.
        assert_eq!(back.schema_version, CONFLICT_RESOLUTION_SCHEMA_VERSION);
    }

    #[test]
    fn choice_json_shape_is_tagged_and_canon_safe() {
        // Internally tagged on "choice"; Merged carries content_ref as hex.
        let h = hash_bytes(b"x");
        let v = serde_json::to_value(ResolutionChoice::Merged { content_ref: h }).unwrap();
        assert_eq!(v, json!({ "choice": "merged", "content_ref": h.to_hex() }));
        assert_eq!(
            serde_json::to_value(ResolutionChoice::Ours).unwrap(),
            json!({ "choice": "ours" })
        );
        // No floats anywhere -> canonicalizes.
        assert!(spork_canon::canonicalize(&v).is_ok());
    }

    #[test]
    fn empty_resolution_is_clean() {
        assert!(ConflictResolution::new(Vec::new()).is_clean());
        assert!(
            !ConflictResolution::new(vec![FileResolution::new("a", ResolutionChoice::Ours)])
                .is_clean()
        );
    }

    #[test]
    fn content_hash_is_stable_and_distinguishes_choices() {
        let ours = ConflictResolution::new(vec![FileResolution::new("a", ResolutionChoice::Ours)]);
        let theirs =
            ConflictResolution::new(vec![FileResolution::new("a", ResolutionChoice::Theirs)]);

        // Stable: recomputing yields the same hash.
        assert_eq!(ours.content_hash().unwrap(), ours.content_hash().unwrap());
        // A different choice yields a different identity.
        assert_ne!(ours.content_hash().unwrap(), theirs.content_hash().unwrap());
    }

    #[test]
    fn content_hash_matches_manual_canonicalization() {
        let cr = ConflictResolution::new(vec![merged("c.rs", b"hello")]);
        let bytes = spork_canon::canonicalize(&cr).unwrap();
        assert_eq!(cr.content_hash().unwrap(), hash_bytes(&bytes));
    }
}
