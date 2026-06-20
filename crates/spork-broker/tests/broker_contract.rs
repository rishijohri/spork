//! Contract tests for `spork-broker` (DESIGN.md §15.1, §15.2).
//!
//! These exercise the broker as an external consumer does: the deny-by-default
//! decision, the AuditEntry-per-call trail (allow *and* deny), the versioned
//! scope grammar's serde stability, and the frozen capability vocabulary's wire
//! form.

use spork_broker::{
    AuditEntry, Capability, CapabilityBroker, Grant, RequestedScope, Scope, ScopedToken,
    AUDIT_ENTRY_SCHEMA_VERSION, CAPABILITY_SCHEMA_VERSION, SCOPED_TOKEN_SCHEMA_VERSION,
    SCOPE_SCHEMA_VERSION,
};

/// The deny-by-default contract end to end: no grant means denial, and a denial
/// is itself an audited decision.
#[test]
fn deny_by_default_with_audit() {
    let mut broker = CapabilityBroker::new(vec![]);

    for cap in Capability::ALL {
        let err = broker.authorize(cap, &RequestedScope::none()).unwrap_err();
        assert!(matches!(err, spork_broker::BrokerError::Denied { .. }));
    }

    let log = broker.audit_log();
    assert_eq!(log.len(), Capability::ALL.len());
    assert!(log.iter().all(|e| !e.allowed));
    assert!(log.iter().all(|e| e.bytes.is_none()));
}

/// A realistic bundle: snapshot read+write under `src/**`, model.invoke within a
/// budget, and net.connect to one host. In-scope requests are allowed and
/// audited; out-of-scope ones are denied and audited.
#[test]
fn realistic_bundle_allows_in_scope_denies_out_of_scope() {
    let grants = vec![
        Grant::new(
            Capability::SnapshotRead,
            Scope::new().with_path_globs(["src/**", "Cargo.toml"]),
        ),
        Grant::new(
            Capability::SnapshotWrite,
            Scope::new().with_path_globs(["src/**"]),
        ),
        Grant::new(
            Capability::ModelInvoke,
            Scope::new()
                .with_token_budget(8_000)
                .with_usd_budget_micros(100_000),
        ),
        Grant::new(
            Capability::NetConnect,
            Scope::new().with_hosts(["api.anthropic.com"]),
        ),
    ];
    let mut broker = CapabilityBroker::new(grants);

    // Allowed.
    let t1 = broker
        .authorize(
            Capability::SnapshotRead,
            &RequestedScope::path("src/lib.rs"),
        )
        .unwrap();
    broker.record_bytes(2048);
    let t2 = broker
        .authorize(Capability::SnapshotWrite, &RequestedScope::path("src/x.rs"))
        .unwrap();
    let t3 = broker
        .authorize(
            Capability::ModelInvoke,
            &RequestedScope::model(4_000, 50_000),
        )
        .unwrap();
    let t4 = broker
        .authorize(
            Capability::NetConnect,
            &RequestedScope::host("api.anthropic.com"),
        )
        .unwrap();

    // Tokens have distinct correlation ids.
    let ids: std::collections::HashSet<_> = [&t1.id, &t2.id, &t3.id, &t4.id].into_iter().collect();
    assert_eq!(ids.len(), 4);

    // Denied: writing outside src, reading docs, over budget, wrong host,
    // and a capability never granted at all (secrets.get).
    assert!(broker
        .authorize(
            Capability::SnapshotWrite,
            &RequestedScope::path("docs/readme.md")
        )
        .is_err());
    assert!(broker
        .authorize(
            Capability::SnapshotRead,
            &RequestedScope::path("secrets/key")
        )
        .is_err());
    assert!(broker
        .authorize(
            Capability::ModelInvoke,
            &RequestedScope::model(9_000, 10_000)
        )
        .is_err());
    assert!(broker
        .authorize(
            Capability::NetConnect,
            &RequestedScope::host("exfil.example")
        )
        .is_err());
    assert!(broker
        .authorize(Capability::SecretsGet, &RequestedScope::none())
        .is_err());

    let log = broker.audit_log();
    let allowed = log.iter().filter(|e| e.allowed).count();
    let denied = log.iter().filter(|e| !e.allowed).count();
    assert_eq!(allowed, 4);
    assert_eq!(denied, 5);

    // The bytes accounting landed on the first (read) entry only.
    assert_eq!(log[0].bytes, Some(2048));
    assert!(log[1..].iter().all(|e| e.bytes.is_none()));
}

/// The frozen capability vocabulary serializes to the dotted manifest spelling.
#[test]
fn capability_wire_form_is_dotted_manifest_spelling() {
    let cases = [
        (Capability::SnapshotRead, "\"snapshot.read\""),
        (Capability::SnapshotWrite, "\"snapshot.write\""),
        (Capability::ProcessSpawn, "\"process.spawn\""),
        (Capability::NetConnect, "\"net.connect\""),
        (Capability::ModelInvoke, "\"model.invoke\""),
        (Capability::NodesReadOutputs, "\"nodes.readOutputs\""),
        (Capability::SecretsGet, "\"secrets.get\""),
    ];
    for (cap, json) in cases {
        assert_eq!(serde_json::to_string(&cap).unwrap(), json);
        let back: Capability = serde_json::from_str(json).unwrap();
        assert_eq!(back, cap);
        assert_eq!(cap.as_str(), json.trim_matches('"'));
    }
}

/// Grant, Scope, AuditEntry, and ScopedToken all round-trip through JSON,
/// preserving their self-describing schema versions.
#[test]
fn persisted_structs_round_trip_with_schema_versions() {
    let scope = Scope::new()
        .with_path_globs(["src/**"])
        .with_hosts(["h"])
        .with_token_budget(10)
        .with_usd_budget_micros(20);
    assert_eq!(scope.schema_version, SCOPE_SCHEMA_VERSION);

    let grant = Grant::new(Capability::SnapshotRead, scope.clone());
    let grant2: Grant = serde_json::from_str(&serde_json::to_string(&grant).unwrap()).unwrap();
    assert_eq!(grant, grant2);

    let mut broker = CapabilityBroker::new(vec![grant]);
    let token = broker
        .authorize(Capability::SnapshotRead, &RequestedScope::path("src/a.rs"))
        .unwrap();
    assert_eq!(token.schema_version, SCOPED_TOKEN_SCHEMA_VERSION);
    let token2: ScopedToken =
        serde_json::from_str(&serde_json::to_string(&token).unwrap()).unwrap();
    assert_eq!(token, token2);

    let entry = &broker.audit_log()[0];
    assert_eq!(entry.schema_version, AUDIT_ENTRY_SCHEMA_VERSION);
    let entry2: AuditEntry = serde_json::from_str(&serde_json::to_string(entry).unwrap()).unwrap();
    assert_eq!(entry, &entry2);

    // The capability vocabulary version is pinned (the set is frozen).
    assert_eq!(CAPABILITY_SCHEMA_VERSION, 1);
}

/// An older audit entry that lacks the (additive) `bytes` field still
/// deserializes — the schema can grow forward-compatibly.
#[test]
fn audit_entry_tolerates_absent_optional_bytes() {
    let json = r#"{
        "schema_version": 1,
        "capability": "snapshot.read",
        "scope_used": "path=src/a.rs",
        "allowed": true,
        "bytes": null
    }"#;
    let entry: AuditEntry = serde_json::from_str(json).unwrap();
    assert!(entry.allowed);
    assert_eq!(entry.capability, Capability::SnapshotRead);
    assert_eq!(entry.bytes, None);
}

/// The broker never panics regardless of the request shape thrown at it: every
/// (capability, odd-request) pair against an empty broker is a clean denial.
#[test]
fn never_panics_on_any_request_shape() {
    let mut broker = CapabilityBroker::new(vec![]);
    let weird = [
        RequestedScope::none(),
        RequestedScope::path(""),
        RequestedScope::path("../../etc/passwd"),
        RequestedScope::host(""),
        RequestedScope::model(u64::MAX, u64::MAX),
        RequestedScope {
            path: Some("p".into()),
            host: Some("h".into()),
            tokens: Some(0),
            usd_micros: Some(0),
        },
    ];
    for cap in Capability::ALL {
        for req in &weird {
            assert!(broker.authorize(cap, req).is_err());
        }
    }
    assert_eq!(
        broker.audit_log().len(),
        Capability::ALL.len() * weird.len()
    );
}
