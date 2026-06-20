//! The broker's failure mode: [`BrokerError`].
//!
//! Authorization has exactly one way to fail — the request is denied — but the
//! *reason* it was denied is part of the audit story, so [`BrokerError::Denied`]
//! carries both the offending [`Capability`] and a human-readable rationale.
//! The enum is `#[non_exhaustive]` (CLAUDE.md C5) so future failure modes (for
//! example a hardened broker's lease-expiry or revocation errors in plan §P8)
//! can be added without breaking existing `match` arms.
//!
//! Design references: DESIGN.md §15.2 ("Capability-Based Permissions" — every
//! privileged call appends an `AuditEntry` with allow/deny; a denial is a
//! first-class, recorded outcome, not a panic).

use crate::capability::Capability;

/// The error returned when the broker refuses to mint a [`ScopedToken`].
///
/// [`ScopedToken`]: crate::ScopedToken
///
/// A denial is always paired with an `AuditEntry` (`allowed = false`) in the
/// broker's audit log; this error is the *caller-facing* half of that same
/// decision. The broker never panics on a malformed or unexpected request — an
/// unknown or ungranted capability is simply [`BrokerError::Denied`].
///
/// `#[non_exhaustive]`: matchers must include a wildcard arm so new failure
/// modes can be added additively (CLAUDE.md C5).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BrokerError {
    /// The request was refused: no grant covered it, or the request fell
    /// outside a grant's scope.
    ///
    /// `capability` is the capability that was requested; `reason` is a
    /// human-readable explanation suitable for surfacing in the audit trail and
    /// to the user (DESIGN.md §15.2 favors human-readable rationales).
    #[error("capability {capability:?} denied: {reason}")]
    Denied {
        /// The capability that was requested and refused.
        capability: Capability,
        /// Why the request was refused (deny-by-default, out-of-scope path,
        /// disallowed host, exceeded budget, …).
        reason: String,
    },
}
