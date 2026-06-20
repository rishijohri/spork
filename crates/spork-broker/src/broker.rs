//! The deny-by-default authorizer: [`CapabilityBroker`].
//!
//! The broker is the single gate every side effect passes through. It is
//! constructed from a fixed set of [`Grant`]s (what the user reviewed and
//! approved); from then on, [`CapabilityBroker::authorize`] is the *only* way to
//! obtain a [`ScopedToken`], and a token is the *only* path to a side effect
//! (DESIGN.md §15.2).
//!
//! Three invariants define its behavior:
//!
//! 1. **Deny by default.** A request is allowed only if some grant for that
//!    exact capability covers the requested scope. No grant ⇒ denied. There is
//!    no implicit allow, no ambient authority.
//! 2. **Audit every decision.** Allow *or* deny, each call appends exactly one
//!    [`AuditEntry`]. The audit log ([`CapabilityBroker::audit_log`]) is the
//!    reconstructable trail of everything attempted.
//! 3. **Never panic.** An unknown, unrequested, or malformed request is a
//!    denial, not a crash — the broker is a security boundary and must fail
//!    closed and loud-in-the-log rather than abort.
//!
//! Design references: DESIGN.md §15.1 (all privileged operations behind the
//! broker), §15.2 (deny-by-default capability broker, short-lived scoped tokens,
//! `AuditEntry` per call).

use crate::audit::AuditEntry;
use crate::capability::Capability;
use crate::error::BrokerError;
use crate::grant::Grant;
use crate::scope::RequestedScope;
use crate::token::ScopedToken;

/// The runtime capability broker: deny-by-default, audited, single-path.
///
/// Build one with [`CapabilityBroker::new`] from the grants the user approved,
/// then call [`CapabilityBroker::authorize`] for each side effect. The broker
/// owns its audit log; mutating methods take `&mut self` because every decision
/// extends that log.
#[derive(Debug, Clone)]
pub struct CapabilityBroker {
    /// The complete set of approved grants. Anything not covered here is denied.
    grants: Vec<Grant>,
    /// The append-only decision trail (one entry per `authorize` call).
    audit: Vec<AuditEntry>,
    /// Monotonic token-issuance counter, folded into each token's id so two
    /// tokens for the same capability+scope still get distinct correlation ids.
    issued: u64,
}

impl CapabilityBroker {
    /// Construct a broker that authorizes exactly the side effects covered by
    /// `grants` (and nothing else).
    ///
    /// The audit log starts empty. Passing an empty `grants` vector yields a
    /// broker that denies *everything* — the strictest deny-by-default posture.
    #[must_use]
    pub fn new(grants: Vec<Grant>) -> Self {
        CapabilityBroker {
            grants,
            audit: Vec::new(),
            issued: 0,
        }
    }

    /// Authorize one side effect, returning a [`ScopedToken`] on success.
    ///
    /// The broker searches its grants for one whose capability equals `cap` and
    /// whose [`Scope`](crate::Scope) covers `requested`. On the first match it
    /// mints a short-lived token, appends an *allow* [`AuditEntry`], and returns
    /// the token. If no grant matches (none for this capability, or all of them
    /// out of scope) it appends a *deny* [`AuditEntry`] and returns
    /// [`BrokerError::Denied`] with the reason.
    ///
    /// This is the **only** way to obtain a token, and it records a decision on
    /// **every** call — including denials — satisfying the "AuditEntry per call,
    /// allow and deny" contract. It never panics on an unexpected request.
    ///
    /// # Errors
    /// Returns [`BrokerError::Denied`] when no grant covers the request. The
    /// matching deny entry is appended to [`CapabilityBroker::audit_log`] before
    /// returning.
    pub fn authorize(
        &mut self,
        cap: Capability,
        requested: &RequestedScope,
    ) -> Result<ScopedToken, BrokerError> {
        let scope_used = requested.describe();

        // Collect the grants for this exact capability. With none, the request
        // is denied by default — this also covers an "unrequested"/unexpected
        // capability with no grant: a denial, never a panic.
        let mut matching = self
            .grants
            .iter()
            .filter(|g| g.capability == cap)
            .peekable();

        if matching.peek().is_none() {
            self.audit.push(AuditEntry::deny(cap, scope_used.clone()));
            return Err(BrokerError::Denied {
                capability: cap,
                reason: format!("no grant for {} (deny-by-default)", cap.as_str()),
            });
        }

        // Try each grant for this capability; the grant set is a union, so the
        // first one whose scope covers the request wins. Remember the last
        // out-of-scope reason for the audit/deny message.
        let mut last_reason: Option<String> = None;
        for grant in matching {
            match grant.scope.covers(cap, requested) {
                Ok(()) => {
                    self.issued += 1;
                    let token = ScopedToken::mint(cap, scope_used.clone(), self.issued);
                    self.audit.push(AuditEntry::allow(cap, scope_used));
                    return Ok(token);
                }
                Err(reason) => last_reason = Some(reason),
            }
        }

        // A grant for the capability existed, but none covered this request.
        let reason =
            last_reason.unwrap_or_else(|| format!("request out of scope for {}", cap.as_str()));
        self.audit.push(AuditEntry::deny(cap, scope_used));
        Err(BrokerError::Denied {
            capability: cap,
            reason,
        })
    }

    /// The append-only audit log: one [`AuditEntry`] per
    /// [`CapabilityBroker::authorize`] call, in decision order.
    ///
    /// This is the reconstructable trail the design requires (every allow and
    /// every deny). It is read-only; entries are only ever appended.
    #[must_use]
    pub fn audit_log(&self) -> &[AuditEntry] {
        &self.audit
    }

    /// Attach a `bytes` accounting to the most recent **allow** entry.
    ///
    /// The design's audit tuple includes a `bytes` column for how much data an
    /// authorized operation moved; that figure is only known *after* the side
    /// effect runs, so the caller records it here once the operation completes.
    /// The call targets the most recent allowed entry (the token just used) and
    /// is a no-op (returning `false`) if the most recent entry is a denial or
    /// the log is empty — a denial moved no bytes.
    ///
    /// Returns `true` if an allow entry was updated.
    pub fn record_bytes(&mut self, bytes: u64) -> bool {
        match self.audit.last_mut() {
            Some(entry) if entry.allowed => {
                entry.bytes = Some(bytes);
                true
            }
            _ => false,
        }
    }

    /// The grants this broker was constructed with (read-only).
    ///
    /// Exposed for inspection (UI listings, tests, observability); the grant set
    /// is immutable for the broker's lifetime.
    #[must_use]
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::Scope;

    fn read_grant(globs: &[&str]) -> Grant {
        Grant::new(
            Capability::SnapshotRead,
            Scope::new().with_path_globs(globs.iter().copied()),
        )
    }

    #[test]
    fn no_grant_denies_and_records_deny_entry() {
        let mut broker = CapabilityBroker::new(vec![]);
        let err = broker
            .authorize(
                Capability::SnapshotRead,
                &RequestedScope::path("src/main.rs"),
            )
            .unwrap_err();
        match err {
            BrokerError::Denied { capability, .. } => {
                assert_eq!(capability, Capability::SnapshotRead);
            }
        }
        let log = broker.audit_log();
        assert_eq!(log.len(), 1);
        assert!(!log[0].allowed);
        assert_eq!(log[0].capability, Capability::SnapshotRead);
        assert_eq!(log[0].scope_used, "path=src/main.rs");
    }

    #[test]
    fn grant_within_scope_allows_and_records_allow_entry() {
        let mut broker = CapabilityBroker::new(vec![read_grant(&["src/**"])]);
        let token = broker
            .authorize(
                Capability::SnapshotRead,
                &RequestedScope::path("src/a/b.rs"),
            )
            .expect("in scope");
        assert_eq!(token.capability, Capability::SnapshotRead);
        assert!(token.id.starts_with("b3.1:"));
        assert_eq!(token.scope_used, "path=src/a/b.rs");

        let log = broker.audit_log();
        assert_eq!(log.len(), 1);
        assert!(log[0].allowed);
        assert_eq!(log[0].bytes, None);
    }

    #[test]
    fn request_outside_granted_path_glob_is_denied() {
        let mut broker = CapabilityBroker::new(vec![read_grant(&["src/**"])]);
        let err = broker
            .authorize(
                Capability::SnapshotRead,
                &RequestedScope::path("tests/a.rs"),
            )
            .unwrap_err();
        assert!(matches!(err, BrokerError::Denied { .. }));
        assert!(!broker.audit_log()[0].allowed);
    }

    #[test]
    fn request_outside_host_allowlist_is_denied() {
        let grant = Grant::new(
            Capability::NetConnect,
            Scope::new().with_hosts(["api.anthropic.com"]),
        );
        let mut broker = CapabilityBroker::new(vec![grant]);

        assert!(broker
            .authorize(
                Capability::NetConnect,
                &RequestedScope::host("api.anthropic.com")
            )
            .is_ok());
        assert!(broker
            .authorize(
                Capability::NetConnect,
                &RequestedScope::host("evil.example")
            )
            .is_err());

        let log = broker.audit_log();
        assert!(log[0].allowed);
        assert!(!log[1].allowed);
    }

    #[test]
    fn unrequested_capability_is_denied_never_panics() {
        // A broker with only a read grant must deny every *other* capability,
        // for every kind of request shape, without panicking.
        let mut broker = CapabilityBroker::new(vec![read_grant(&["**"])]);
        for cap in Capability::ALL {
            if cap == Capability::SnapshotRead {
                continue;
            }
            let req = RequestedScope {
                path: Some("x".into()),
                host: Some("h".into()),
                tokens: Some(1),
                usd_micros: Some(1),
            };
            assert!(
                broker.authorize(cap, &req).is_err(),
                "{cap:?} should be denied"
            );
        }
        // One deny entry per non-read capability.
        assert_eq!(broker.audit_log().len(), Capability::ALL.len() - 1);
        assert!(broker.audit_log().iter().all(|e| !e.allowed));
    }

    #[test]
    fn model_budget_enforced() {
        let grant = Grant::new(
            Capability::ModelInvoke,
            Scope::new()
                .with_token_budget(1000)
                .with_usd_budget_micros(50_000),
        );
        let mut broker = CapabilityBroker::new(vec![grant]);

        assert!(broker
            .authorize(Capability::ModelInvoke, &RequestedScope::model(500, 25_000))
            .is_ok());
        // Over token budget.
        assert!(broker
            .authorize(
                Capability::ModelInvoke,
                &RequestedScope::model(2000, 25_000)
            )
            .is_err());
        // Over USD budget.
        assert!(broker
            .authorize(Capability::ModelInvoke, &RequestedScope::model(500, 99_000))
            .is_err());
    }

    #[test]
    fn union_of_grants_for_same_capability() {
        let mut broker =
            CapabilityBroker::new(vec![read_grant(&["src/**"]), read_grant(&["tests/**"])]);
        assert!(broker
            .authorize(Capability::SnapshotRead, &RequestedScope::path("src/a.rs"))
            .is_ok());
        assert!(broker
            .authorize(
                Capability::SnapshotRead,
                &RequestedScope::path("tests/b.rs")
            )
            .is_ok());
        assert!(broker
            .authorize(Capability::SnapshotRead, &RequestedScope::path("docs/c.md"))
            .is_err());
    }

    #[test]
    fn record_bytes_targets_last_allow_only() {
        let mut broker = CapabilityBroker::new(vec![read_grant(&["**"])]);
        broker
            .authorize(Capability::SnapshotRead, &RequestedScope::path("a"))
            .unwrap();
        assert!(broker.record_bytes(4096));
        assert_eq!(broker.audit_log().last().unwrap().bytes, Some(4096));

        // A denial moved no bytes: record_bytes is a no-op after a deny.
        let _ = broker.authorize(Capability::SnapshotWrite, &RequestedScope::path("a"));
        assert!(!broker.record_bytes(10));
        assert_eq!(broker.audit_log().last().unwrap().bytes, None);
    }

    #[test]
    fn empty_path_glob_grants_nothing() {
        // A read grant with no globs is deny-by-default along the path axis.
        let mut broker = CapabilityBroker::new(vec![read_grant(&[])]);
        assert!(broker
            .authorize(Capability::SnapshotRead, &RequestedScope::path("anything"))
            .is_err());
    }

    #[test]
    fn capabilities_with_no_scope_axis_allowed_by_grant_existence() {
        // process.spawn / nodes.readOutputs / secrets.get have no path/host/
        // budget axis: the grant itself is the permission.
        for cap in [
            Capability::ProcessSpawn,
            Capability::NodesReadOutputs,
            Capability::SecretsGet,
        ] {
            let mut broker = CapabilityBroker::new(vec![Grant::new(cap, Scope::new())]);
            assert!(
                broker.authorize(cap, &RequestedScope::none()).is_ok(),
                "{cap:?} should be allowed by grant existence"
            );
        }
    }
}
