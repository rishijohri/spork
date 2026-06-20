//! Integration tests for `spork-graph`: the validate-then-append command layer,
//! the SQLite graph projection, projection==log purity, lazy payload upgrade,
//! mixed-version resolution, and randomized DAG/replay properties (DESIGN §6.2,
//! §6.3, §6.5, §7.x, A.1).

use std::sync::Arc;

use proptest::prelude::*;
use semver::Version;
use serde_json::{json, Value};
use spork_edges::{EdgeError, EdgeType, RefKind};
use spork_graph::{
    GraphError, GraphProjection, GraphService, Lifecycle, NodeEnvelope, BUILTIN_SNAPSHOT_KIND,
};
use spork_hash::{hash_bytes, Hash};
use spork_log::EventLog;
use spork_migrate::{EventMigration, MigrationError, MigrationRegistry};
use spork_registry::{Family, NodeTypeDescriptor, NodeTypeRegistry, StalenessRule};
use tempfile::TempDir;
use ulid::Ulid;

/// A test harness: a temp dir, an open event log, and a service over it.
struct Harness {
    _dir: TempDir,
    log: EventLog,
    svc: GraphService,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(&dir.path().join("log.db")).unwrap();
        let svc = GraphService::open_in_memory(log.writer()).unwrap();
        Harness {
            _dir: dir,
            log,
            svc,
        }
    }

    /// A harness whose service is built with an explicit migration registry.
    // The `Arc<MigrationRegistry>` is confined to the single-threaded service
    // (matching `spork-log`'s reader API); the cross-thread lint does not apply.
    #[allow(clippy::arc_with_non_send_sync)]
    fn with_migrations(migrations: MigrationRegistry) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(&dir.path().join("log.db")).unwrap();
        let proj = GraphProjection::open_in_memory().unwrap();
        let svc = GraphService::new(log.writer(), proj, Arc::new(migrations));
        Harness {
            _dir: dir,
            log,
            svc,
        }
    }

    /// Rebuild a fresh projection by folding the whole log.
    fn rebuild(&self) -> GraphProjection {
        let reader = self.log.reader().unwrap();
        GraphProjection::rebuild_from_log(&reader).unwrap()
    }
}

/// A non-snapshot-owning context descriptor for a given kind/version.
fn context_descriptor(id: &str, version: Version) -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: id.to_string(),
        type_version: version,
        family: Family::Context,
        owns_snapshot: false,
        payload_schema: json!({ "type": "object" }),
        result_schema: None,
        allowed_edges: vec![EdgeType::ParentChild, EdgeType::DerivedFrom],
        ports: vec![],
        staleness_rule: StalenessRule::Never,
        capabilities_required: vec![],
        ui_contributions: json!({}),
        revoked_provenance: None,
    }
}

/// An observing (test-class) descriptor allowed to VALIDATE its target.
fn validation_descriptor(version: Version) -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: "validation".to_string(),
        type_version: version,
        family: Family::Observing,
        owns_snapshot: false,
        payload_schema: json!({ "type": "object" }),
        result_schema: Some(json!({ "type": "object" })),
        allowed_edges: vec![EdgeType::Validates, EdgeType::ParentChild],
        ports: vec![],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec![],
        ui_contributions: json!({}),
        revoked_provenance: None,
    }
}

/// Create a built-in snapshot node with a fresh snapshot hash.
fn make_snapshot(svc: &mut GraphService, parents: Vec<Ulid>, tag: &[u8]) -> NodeEnvelope {
    svc.create_node(
        BUILTIN_SNAPSHOT_KIND,
        None,
        parents,
        "main",
        json!({ "origin": "manual" }),
        true,
        Some(hash_bytes(tag)),
    )
    .unwrap()
}

// ---- DoD: built-in dogfood + owns_snapshot rules -----------------------------

#[test]
fn builtin_snapshot_registers_through_public_path() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let d = h
        .svc
        .registry()
        .resolve(BUILTIN_SNAPSHOT_KIND, None)
        .unwrap();
    assert_eq!(d.family, Family::Mutating);
    assert!(d.owns_snapshot);
    assert!(d.has_snapshot_out_port());
}

#[test]
fn descriptor_owns_snapshot_without_snapshot_ref_is_refused_at_registration() {
    // Exactly as a third party would register it — no special built-in path.
    let mut reg = NodeTypeRegistry::new();
    let bad = NodeTypeDescriptor {
        id: "bad".to_string(),
        type_version: Version::new(1, 0, 0),
        family: Family::Mutating,
        owns_snapshot: true, // claims to own a snapshot...
        payload_schema: json!({}),
        result_schema: None,
        allowed_edges: vec![],
        ports: vec![], // ...but exposes no SnapshotRef out-port.
        staleness_rule: StalenessRule::Never,
        capabilities_required: vec![],
        ui_contributions: json!({}),
        revoked_provenance: None,
    };
    let err = reg.register(bad).unwrap_err();
    assert!(matches!(
        err,
        spork_registry::RegistryError::OwnsSnapshotMismatch { .. }
    ));
}

#[test]
fn create_node_owns_snapshot_without_hash_is_rejected() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let err = h
        .svc
        .create_node(
            BUILTIN_SNAPSHOT_KIND,
            None,
            vec![],
            "main",
            json!({ "origin": "manual" }),
            true,
            None, // missing snapshot hash for an owns_snapshot node
        )
        .unwrap_err();
    assert_eq!(err, GraphError::OwnsSnapshotMismatch);
}

#[test]
fn create_node_non_owning_with_hash_is_rejected() {
    let mut h = Harness::new();
    h.svc
        .register_descriptor(context_descriptor("plan", Version::new(1, 0, 0)))
        .unwrap();
    let err = h
        .svc
        .create_node(
            "plan",
            None,
            vec![],
            "main",
            json!({}),
            false,
            Some(hash_bytes(b"x")), // a hash on a non-owning node
        )
        .unwrap_err();
    assert_eq!(err, GraphError::OwnsSnapshotMismatch);
}

#[test]
fn create_node_owns_snapshot_disagreeing_with_descriptor_is_rejected() {
    let mut h = Harness::new();
    h.svc
        .register_descriptor(context_descriptor("plan", Version::new(1, 0, 0)))
        .unwrap();
    // Descriptor says owns_snapshot=false; request says true.
    let err = h
        .svc
        .create_node(
            "plan",
            None,
            vec![],
            "main",
            json!({}),
            true,
            Some(hash_bytes(b"x")),
        )
        .unwrap_err();
    assert_eq!(err, GraphError::OwnsSnapshotMismatch);
}

#[test]
fn create_node_unknown_kind_is_rejected() {
    let mut h = Harness::new();
    let err = h
        .svc
        .create_node("nope", None, vec![], "main", json!({}), false, None)
        .unwrap_err();
    assert!(matches!(err, GraphError::UnknownKind { .. }));
}

// ---- lineage hash ------------------------------------------------------------

#[test]
fn lineage_hash_populates_and_chains_through_parents() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();

    let root = make_snapshot(&mut h.svc, vec![], b"root");
    let child = make_snapshot(&mut h.svc, vec![root.id], b"child");

    // The root lineage is the hash of an empty-parent document.
    let expected_root = spork_graph::lineage_hash(&[], root.id, BUILTIN_SNAPSHOT_KIND).unwrap();
    assert_eq!(root.lineage_hash, expected_root);

    // The child folds the root's lineage hash.
    let expected_child =
        spork_graph::lineage_hash(&[root.lineage_hash], child.id, BUILTIN_SNAPSHOT_KIND).unwrap();
    assert_eq!(child.lineage_hash, expected_child);
    assert_ne!(root.lineage_hash, child.lineage_hash);
}

// ---- edges: acyclicity + allow-list ------------------------------------------

#[test]
fn cycle_creating_edge_is_rejected_and_graph_stays_acyclic() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();

    let a = make_snapshot(&mut h.svc, vec![], b"a");
    // b descends from a via the structural parent edge (b -> a).
    let b = make_snapshot(&mut h.svc, vec![a.id], b"b");

    // Adding a -> b would close the loop (a already an ancestor of b).
    let err = h
        .svc
        .add_edge(a.id, b.id, EdgeType::ParentChild)
        .unwrap_err();
    assert!(matches!(err, GraphError::Edge(EdgeError::Cycle { .. })));

    // The rejected edge left no trace: the projection is unchanged and a rebuild
    // from the log is identical.
    let before = h.svc.projection().canonical_digest().unwrap();
    let rebuilt = h.rebuild();
    assert_eq!(before, rebuilt.canonical_digest().unwrap());
}

#[test]
fn self_edge_is_a_cycle() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let a = make_snapshot(&mut h.svc, vec![], b"a");
    let err = h
        .svc
        .add_edge(a.id, a.id, EdgeType::ParentChild)
        .unwrap_err();
    assert!(matches!(err, GraphError::Edge(EdgeError::Cycle { .. })));
}

#[test]
fn transitive_cycle_is_rejected() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let a = make_snapshot(&mut h.svc, vec![], b"a");
    let b = make_snapshot(&mut h.svc, vec![a.id], b"b");
    let c = make_snapshot(&mut h.svc, vec![b.id], b"c");
    // a descends from ... nothing; c descends from b descends from a.
    // Adding a -> c would make c an ancestor of a, i.e. a cycle.
    let err = h
        .svc
        .add_edge(a.id, c.id, EdgeType::ParentChild)
        .unwrap_err();
    assert!(matches!(err, GraphError::Edge(EdgeError::Cycle { .. })));
}

#[test]
fn non_cycle_edge_is_accepted() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    h.svc
        .register_descriptor(validation_descriptor(Version::new(1, 0, 0)))
        .unwrap();

    let edit = make_snapshot(&mut h.svc, vec![], b"edit");
    let check = h
        .svc
        .create_node("validation", None, vec![], "main", json!({}), false, None)
        .unwrap();

    // A validation node VALIDATES the edit node (check -> edit): not a cycle.
    h.svc
        .add_edge(check.id, edit.id, EdgeType::Validates)
        .unwrap();

    let env = h.svc.get_node(check.id).unwrap().unwrap();
    // The validation node now has the edit as a parent via the Validates edge?
    // Validates is not a parent relation, but the structural parent of `check`
    // is none; the edge is recorded though.
    assert_eq!(env.kind, "validation");
}

#[test]
fn disallowed_edge_type_is_rejected() {
    let mut h = Harness::new();
    // A context "plan" descriptor that allows only ParentChild and DerivedFrom.
    h.svc
        .register_descriptor(context_descriptor("plan", Version::new(1, 0, 0)))
        .unwrap();
    h.svc.register_builtin_snapshot().unwrap();

    let plan = h
        .svc
        .create_node("plan", None, vec![], "main", json!({}), false, None)
        .unwrap();
    let snap = make_snapshot(&mut h.svc, vec![], b"s");

    // plan does not allow Validates.
    let err = h
        .svc
        .add_edge(plan.id, snap.id, EdgeType::Validates)
        .unwrap_err();
    assert!(matches!(
        err,
        GraphError::Edge(EdgeError::DisallowedEdgeType { .. })
    ));
}

#[test]
fn add_edge_with_missing_endpoint_is_rejected() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let a = make_snapshot(&mut h.svc, vec![], b"a");
    let ghost = Ulid::new();
    let err = h
        .svc
        .add_edge(a.id, ghost, EdgeType::ParentChild)
        .unwrap_err();
    assert!(matches!(err, GraphError::MissingNode { .. }));
    let err2 = h
        .svc
        .add_edge(ghost, a.id, EdgeType::ParentChild)
        .unwrap_err();
    assert!(matches!(err2, GraphError::MissingNode { .. }));
}

#[test]
fn create_node_with_missing_parent_is_rejected() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let ghost = Ulid::new();
    let err = h
        .svc
        .create_node(
            BUILTIN_SNAPSHOT_KIND,
            None,
            vec![ghost],
            "main",
            json!({ "origin": "manual" }),
            true,
            Some(hash_bytes(b"x")),
        )
        .unwrap_err();
    assert!(matches!(err, GraphError::MissingNode { .. }));
}

// ---- parent/child materialization + hot columns ------------------------------

#[test]
fn parent_child_relations_materialize_both_ways() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();

    let root = make_snapshot(&mut h.svc, vec![], b"root");
    let c1 = make_snapshot(&mut h.svc, vec![root.id], b"c1");
    let c2 = make_snapshot(&mut h.svc, vec![root.id], b"c2");

    let root_env = h.svc.get_node(root.id).unwrap().unwrap();
    assert_eq!(root_env.parent_ids, Vec::<Ulid>::new());
    assert_eq!(root_env.child_ids, vec![c1.id, c2.id]);

    let c1_env = h.svc.get_node(c1.id).unwrap().unwrap();
    assert_eq!(c1_env.parent_ids, vec![root.id]);
    assert!(c1_env.child_ids.is_empty());
}

#[test]
fn merge_parent_edge_is_a_symmetric_parent_child_relation() {
    // A merge node M has a structural parent P1 and a MERGE_PARENT edge to P2.
    // parent_ids must list both, and crucially M must appear in BOTH P1's and
    // P2's child_ids — otherwise a staleness walk down P2's child_ids would miss
    // the merge node when P2's snapshot changes (DESIGN §6.3, §7.3).
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();

    let p1 = make_snapshot(&mut h.svc, vec![], b"p1");
    let p2 = make_snapshot(&mut h.svc, vec![], b"p2");
    // M is created with P1 as its structural parent.
    let m = make_snapshot(&mut h.svc, vec![p1.id], b"merge");
    // The second parent is attached via a MERGE_PARENT edge (M -> P2).
    h.svc.add_edge(m.id, p2.id, EdgeType::MergeParent).unwrap();

    let m_env = h.svc.get_node(m.id).unwrap().unwrap();
    assert!(m_env.parent_ids.contains(&p1.id));
    assert!(m_env.parent_ids.contains(&p2.id));

    // Symmetry: M is a child of both parents.
    assert!(h
        .svc
        .get_node(p1.id)
        .unwrap()
        .unwrap()
        .child_ids
        .contains(&m.id));
    assert!(
        h.svc
            .get_node(p2.id)
            .unwrap()
            .unwrap()
            .child_ids
            .contains(&m.id),
        "merge node must appear in its merge-parent's child_ids"
    );

    // And the relation survives a from-log rebuild.
    let rebuilt = h.rebuild();
    assert!(rebuilt
        .get_node(p2.id)
        .unwrap()
        .unwrap()
        .child_ids
        .contains(&m.id));
}

#[test]
fn snapshot_node_has_status_passed_and_hash_present() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let snap = make_snapshot(&mut h.svc, vec![], b"s");
    let env = h.svc.get_node(snap.id).unwrap().unwrap();
    assert_eq!(env.status, Lifecycle::Passed);
    assert!(env.owns_snapshot);
    assert_eq!(env.snapshot_hash, Some(hash_bytes(b"s")));
    assert!(!env.is_stale);
}

#[test]
fn observing_node_starts_pending_and_owns_no_snapshot() {
    let mut h = Harness::new();
    h.svc
        .register_descriptor(validation_descriptor(Version::new(1, 0, 0)))
        .unwrap();
    let v = h
        .svc
        .create_node("validation", None, vec![], "main", json!({}), false, None)
        .unwrap();
    let env = h.svc.get_node(v.id).unwrap().unwrap();
    assert_eq!(env.status, Lifecycle::Pending);
    assert_eq!(env.family, Family::Observing);
    assert!(env.snapshot_hash.is_none());
}

#[test]
fn custom_kind_materializes_descriptor_family_not_a_kind_heuristic() {
    // A third party registers an observing type whose id is NOT one the
    // projection could classify from a kind-string list (e.g. "a11y_audit").
    // The materialized envelope must reflect the *descriptor's* family, proving
    // the family is the authoritative, payload-independent envelope contract
    // (DESIGN §6.2, §7.1; D-5) rather than a guess.
    let mut h = Harness::new();
    let observing = NodeTypeDescriptor {
        id: "a11y_audit".to_string(),
        type_version: Version::new(1, 0, 0),
        family: Family::Observing,
        owns_snapshot: false,
        payload_schema: json!({ "type": "object" }),
        result_schema: Some(json!({ "type": "object" })),
        allowed_edges: vec![EdgeType::Validates, EdgeType::ParentChild],
        ports: vec![],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec![],
        ui_contributions: json!({}),
        revoked_provenance: None,
    };
    h.svc.register_descriptor(observing).unwrap();

    let n = h
        .svc
        .create_node("a11y_audit", None, vec![], "main", json!({}), false, None)
        .unwrap();
    assert_eq!(n.family, Family::Observing);
    // And it survives a from-log rebuild identically (purity).
    let rebuilt = h.rebuild();
    assert_eq!(
        rebuilt.get_node(n.id).unwrap().unwrap().family,
        Family::Observing
    );
}

// ---- refs --------------------------------------------------------------------

#[test]
fn refs_create_and_move() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    let a = make_snapshot(&mut h.svc, vec![], b"a");
    let b = make_snapshot(&mut h.svc, vec![a.id], b"b");

    h.svc.create_ref("HEAD", RefKind::Head, a.id).unwrap();
    assert_eq!(h.svc.projection().ref_target("HEAD").unwrap(), Some(a.id));

    h.svc.move_ref("HEAD", b.id).unwrap();
    assert_eq!(h.svc.projection().ref_target("HEAD").unwrap(), Some(b.id));

    // Rebuild reproduces the moved ref.
    let rebuilt = h.rebuild();
    assert_eq!(rebuilt.ref_target("HEAD").unwrap(), Some(b.id));
}

#[test]
fn create_ref_to_missing_node_is_rejected() {
    let mut h = Harness::new();
    let err = h
        .svc
        .create_ref("HEAD", RefKind::Head, Ulid::new())
        .unwrap_err();
    assert!(matches!(err, GraphError::MissingNode { .. }));
}

// ---- effective_status totality ----------------------------------------------

#[test]
fn effective_status_defined_for_every_node() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    h.svc
        .register_descriptor(validation_descriptor(Version::new(1, 0, 0)))
        .unwrap();

    let snap = make_snapshot(&mut h.svc, vec![], b"s");
    let v = h
        .svc
        .create_node("validation", None, vec![], "main", json!({}), false, None)
        .unwrap();

    assert!(h.svc.effective_status(snap.id).unwrap().is_some());
    assert!(h.svc.effective_status(v.id).unwrap().is_some());
    // A non-existent node yields None.
    assert!(h.svc.effective_status(Ulid::new()).unwrap().is_none());
}

// ---- mixed-version resolution (descriptors retained) -------------------------

#[test]
fn node_under_v1_still_resolves_and_restores_after_v2_registered() {
    let mut h = Harness::new();
    h.svc
        .register_descriptor(context_descriptor("plan", Version::new(1, 0, 0)))
        .unwrap();

    // Create a node under v1 (resolve(None) picks the highest = 1.0.0).
    let n = h
        .svc
        .create_node("plan", None, vec![], "main", json!({ "v": 1 }), false, None)
        .unwrap();

    // Register v2; both versions remain resolvable.
    h.svc
        .register_descriptor(context_descriptor("plan", Version::new(2, 0, 0)))
        .unwrap();
    assert_eq!(
        h.svc.registry().resolve("plan", None).unwrap().type_version,
        Version::new(2, 0, 0)
    );
    assert!(h
        .svc
        .registry()
        .resolve("plan", Some(&Version::new(1, 0, 0)))
        .is_some());

    // The pre-existing node still resolves + restores identically from the log.
    let rebuilt = h.rebuild();
    let restored = rebuilt.get_node(n.id).unwrap().unwrap();
    assert_eq!(restored, h.svc.get_node(n.id).unwrap().unwrap());
}

// ---- lazy payload upgrade (no stored-row rewrite) ----------------------------

/// A v1 -> v2 migration that adds a defaulted `enabled` flag to a `plan` payload.
struct AddEnabled;
impl EventMigration for AddEnabled {
    fn event_type(&self) -> &str {
        "plan"
    }
    fn from_version(&self) -> u16 {
        1
    }
    fn to_version(&self) -> u16 {
        2
    }
    fn upgrade(&self, mut p: Value) -> std::result::Result<Value, MigrationError> {
        p.as_object_mut()
            .ok_or_else(|| MigrationError::Upgrade("expected object".into()))?
            .insert("enabled".into(), json!(true));
        Ok(p)
    }
}

#[test]
fn older_payload_reads_back_upgraded_without_rewriting_storage() {
    let mut migrations = MigrationRegistry::new();
    migrations.register(Box::new(AddEnabled)).unwrap();
    let mut h = Harness::with_migrations(migrations);
    h.svc
        .register_descriptor(context_descriptor("plan", Version::new(1, 0, 0)))
        .unwrap();

    // Create a node while the current payload version for "plan" is still 1
    // (the default), so the row is stored at v1.
    let n = h
        .svc
        .create_node(
            "plan",
            None,
            vec![],
            "main",
            json!({ "name": "x" }),
            false,
            None,
        )
        .unwrap();
    assert_eq!(n.payload_schema_version, 1);

    // The stored raw payload is exactly what was written (v1, no `enabled`).
    let (raw, raw_ver) = h.svc.projection().raw_payload(n.id).unwrap().unwrap();
    assert_eq!(raw, json!({ "name": "x" }));
    assert_eq!(raw_ver, 1);

    // Now declare that "plan"'s current payload version is 2. A read upgrades the
    // in-memory payload via the migration registry...
    h.svc.set_payload_current_version("plan", 2);
    let (payload, ver) = h.svc.get_payload(n.id).unwrap().unwrap();
    assert_eq!(ver, 2);
    assert_eq!(payload, json!({ "name": "x", "enabled": true }));

    // ...while the stored row is untouched (still v1, no `enabled`) — no history
    // rewrite. A rebuild from the log also keeps the stored bytes at v1.
    let (raw_after, ver_after) = h.svc.projection().raw_payload(n.id).unwrap().unwrap();
    assert_eq!(
        raw_after,
        json!({ "name": "x" }),
        "stored row must be unchanged"
    );
    assert_eq!(ver_after, 1);

    let rebuilt = h.rebuild();
    let (raw_rebuilt, ver_rebuilt) = rebuilt.raw_payload(n.id).unwrap().unwrap();
    assert_eq!(raw_rebuilt, json!({ "name": "x" }));
    assert_eq!(ver_rebuilt, 1);
}

/// Exercise an actual upgrade by composing the migration registry directly,
/// proving the lazy-upgrade mechanism the service uses produces an upgraded
/// payload while a from-log rebuild keeps the stored bytes intact.
#[test]
fn migration_registry_upgrades_stored_payload_in_memory_only() {
    let mut migrations = MigrationRegistry::new();
    migrations.register(Box::new(AddEnabled)).unwrap();

    let h = Harness::new();
    // Store a "plan" node at schema version 1 directly via the writer so we can
    // simulate an old stored generation, then fold it.
    use spork_graph::{NodeCreatedPayload, EVENT_NODE_CREATED};
    let node_id = Ulid::new();
    let created = NodeCreatedPayload {
        node_id,
        kind: "plan".to_string(),
        family: Family::Context,
        type_version: "1.0.0".to_string(),
        parent_ids: vec![],
        branch_id: "main".to_string(),
        owns_snapshot: false,
        snapshot_hash: None,
        payload: json!({ "name": "x" }),
        payload_schema_version: 1,
        lineage_hash: spork_graph::lineage_hash(&[], node_id, "plan").unwrap(),
        model: None,
    };
    let value = serde_json::to_value(&created).unwrap();
    h.log
        .writer()
        .append(spork_log::NewEvent::new(
            EVENT_NODE_CREATED,
            1,
            value,
            "test",
        ))
        .unwrap();

    let proj = h.rebuild();
    let (raw, stored_ver) = proj.raw_payload(node_id).unwrap().unwrap();
    assert_eq!(raw, json!({ "name": "x" }));
    assert_eq!(stored_ver, 1);

    // The registry upgrades the in-memory copy to v2 with `enabled` added.
    let (upgraded, ver) = migrations
        .upgrade_to_current("plan", stored_ver, 2, raw)
        .unwrap();
    assert_eq!(ver, 2);
    assert_eq!(upgraded, json!({ "name": "x", "enabled": true }));

    // The stored row is untouched (still v1, no `enabled`) — no history rewrite.
    let (raw_after, ver_after) = proj.raw_payload(node_id).unwrap().unwrap();
    assert_eq!(raw_after, json!({ "name": "x" }));
    assert_eq!(ver_after, 1);
}

// ---- projection == log purity (drop-and-rebuild identity) --------------------

#[test]
fn drop_and_rebuild_yields_identical_projection_digest() {
    let mut h = Harness::new();
    h.svc.register_builtin_snapshot().unwrap();
    h.svc
        .register_descriptor(validation_descriptor(Version::new(1, 0, 0)))
        .unwrap();

    let a = make_snapshot(&mut h.svc, vec![], b"a");
    let b = make_snapshot(&mut h.svc, vec![a.id], b"b");
    let c = make_snapshot(&mut h.svc, vec![b.id], b"c");
    let v = h
        .svc
        .create_node("validation", None, vec![], "main", json!({}), false, None)
        .unwrap();
    h.svc.add_edge(v.id, c.id, EdgeType::Validates).unwrap();
    h.svc.create_ref("HEAD", RefKind::Head, c.id).unwrap();
    h.svc.move_ref("HEAD", b.id).unwrap();

    let live = h.svc.projection().canonical_digest().unwrap();
    let rebuilt = h.rebuild().canonical_digest().unwrap();
    assert_eq!(live, rebuilt);

    // Rebuilding twice is deterministic.
    let rebuilt2 = h.rebuild().canonical_digest().unwrap();
    assert_eq!(rebuilt, rebuilt2);
}

#[test]
fn empty_projection_has_stable_digest() {
    let p1 = GraphProjection::open_in_memory().unwrap();
    let p2 = GraphProjection::open_in_memory().unwrap();
    assert_eq!(
        p1.canonical_digest().unwrap(),
        p2.canonical_digest().unwrap()
    );
}

// ---- helpers for property tests ---------------------------------------------

/// A scripted command for the randomized DAG/replay test.
#[derive(Debug, Clone)]
enum Cmd {
    /// Create a snapshot node whose parents are the given indices into the
    /// already-created node list.
    Create(Vec<usize>),
    /// Try to add a ParentChild edge from node index `a` to node index `b`.
    Edge(usize, usize),
}

fn cmd_strategy() -> impl Strategy<Value = Cmd> {
    prop_oneof![
        prop::collection::vec(0usize..8, 0..3).prop_map(Cmd::Create),
        (0usize..8, 0usize..8).prop_map(|(a, b)| Cmd::Edge(a, b)),
    ]
}

proptest! {
    /// A random script of create/edge commands keeps the graph a DAG and the
    /// projection replays from the log to an identical canonical digest.
    #[test]
    fn random_scripts_keep_dag_and_replay_identically(
        script in prop::collection::vec(cmd_strategy(), 0..40)
    ) {
        let mut h = Harness::new();
        h.svc.register_builtin_snapshot().unwrap();
        let mut nodes: Vec<Ulid> = Vec::new();
        let mut counter: u64 = 0;

        for cmd in script {
            match cmd {
                Cmd::Create(parent_idx) => {
                    // Map indices to existing node ids (skip out-of-range).
                    let parents: Vec<Ulid> = parent_idx
                        .iter()
                        .filter_map(|&i| nodes.get(i).copied())
                        .collect();
                    // Dedup parents to avoid a duplicate-parent edge (harmless,
                    // but keeps the structural relation a set).
                    let mut seen = std::collections::HashSet::new();
                    let parents: Vec<Ulid> =
                        parents.into_iter().filter(|p| seen.insert(*p)).collect();
                    counter += 1;
                    let tag = format!("n{counter}");
                    let env = make_snapshot(&mut h.svc, parents, tag.as_bytes());
                    nodes.push(env.id);
                }
                Cmd::Edge(a, b) => {
                    if let (Some(&from), Some(&to)) = (nodes.get(a), nodes.get(b)) {
                        // Add the edge; a cycle (or duplicate) is a legitimate
                        // rejection, never a panic. On success the graph stays a
                        // DAG by construction (the guard refuses cycles).
                        let _ = h.svc.add_edge(from, to, EdgeType::ParentChild);
                    }
                }
            }
        }

        // INVARIANT 1: the graph is acyclic. Verify by attempting a topological
        // order over the materialized parent relation; if it succeeds, no cycle.
        prop_assert!(is_acyclic(h.svc.projection()));

        // INVARIANT 2: projection == log. A from-scratch rebuild matches live.
        let live = h.svc.projection().canonical_digest().unwrap();
        let rebuilt = h.rebuild().canonical_digest().unwrap();
        prop_assert_eq!(live, rebuilt);
    }
}

/// A standalone acyclicity check over the projection's parent relation, used by
/// the property test as an independent witness (not the same code the guard
/// uses). Kahn-style: repeatedly remove nodes with no outstanding parents.
fn is_acyclic(proj: &GraphProjection) -> bool {
    use spork_edges::Adjacency;
    // Collect every node id by reading the state.
    let state = proj.state().unwrap();
    let ids: Vec<Ulid> = state
        .nodes
        .keys()
        .map(|s| Ulid::from_string(s).unwrap())
        .collect();

    // parents[node] = its direct parents.
    let mut remaining: std::collections::HashMap<Ulid, std::collections::HashSet<Ulid>> = ids
        .iter()
        .map(|&id| (id, proj.parents_of(id).into_iter().collect()))
        .collect();

    loop {
        // Find a node whose remaining parents are all already removed.
        let ready: Vec<Ulid> = remaining
            .iter()
            .filter(|(_, parents)| parents.is_empty())
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            // If nothing is ready but nodes remain, there is a cycle.
            return remaining.is_empty();
        }
        for id in ready {
            remaining.remove(&id);
            for parents in remaining.values_mut() {
                parents.remove(&id);
            }
        }
    }
}

// ---- lineage stability under hashing -----------------------------------------

#[test]
fn lineage_hash_is_canonical_and_deterministic() {
    let id = Ulid::new();
    let p1 = hash_bytes(b"p1");
    let p2 = hash_bytes(b"p2");
    let a = spork_graph::lineage_hash(&[p1, p2], id, "k").unwrap();
    let b = spork_graph::lineage_hash(&[p1, p2], id, "k").unwrap();
    assert_eq!(a, b);
    // Parent order matters (lineage is ordered).
    let c = spork_graph::lineage_hash(&[p2, p1], id, "k").unwrap();
    assert_ne!(a, c);
    // Kind matters.
    let d = spork_graph::lineage_hash(&[p1, p2], id, "other").unwrap();
    assert_ne!(a, d);
}

#[test]
fn empty_lineage_hash_is_a_known_value() {
    let id = Ulid::from_string("00000000000000000000000000").unwrap();
    let h: Hash = spork_graph::lineage_hash(&[], id, "snapshot").unwrap();
    // Recompute the long way to pin the document shape.
    let doc = json!({ "parents": [], "id": id.to_string(), "kind": "snapshot" });
    let bytes = spork_canon::canonicalize_value(&doc).unwrap();
    assert_eq!(h, hash_bytes(&bytes));
}
