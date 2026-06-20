//! The decision trail: [`AuditEntry`].
//!
//! Every authorization decision the broker makes — **allow or deny** — appends
//! an [`AuditEntry`]. This is the non-negotiable half of the capability model:
//! "Every privileged call appends an `AuditEntry` (capability, scope used,
//! bytes, allow/deny) onto the node" (DESIGN.md §15.2). The audit log makes the
//! full history of what was attempted, what was permitted, and what was refused
//! reconstructable after the fact — it is a first-class decision trace
//! (DESIGN.md §7.2 / §13.6 observability).
//!
//! An [`AuditEntry`] is a persisted record, so it carries an explicit
//! [`AuditEntry::schema_version`] (CLAUDE.md C5); [`AUDIT_ENTRY_SCHEMA_VERSION`]
//! is the version this build writes.
//!
//! Design references: DESIGN.md §15.2 (the audit entry tuple), §7.2 / §13.6
//! (first-class decision traces, local-only by default).

use serde::{Deserialize, Serialize};

use crate::capability::Capability;

/// The schema version of the [`AuditEntry`] record this build emits.
pub const AUDIT_ENTRY_SCHEMA_VERSION: u16 = 1;

/// One recorded authorization decision.
///
/// Every call to
/// [`CapabilityBroker::authorize`](crate::CapabilityBroker::authorize) produces
/// exactly one of these, whether the request was allowed or denied. The
/// `allowed` flag is the decision; `scope_used` records the concrete request
/// that was judged; `bytes` is an optional after-the-fact accounting of how much
/// data the authorized operation moved (the design's "bytes" column — populated
/// by the caller via [`CapabilityBroker::record_bytes`] once the side effect has
/// run).
///
/// [`CapabilityBroker::record_bytes`]: crate::CapabilityBroker::record_bytes
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Self-describing schema version (CLAUDE.md C5).
    pub schema_version: u16,
    /// The capability whose authorization was decided.
    pub capability: Capability,
    /// A human-readable rendering of the concrete request that was judged (the
    /// `RequestedScope::describe` text, or for a denial of an *unrequested*
    /// capability, a short note).
    pub scope_used: String,
    /// `true` if the request was authorized (a token was minted), `false` if it
    /// was denied.
    pub allowed: bool,
    /// Bytes moved by the authorized operation, filled in after the side effect
    /// completes; `None` until then (and always `None` for a denial).
    pub bytes: Option<u64>,
}

impl AuditEntry {
    /// Build an *allow* entry for `capability` against the `scope_used` text.
    pub(crate) fn allow(capability: Capability, scope_used: String) -> Self {
        AuditEntry {
            schema_version: AUDIT_ENTRY_SCHEMA_VERSION,
            capability,
            scope_used,
            allowed: true,
            bytes: None,
        }
    }

    /// Build a *deny* entry for `capability` against the `scope_used` text.
    pub(crate) fn deny(capability: Capability, scope_used: String) -> Self {
        AuditEntry {
            schema_version: AUDIT_ENTRY_SCHEMA_VERSION,
            capability,
            scope_used,
            allowed: false,
            bytes: None,
        }
    }
}
