//! Spork F3 atomic dual-restore guard — code and conversation, or neither.
//!
//! Restoring a node restores two bound things at once: the code snapshot and the
//! conversation it was paired with. This crate performs that restore under a
//! single lock — it materializes the snapshot via `spork-cas` and verifies /
//! resolves the node's bound `conversation_ref` (an opaque content hash; the
//! canonical transcript schema is an F4 concern, so only the ref slot and the
//! guard are frozen here). If the snapshot or conversation ref diverges or is
//! missing, the restore **fails closed**: it rolls back and changes nothing — the
//! working directory and refs are left intact.
//!
//! A restore is recorded as an *event* (`RestorePerformed`), not an overwrite, so
//! the previously-forward node survives as a sibling and forward history is never
//! lost. `branch_fork` is metadata-only: it creates a new `Ref` and copies zero
//! bytes.
//!
//! This realizes the restore guarantee in DESIGN.md §6.4 ("Restore as an event,
//! not an overwrite"), the dual-restore atomicity in §10.3 ("Restoring code and
//! conversation together"), and the bound-conversation / effects-log slot in
//! §11.4 ("Conversation binding and external effects").
//!
//! # Evolution safety (CLAUDE.md C5)
//!
//! `RestoreOutcome` carries an explicit `schema_version` and a shaped-but-empty
//! `external_effects` slot (DESIGN §11.4 / §18.3 Q5), so the outcome record can
//! grow as the effects-log lands without invalidating older entries.
//!
//! # Public surface
//!
//! - [`RestoreGuard`] — owns the graph service, content store, and working
//!   directory; serializes [`RestoreGuard::restore`] and
//!   [`RestoreGuard::branch_fork`] behind one lock.
//! - [`RestoreOutcome`] / [`ExternalEffect`] / [`ExternalEffectKind`] — the
//!   schema-versioned result records, including the shaped-but-empty effects-log
//!   slot (DESIGN §11.4).
//! - [`RefId`] — the typed handle to the branch ref `branch_fork` creates.
//! - [`RestoreError`] — the fail-closed error taxonomy.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod guard;
mod outcome;

pub use error::RestoreError;
pub use guard::{
    RestoreGuard, DEFAULT_CONVERSATION_REF_FIELD, EVENT_RESTORE_PERFORMED,
    RESTORE_EVENT_SCHEMA_VERSION,
};
pub use outcome::{
    ExternalEffect, ExternalEffectKind, RefId, RestoreOutcome, EXTERNAL_EFFECT_SCHEMA_VERSION,
    RESTORE_OUTCOME_SCHEMA_VERSION,
};
