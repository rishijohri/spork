//! The [`HandoffDocument`] and the [`HandoffGenerator`] seam.
//!
//! A durable, **regenerable** `HandoffDocument` is auto-generated at node
//! completion and branch points, distilling parent→child lineage into a compact
//! artifact: summary, key decisions, files touched with rationale, open threads,
//! constraints, and test state (DESIGN.md §13.5). It is the cheapest defense
//! against context rot on deep branches — a fresh agent starts cold without
//! replaying ancestor transcripts — and is the primary input to the
//! [`HandoffOnly`](crate::AncestorStrategy::HandoffOnly) ancestor strategy.
//!
//! Distillation is lossy (a confident-but-wrong handoff propagates false
//! premises — the "almost-right" failure mode), so the document is **regenerable**:
//! it carries a `lineage_hash` binding it to the exact ancestry it was distilled
//! from, and `regenerable = true` records that it can be re-derived when an
//! ancestor is later edited (DESIGN.md §13.5). F4 ships a single-node generator
//! behind the [`HandoffGenerator`] trait; P7 adds the lineage *walk* additively.

use serde::{Deserialize, Serialize};
use spork_hash::Hash;
use ulid::Ulid;

use crate::error::ContextError;
use crate::source::HandoffSource;

/// The frozen schema version of the [`HandoffDocument`] shape (CLAUDE.md C5).
///
/// The handoff is a persisted, content-addressable artifact (it doubles as a
/// lineage-scoped memory entry and an exportable AGENTS.md), so versioning it
/// lets the distillation shape evolve additively without reinterpreting a stored
/// handoff (DESIGN.md §13.5, A.7).
pub const HANDOFF_DOCUMENT_SCHEMA_VERSION: u16 = 1;

/// One file the node touched, with the rationale for the change.
///
/// The "files touched with rationale" of §13.5: keeping the *why* alongside the
/// path is what lets a fresh agent understand the change without replaying the
/// ancestor transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTouched {
    /// The repo-relative path that was changed.
    pub path: String,
    /// Why it was changed (the distilled rationale).
    pub rationale: String,
}

impl FileTouched {
    /// Construct a `FileTouched` from a path and its change rationale.
    #[must_use]
    pub fn new(path: impl Into<String>, rationale: impl Into<String>) -> Self {
        FileTouched {
            path: path.into(),
            rationale: rationale.into(),
        }
    }
}

/// A durable, regenerable distillation of parent→child lineage (DESIGN.md §13.5).
///
/// Every field is the compact distillation a fresh agent needs to start cold:
/// the `summary`, the `key_decisions`, the `files_touched` with rationale, the
/// still-`open_threads`, the `constraints` to respect, and the `test_state`. The
/// `lineage_hash` binds the document to the exact ancestry it was distilled from
/// (so it can be re-derived when an ancestor changes), and `regenerable` records
/// that it is a derived artifact, never hand-authored truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffDocument {
    /// The schema version of this handoff shape (CLAUDE.md C5).
    pub schema_version: u16,
    /// A prose summary of what this node accomplished.
    pub summary: String,
    /// The key decisions made, each a single distilled statement.
    pub key_decisions: Vec<String>,
    /// The files this node touched, with rationale.
    pub files_touched: Vec<FileTouched>,
    /// Threads left open for the next agent to pick up.
    pub open_threads: Vec<String>,
    /// Constraints the next agent must respect.
    pub constraints: Vec<String>,
    /// The test state at this node (e.g. "all green", "3 failing in auth").
    pub test_state: String,
    /// The lineage hash this handoff was distilled from — binds it to its
    /// ancestry so it can be regenerated when an ancestor is edited (§13.5).
    pub lineage_hash: Hash,
    /// Whether this document is a regenerable derived artifact (always `true`
    /// for a generated handoff).
    pub regenerable: bool,
}

impl HandoffDocument {
    /// The content-address hash of this handoff over its canonical bytes.
    ///
    /// Two byte-identical handoffs hash identically and dedup; the hash keys the
    /// handoff as a lineage-scoped memory entry (DESIGN.md §13.5, §6.1).
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::Canon`] only if the document somehow contains a
    /// value the canonical encoder rejects (a float); the typed schema never
    /// introduces one.
    pub fn content_hash(&self) -> Result<Hash, ContextError> {
        let bytes = spork_canon::canonicalize(self)?;
        Ok(spork_hash::hash_bytes(&bytes))
    }
}

/// The handoff-generation seam (DESIGN.md §13.5).
///
/// Per the foundation discipline (CLAUDE.md C3) the trait is the seam and F4
/// ships exactly one real implementation behind it ([`SingleNodeHandoffGenerator`]);
/// the lineage-walking, multi-ancestor generator is added additively in P7.
pub trait HandoffGenerator {
    /// Generate a regenerable [`HandoffDocument`] for `node`.
    ///
    /// # Errors
    ///
    /// Returns [`ContextError::NodeNotFound`] if the node is unknown to the
    /// generator's source, or [`ContextError::Provider`] /
    /// [`ContextError::Canon`] on a hashing failure.
    fn generate(&self, node: Ulid) -> Result<HandoffDocument, ContextError>;
}

/// The v1 single-node handoff generator.
///
/// It distills exactly one node's completion materials — drawn from a
/// [`HandoffSource`] — into a regenerable [`HandoffDocument`], stamping the
/// node's `lineage_hash` so the document is bound to its ancestry and can be
/// re-derived when an ancestor is edited (DESIGN.md §13.5). It is real and
/// complete for the single-node case; the lineage walk is P7.
pub struct SingleNodeHandoffGenerator<S: HandoffSource> {
    source: S,
}

impl<S: HandoffSource> SingleNodeHandoffGenerator<S> {
    /// Construct a generator over a [`HandoffSource`].
    #[must_use]
    pub fn new(source: S) -> Self {
        SingleNodeHandoffGenerator { source }
    }

    /// Borrow the underlying source (read-only).
    #[must_use]
    pub fn source(&self) -> &S {
        &self.source
    }
}

impl<S: HandoffSource> HandoffGenerator for SingleNodeHandoffGenerator<S> {
    fn generate(&self, node: Ulid) -> Result<HandoffDocument, ContextError> {
        let materials = self
            .source
            .handoff_materials(node)?
            .ok_or_else(|| ContextError::NodeNotFound(node.to_string()))?;

        Ok(HandoffDocument {
            schema_version: HANDOFF_DOCUMENT_SCHEMA_VERSION,
            summary: materials.summary,
            key_decisions: materials.key_decisions,
            files_touched: materials.files_touched,
            open_threads: materials.open_threads,
            constraints: materials.constraints,
            test_state: materials.test_state,
            lineage_hash: materials.lineage_hash,
            // A generated handoff is always a derived, re-derivable artifact.
            regenerable: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::HandoffMaterials;

    /// A fixed in-memory source for one node.
    struct FixedSource {
        id: Ulid,
        materials: HandoffMaterials,
    }

    impl HandoffSource for FixedSource {
        fn handoff_materials(&self, node: Ulid) -> Result<Option<HandoffMaterials>, ContextError> {
            if node == self.id {
                Ok(Some(self.materials.clone()))
            } else {
                Ok(None)
            }
        }
    }

    fn materials(lineage: Hash) -> HandoffMaterials {
        HandoffMaterials {
            summary: "added retry to the http client".into(),
            key_decisions: vec!["use exponential backoff".into()],
            files_touched: vec![FileTouched::new("src/client.rs", "added retry loop")],
            open_threads: vec!["decide max retries".into()],
            constraints: vec!["must stay sync".into()],
            test_state: "all green".into(),
            lineage_hash: lineage,
        }
    }

    #[test]
    fn schema_version_is_frozen_at_one() {
        assert_eq!(HANDOFF_DOCUMENT_SCHEMA_VERSION, 1);
    }

    #[test]
    fn generated_handoff_is_regenerable_and_carries_lineage() {
        let id = Ulid::new();
        let lineage = spork_hash::hash_bytes(b"lineage");
        let gen = SingleNodeHandoffGenerator::new(FixedSource {
            id,
            materials: materials(lineage),
        });
        let doc = gen.generate(id).unwrap();
        assert!(doc.regenerable);
        assert_eq!(doc.lineage_hash, lineage);
        assert_eq!(doc.schema_version, HANDOFF_DOCUMENT_SCHEMA_VERSION);
        assert_eq!(doc.test_state, "all green");
        assert_eq!(doc.files_touched[0].path, "src/client.rs");
    }

    #[test]
    fn unknown_node_is_not_found() {
        let gen = SingleNodeHandoffGenerator::new(FixedSource {
            id: Ulid::new(),
            materials: materials(spork_hash::hash_bytes(b"x")),
        });
        let err = gen.generate(Ulid::new()).unwrap_err();
        assert!(matches!(err, ContextError::NodeNotFound(_)));
    }

    #[test]
    fn handoff_content_hash_is_stable_and_dedups() {
        let id = Ulid::new();
        let lineage = spork_hash::hash_bytes(b"lineage");
        let gen = SingleNodeHandoffGenerator::new(FixedSource {
            id,
            materials: materials(lineage),
        });
        let a = gen.generate(id).unwrap();
        let b = gen.generate(id).unwrap();
        // Regeneration is deterministic: identical bytes -> identical hash.
        assert_eq!(a, b);
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn handoff_round_trips_through_serde() {
        let doc = HandoffDocument {
            schema_version: HANDOFF_DOCUMENT_SCHEMA_VERSION,
            summary: "s".into(),
            key_decisions: vec!["d".into()],
            files_touched: vec![FileTouched::new("a.rs", "r")],
            open_threads: vec!["t".into()],
            constraints: vec!["c".into()],
            test_state: "ok".into(),
            lineage_hash: spork_hash::hash_bytes(b"l"),
            regenerable: true,
        };
        let v = serde_json::to_value(&doc).unwrap();
        let back: HandoffDocument = serde_json::from_value(v).unwrap();
        assert_eq!(doc, back);
    }
}
