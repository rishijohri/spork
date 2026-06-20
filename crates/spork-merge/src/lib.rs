//! Spork F4 merge schemas — conflict resolution and synthetic transcripts.
//!
//! This crate freezes the merge-payload schemas that the P5 Merge node and the
//! P9 graft consume, so those phases only add logic behind already-frozen,
//! versioned data shapes (CLAUDE.md C2 / C5). It depends on `spork-provider`
//! because a synthetic transcript is built from canonical transcripts.
//!
//! # Why freeze schemas, not behavior
//!
//! A Merge is an *explicit, recorded* operation — branches are never
//! auto-merged (DESIGN.md §6.5). Code is reconciled by a three-way file merge
//! against the nearest common ancestor, and the outcome is stored as a
//! [`ConflictResolution`]. Conversations have no clean three-way analogue, so a
//! conversation merge is append-style: a [`SyntheticTranscript`] = common
//! prefix + a generated merge summary + both branch tails marked with
//! provenance (DESIGN.md A.4). F4 freezes these *records* under versioned,
//! byte-stable schemas; the engines that compute them (the P5 Merge node, the
//! P9 graft) build behind these shapes without changing them (CLAUDE.md C2).
//!
//! # The frozen surface
//!
//! - [`ConflictResolution`] / [`FileResolution`] / [`ResolutionChoice`] — a
//!   schema-versioned ([`CONFLICT_RESOLUTION_SCHEMA_VERSION`]), serde
//!   round-tripping, content-addressable record of per-file resolution choices
//!   (Ours / Theirs / Merged with a content ref).
//! - [`SyntheticTranscript`] / [`ProvenanceTaggedTail`] — a schema-versioned
//!   ([`SYNTHETIC_TRANSCRIPT_SCHEMA_VERSION`]) merge transcript: a common
//!   prefix, a merge summary, and provenance-tagged tails.
//! - [`synthesize`] — a deterministic function that builds a
//!   [`SyntheticTranscript`] from a common, an "ours", and a "theirs" canonical
//!   transcript, so the result can be canonical-hashed stably.
//! - [`MergeSchemaError`] — the one `#[non_exhaustive]` error taxonomy.
//!
//! Both payloads expose `content_hash()` so a Merge node can bind its identity
//! to its resolution decisions and its merged conversation, and so the merged
//! conversation restores alongside its code (DESIGN.md §6.1, §6.5).
//!
//! # Example: an end-to-end conversation merge that hashes stably
//! ```
//! use spork_merge::{synthesize, ConflictResolution, FileResolution, ResolutionChoice};
//! use spork_provider::{CanonicalTranscript, CanonicalTurn, Role};
//!
//! // Code side: record how a conflict was settled.
//! let cr = ConflictResolution::new(vec![FileResolution::new(
//!     "src/lib.rs",
//!     ResolutionChoice::Ours,
//! )]);
//! assert!(cr.content_hash().is_ok());
//!
//! // Conversation side: build the append-style synthetic transcript.
//! let common = CanonicalTranscript::new(vec![CanonicalTurn::text(Role::User, "task")]);
//! let ours = CanonicalTranscript::new(vec![
//!     CanonicalTurn::text(Role::User, "task"),
//!     CanonicalTurn::text(Role::Assistant, "our work"),
//! ]);
//! let theirs = CanonicalTranscript::new(vec![
//!     CanonicalTurn::text(Role::User, "task"),
//!     CanonicalTurn::text(Role::Assistant, "their work"),
//! ]);
//! let merged = synthesize(&common, &ours, &theirs, "merged context".into());
//!
//! // Deterministic: same inputs -> same content address.
//! assert_eq!(
//!     merged.content_hash().unwrap(),
//!     synthesize(&common, &ours, &theirs, "merged context".into())
//!         .content_hash()
//!         .unwrap()
//! );
//! ```
//!
//! This realizes the merge model in DESIGN.md §6.5 ("Merge and extensibility")
//! and the synthetic-transcript / conflict-resolution semantics in appendix A.4.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod resolution;
mod synthetic;

pub use error::MergeSchemaError;
pub use resolution::{
    ConflictResolution, FileResolution, ResolutionChoice, CONFLICT_RESOLUTION_SCHEMA_VERSION,
};
pub use synthetic::{
    synthesize, ProvenanceTaggedTail, SyntheticTranscript, SOURCE_OURS, SOURCE_THEIRS,
    SYNTHETIC_TRANSCRIPT_SCHEMA_VERSION,
};
