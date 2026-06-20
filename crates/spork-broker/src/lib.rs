//! Spork F3 capability broker — deny-by-default side-effect authorization.
//!
//! Every side effect a node may attempt (reading or writing snapshots, spawning
//! a process, opening a network connection, invoking a model, reading another
//! node's outputs, fetching a secret) passes through this broker first. The
//! broker holds a fixed `Capability` vocabulary, a versioned `Scope` grammar
//! (path globs, host allowlists, model token/USD budgets), and issues a
//! short-lived `ScopedToken` only when an explicit `Grant` covers the request.
//! With no matching grant the request is denied — there is no implicit allow.
//!
//! Every authorization decision, allow or deny, appends an `AuditEntry` so the
//! full trail of what was attempted, what was permitted, and what was refused is
//! always reconstructable.
//!
//! This realizes the capability model in DESIGN.md §15.1 ("Capabilities and the
//! broker") and the scope grammar and audit trail in §15.2 ("Scopes, grants, and
//! the audit log").
//!
//! # Evolution safety (CLAUDE.md C5)
//!
//! `Scope` and `AuditEntry` each carry an explicit `schema_version`, so the
//! scope grammar and the audit record can grow new fields without invalidating
//! older persisted entries. The `Capability` vocabulary is deliberately fixed
//! and frozen.
//!
//! # Module map
//!
//! - [`capability`] — the frozen [`Capability`] vocabulary (DESIGN.md §15.2
//!   table).
//! - [`scope`] — the versioned [`Scope`] grammar and the [`RequestedScope`] a
//!   caller asks against.
//! - [`grant`] — the [`Grant`] (capability + scope) the user approves.
//! - [`token`] — the [`ScopedToken`] minted on an allowed request.
//! - [`audit`] — the [`AuditEntry`] appended on every decision.
//! - [`error`] — the [`BrokerError`] returned on a denial.
//! - [`broker`] — the [`CapabilityBroker`] that ties them together.
//!
//! # Example
//!
//! ```
//! use spork_broker::{Capability, CapabilityBroker, Grant, RequestedScope, Scope};
//!
//! // The user approved exactly one thing: reads under `src/**`.
//! let grant = Grant::new(
//!     Capability::SnapshotRead,
//!     Scope::new().with_path_globs(["src/**"]),
//! );
//! let mut broker = CapabilityBroker::new(vec![grant]);
//!
//! // An in-scope read is allowed and yields a token.
//! let token = broker
//!     .authorize(Capability::SnapshotRead, &RequestedScope::path("src/main.rs"))
//!     .expect("in-scope read is allowed");
//! assert_eq!(token.capability, Capability::SnapshotRead);
//!
//! // A write was never granted: denied, and recorded.
//! assert!(broker
//!     .authorize(Capability::SnapshotWrite, &RequestedScope::path("src/main.rs"))
//!     .is_err());
//!
//! // Both decisions are in the audit log (allow then deny).
//! let log = broker.audit_log();
//! assert_eq!(log.len(), 2);
//! assert!(log[0].allowed);
//! assert!(!log[1].allowed);
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod audit;
pub mod broker;
pub mod capability;
pub mod error;
pub mod grant;
pub mod scope;
pub mod token;

pub use audit::{AuditEntry, AUDIT_ENTRY_SCHEMA_VERSION};
pub use broker::CapabilityBroker;
pub use capability::{Capability, CAPABILITY_SCHEMA_VERSION};
pub use error::BrokerError;
pub use grant::Grant;
pub use scope::{RequestedScope, Scope, SCOPE_SCHEMA_VERSION};
pub use token::{ScopedToken, SCOPED_TOKEN_SCHEMA_VERSION};
