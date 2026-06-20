//! The proof of authorization: [`ScopedToken`].
//!
//! When the broker authorizes a request it mints a **short-lived scoped token**.
//! The token is the capability-system's "you may now do exactly this" receipt:
//! it names the capability that was granted, the concrete scope it was minted
//! for, and an opaque, content-derived `id` that ties it back to the
//! [`AuditEntry`](crate::AuditEntry) recorded for the same decision. The token
//! is the *only* path to the side effect (DESIGN.md §15.2 — "the only path to
//! side effects").
//!
//! "Short-lived" here means *single decision*: a token authorizes the one
//! request it was minted for. This crate is the headless core, so the token does
//! not yet carry an expiry clock or a lease — that hardening (short-lived leases,
//! revocation) is the explicit plan §P8 broker item. What is frozen now is the
//! token's shape: capability + scope + a stable, non-secret identity.
//!
//! # The token id
//!
//! The `id` is a BLAKE3 fingerprint over the capability, the scope description,
//! and a monotonically increasing per-broker sequence number, rendered through
//! [`spork_hash`]. It is deterministic *given the broker's issuance sequence*
//! and is **not** a secret — it is an audit correlation handle, not a bearer
//! credential. (The secret material a `secrets.get` ultimately yields lives in
//! the vault and is referenced only by an opaque `VaultRef`; it never appears in
//! a token.)
//!
//! Design references: DESIGN.md §15.2 (short-lived scoped tokens are the only
//! path to side effects).

use serde::{Deserialize, Serialize};

use crate::capability::Capability;

/// A minted authorization: the receipt that a specific request was allowed.
///
/// A `ScopedToken` is produced only by
/// [`CapabilityBroker::authorize`](crate::CapabilityBroker::authorize) on an
/// allowed request. Holding one means the broker has already checked the request
/// against the grants and recorded the decision in the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedToken {
    /// Self-describing schema version of the token record (CLAUDE.md C5).
    pub schema_version: u16,
    /// An opaque, non-secret correlation id (a BLAKE3 fingerprint rendered as
    /// `b3.1:<hex>`) that matches this token to its [`AuditEntry`](crate::AuditEntry).
    pub id: String,
    /// The capability this token authorizes.
    pub capability: Capability,
    /// A human-readable rendering of the concrete scope this token was minted
    /// for (the same text stored in the audit entry's `scope_used`).
    pub scope_used: String,
}

/// The schema version of the [`ScopedToken`] record this build emits.
pub const SCOPED_TOKEN_SCHEMA_VERSION: u16 = 1;

impl ScopedToken {
    /// Mint a token for an allowed `capability` request against `scope_used`,
    /// deriving the opaque `id` from `(capability, scope_used, seq)`.
    ///
    /// The `seq` is the broker's issuance counter; folding it into the
    /// fingerprint guarantees two tokens for the *same* capability and scope
    /// still get distinct ids (so the audit correlation is one-to-one).
    pub(crate) fn mint(capability: Capability, scope_used: String, seq: u64) -> Self {
        let mut material = Vec::new();
        material.extend_from_slice(capability.as_str().as_bytes());
        material.push(0);
        material.extend_from_slice(scope_used.as_bytes());
        material.push(0);
        material.extend_from_slice(&seq.to_le_bytes());
        let id = spork_hash::object_id(&material).to_string();
        ScopedToken {
            schema_version: SCOPED_TOKEN_SCHEMA_VERSION,
            id,
            capability,
            scope_used,
        }
    }
}
