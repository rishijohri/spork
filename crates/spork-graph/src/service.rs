//! The command / service layer: [`GraphService`].
//!
//! `GraphService` is the **only** way to mutate the typed graph. It holds a
//! [`NodeTypeRegistry`], the F1 [`WriterHandle`], and the materialized
//! [`GraphProjection`], and it enforces the **validate-then-append** contract
//! (DESIGN §6.1, §6.2, §6.3): every command first validates against the registry
//! and the current projection, and only on success appends an event through the
//! F1 single-writer actor. Nothing ever writes the projection directly — after a
//! successful append the service folds the new event into its own projection so
//! its in-memory view stays current, but the *truth* is the log, and dropping
//! and rebuilding the projection from the log is bit-for-bit identical (the
//! soundness guarantee this layer preserves).
//!
//! # What is validated
//!
//! * **create_node** — the `kind` resolves in the registry; the descriptor's
//!   `owns_snapshot` matches the request; a `snapshot_hash` is present **iff**
//!   `owns_snapshot` (DESIGN §6.2, §7.2). The lineage hash is computed from the
//!   parents' lineage hashes plus the node's identity (DESIGN §6.3).
//! * **add_edge** — both endpoints exist; the edge type is in the `from`-node
//!   descriptor's `allowed_edges`; adding it does **not** create a cycle against
//!   the projection (the graph is always a DAG, DESIGN §6.3).
//!
//! # Lazy payload upgrade (CLAUDE.md C5)
//!
//! [`get_node`](GraphService::get_node) reads the projected envelope and upgrades
//! the *payload* via `spork-migrate` if it was stored under an older
//! `payload_schema_version`, **without** rewriting storage (DESIGN §7.2).
//!
//! # Dogfooded built-in
//!
//! [`register_builtin_snapshot`](GraphService::register_builtin_snapshot)
//! registers the one built-in `"snapshot"` descriptor through the **public**
//! registry path — exactly as a third party would, no special case (DESIGN §6.5,
//! §7.1, §9).

use std::collections::HashMap;
use std::sync::Arc;

use semver::Version;
use serde_json::json;
use spork_edges::{would_create_cycle, EdgeError, EdgeType, RefKind};
use spork_hash::Hash;
use spork_log::{NewEvent, WriterHandle};
use spork_migrate::MigrationRegistry;
use spork_registry::{
    Family, NodeTypeDescriptor, NodeTypeRegistry, Port, PortDirection, PortKind, StalenessRule,
};
use spork_status::{effective_status, EffectiveStatus};
use ulid::Ulid;

use crate::envelope::NodeEnvelope;
use crate::error::{GraphError, Result};
use crate::events::{
    EdgeAddedPayload, NodeCreatedPayload, RefCreatedPayload, RefMovedPayload,
    GRAPH_EVENT_SCHEMA_VERSION,
};
use crate::lineage::lineage_hash;
use crate::projection::GraphProjection;

/// The id (`kind`) of the one dogfooded built-in node type.
pub const BUILTIN_SNAPSHOT_KIND: &str = "snapshot";

/// The actor string the service stamps on the events it appends.
const SERVICE_ACTOR: &str = "spork-graph";

/// The validating command layer over the typed graph.
///
/// Construct with [`new`](GraphService::new) (supplying a writer handle, an
/// optional migration registry, and a rebuilt projection) or with
/// [`open_in_memory`](GraphService::open_in_memory) for the common in-memory
/// case. Mutating commands ([`create_node`](GraphService::create_node),
/// [`add_edge`](GraphService::add_edge), [`create_ref`](GraphService::create_ref),
/// [`move_ref`](GraphService::move_ref)) validate then append; read commands
/// ([`get_node`](GraphService::get_node),
/// [`effective_status`](GraphService::effective_status)) go against the
/// projection (with lazy payload upgrade for the payload).
///
/// The service is inherently single-threaded — it owns a `rusqlite::Connection`
/// (the projection) which is not `Sync` — so the `Arc<MigrationRegistry>` (whose
/// `Box<dyn EventMigration>` steps are not `Send`/`Sync`) is shared only within
/// this thread, matching the `spork-log` reader API's own `Arc` idiom.
pub struct GraphService {
    registry: NodeTypeRegistry,
    writer: WriterHandle,
    projection: GraphProjection,
    migrations: Arc<MigrationRegistry>,
    /// Per-kind current *payload* schema version. A kind absent from the map is
    /// treated as current version [`GRAPH_EVENT_SCHEMA_VERSION`] (its payload has
    /// not evolved). [`get_payload`](GraphService::get_payload) upgrades a stored
    /// payload below this through the migration registry, on read, without
    /// rewriting storage (CLAUDE.md C5, DESIGN §7.2).
    payload_current_versions: HashMap<String, u16>,
}

impl GraphService {
    /// Construct a service over an existing writer handle and projection.
    ///
    /// `migrations` supplies forward payload migrations for lazy upgrade-on-read;
    /// pass an empty registry if no payload schema has evolved yet. The
    /// `projection` should already reflect the log (e.g. via
    /// [`GraphProjection::rebuild_from_log`]); the service keeps it current by
    /// folding each event it appends.
    pub fn new(
        writer: WriterHandle,
        projection: GraphProjection,
        migrations: Arc<MigrationRegistry>,
    ) -> Self {
        GraphService {
            registry: NodeTypeRegistry::new(),
            writer,
            projection,
            migrations,
            payload_current_versions: HashMap::new(),
        }
    }

    /// Construct a service with a fresh in-memory projection and no migrations.
    ///
    /// A convenience for the common case (and for tests): the caller still
    /// supplies the F1 [`WriterHandle`], which is the single write path. The
    /// in-memory projection starts empty; if the log already has events, prefer
    /// [`new`](GraphService::new) with a rebuilt projection.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] if the in-memory projection cannot be created.
    // The `Arc<MigrationRegistry>` is shared only within this single-threaded
    // service (see the struct-level note); the lint does not apply.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn open_in_memory(writer: WriterHandle) -> Result<Self> {
        Ok(GraphService {
            registry: NodeTypeRegistry::new(),
            writer,
            projection: GraphProjection::open_in_memory()?,
            migrations: Arc::new(MigrationRegistry::new()),
            payload_current_versions: HashMap::new(),
        })
    }

    /// Declare the current *payload* schema version for a node `kind`.
    ///
    /// New nodes of `kind` are written at this version, and
    /// [`get_payload`](GraphService::get_payload) upgrades any stored payload
    /// below it through the migration registry on read (CLAUDE.md C5). A kind
    /// that is never declared defaults to [`GRAPH_EVENT_SCHEMA_VERSION`]. This is
    /// the seam through which a node type's payload evolves additively without a
    /// flag day (DESIGN §7.2).
    pub fn set_payload_current_version(&mut self, kind: &str, version: u16) {
        self.payload_current_versions
            .insert(kind.to_string(), version);
    }

    /// Borrow the node-type registry (read-only).
    #[must_use]
    pub fn registry(&self) -> &NodeTypeRegistry {
        &self.registry
    }

    /// Borrow the materialized projection (read-only).
    #[must_use]
    pub fn projection(&self) -> &GraphProjection {
        &self.projection
    }

    /// Register a node-type descriptor through the public registry path.
    ///
    /// This is the same call third parties use — built-ins (see
    /// [`register_builtin_snapshot`](GraphService::register_builtin_snapshot))
    /// go through it too, with no special path (DESIGN §6.5, §7.1).
    ///
    /// # Errors
    ///
    /// Propagates [`spork_registry::RegistryError`] — notably
    /// `OwnsSnapshotMismatch` (a descriptor claiming `owns_snapshot` with no
    /// `SnapshotRef` out-port) and `DuplicateVersion`.
    pub fn register_descriptor(
        &mut self,
        descriptor: NodeTypeDescriptor,
    ) -> std::result::Result<(), spork_registry::RegistryError> {
        self.registry.register(descriptor)
    }

    /// Register the one dogfooded built-in `"snapshot"` descriptor.
    ///
    /// `snapshot` is a [`Family::Mutating`] type that `owns_snapshot` and exposes
    /// a [`PortKind::SnapshotRef`] out-port — so it satisfies the registry's
    /// contentRef rule and is accepted exactly as a third-party type would be.
    /// This dogfoods the extension point rather than special-casing the built-in
    /// (DESIGN §6.5, §7.1, §9).
    ///
    /// # Errors
    ///
    /// Propagates [`spork_registry::RegistryError`] (e.g. if a `snapshot@1.0.0`
    /// is already registered).
    pub fn register_builtin_snapshot(
        &mut self,
    ) -> std::result::Result<(), spork_registry::RegistryError> {
        self.register_descriptor(builtin_snapshot_descriptor())
    }

    /// Create a node: validate against the registry, then append
    /// `graph.node_created`.
    ///
    /// Validation, in order:
    /// 1. `kind` (at `type_version`, or the highest registered version if `None`)
    ///    resolves in the registry, else [`GraphError::UnknownKind`].
    /// 2. the descriptor's `owns_snapshot` equals the `owns_snapshot` argument,
    ///    and a `snapshot_hash` is present **iff** `owns_snapshot`, else
    ///    [`GraphError::OwnsSnapshotMismatch`].
    /// 3. every parent exists, else [`GraphError::MissingNode`].
    ///
    /// On success the lineage hash is computed from the parents' lineage hashes
    /// plus the new node's id and kind (DESIGN §6.3), the `graph.node_created`
    /// event is appended through the F1 writer, and the event is folded into the
    /// projection. The fully-materialized [`NodeEnvelope`] is returned.
    ///
    /// # Errors
    ///
    /// See the validation list above, plus [`GraphError::Log`] /
    /// [`GraphError::Canon`] on an append/encoding failure.
    #[allow(clippy::too_many_arguments)]
    pub fn create_node(
        &mut self,
        kind: &str,
        type_version: Option<&Version>,
        parent_ids: Vec<Ulid>,
        branch_id: &str,
        payload: serde_json::Value,
        owns_snapshot: bool,
        snapshot_hash: Option<Hash>,
    ) -> Result<NodeEnvelope> {
        // (1) Resolve the type in the registry.
        let descriptor =
            self.registry
                .resolve(kind, type_version)
                .ok_or_else(|| GraphError::UnknownKind {
                    id: match type_version {
                        Some(v) => format!("{kind}@{v}"),
                        None => kind.to_string(),
                    },
                })?;
        let resolved_version = descriptor.type_version.clone();
        // The family is taken from the resolved descriptor — the single source of
        // truth — and recorded on the event so the projection never has to guess
        // it from the kind string (DESIGN §6.2, §7.1; D-5).
        let resolved_family = descriptor.family;
        let payload_schema_version = self.payload_schema_version_for(kind);

        // (2) owns_snapshot must agree with the descriptor, and snapshot_hash
        // must be present iff owns_snapshot (the contentRef rule, DESIGN §7.2).
        if owns_snapshot != descriptor.owns_snapshot || owns_snapshot != snapshot_hash.is_some() {
            return Err(GraphError::OwnsSnapshotMismatch);
        }

        // (3) Every parent must already exist.
        let mut parent_lineages = Vec::with_capacity(parent_ids.len());
        for parent in &parent_ids {
            match self.projection.get_node(*parent)? {
                Some(env) => parent_lineages.push(env.lineage_hash),
                None => return Err(GraphError::MissingNode { id: *parent }),
            }
        }

        // Compute identity + lineage, then append.
        let node_id = Ulid::new();
        let lineage = lineage_hash(&parent_lineages, node_id, kind)?;

        let model = extract_model(&payload);
        let created = NodeCreatedPayload {
            node_id,
            kind: kind.to_string(),
            family: resolved_family,
            type_version: resolved_version.to_string(),
            parent_ids: parent_ids.clone(),
            branch_id: branch_id.to_string(),
            owns_snapshot,
            snapshot_hash,
            payload,
            payload_schema_version,
            lineage_hash: lineage,
            model,
        };

        let event = self.append(crate::events::EVENT_NODE_CREATED, &created)?;
        self.projection.apply(&event)?;

        // The materialized envelope is the authoritative read-back.
        self.projection
            .get_node(node_id)?
            .ok_or(GraphError::MissingNode { id: node_id })
    }

    /// Add an edge: validate then append `graph.edge_added`.
    ///
    /// Validation, in order:
    /// 1. both `from` and `to` exist, else [`GraphError::MissingNode`].
    /// 2. `edge_type` is in the `from`-node descriptor's `allowed_edges`, else
    ///    [`GraphError::Edge`] wrapping
    ///    [`EdgeError::DisallowedEdgeType`](spork_edges::EdgeError::DisallowedEdgeType).
    /// 3. adding `from → to` does not create a cycle against the projection, else
    ///    [`GraphError::Edge`] wrapping
    ///    [`EdgeError::Cycle`](spork_edges::EdgeError::Cycle). The graph is always
    ///    a DAG (DESIGN §6.3).
    ///
    /// # Errors
    ///
    /// See the validation list above, plus [`GraphError::Log`] /
    /// [`GraphError::Canon`] on an append failure.
    pub fn add_edge(&mut self, from: Ulid, to: Ulid, edge_type: EdgeType) -> Result<()> {
        // (1) Both endpoints must exist.
        let from_env = self
            .projection
            .get_node(from)?
            .ok_or(GraphError::MissingNode { id: from })?;
        if !self.projection.node_exists(to)? {
            return Err(GraphError::MissingNode { id: to });
        }

        // (2) The edge type must be allowed by the from-node's descriptor. The
        // descriptor is resolved at the from-node's kind (highest version).
        let descriptor =
            self.registry
                .resolve(&from_env.kind, None)
                .ok_or_else(|| GraphError::UnknownKind {
                    id: from_env.kind.clone(),
                })?;
        if !descriptor.allowed_edges.contains(&edge_type) {
            return Err(GraphError::Edge(EdgeError::DisallowedEdgeType {
                kind: from_env.kind.clone(),
                edge: edge_type,
            }));
        }

        // (3) The edge must not create a cycle against the current projection.
        if would_create_cycle(&self.projection, from, to) {
            return Err(GraphError::Edge(EdgeError::Cycle { from, to }));
        }

        let payload = EdgeAddedPayload {
            from,
            to,
            edge_type,
        };
        let event = self.append(crate::events::EVENT_EDGE_ADDED, &payload)?;
        self.projection.apply(&event)?;
        Ok(())
    }

    /// Create a ref (a GC root) pointing at a node: validate then append
    /// `graph.ref_created`.
    ///
    /// # Errors
    ///
    /// - [`GraphError::MissingNode`] if `to` does not exist.
    /// - [`GraphError::Log`] / [`GraphError::Canon`] on an append failure.
    pub fn create_ref(&mut self, name: &str, kind: RefKind, to: Ulid) -> Result<()> {
        if !self.projection.node_exists(to)? {
            return Err(GraphError::MissingNode { id: to });
        }
        let payload = RefCreatedPayload {
            name: name.to_string(),
            kind,
            to,
        };
        let event = self.append(crate::events::EVENT_REF_CREATED, &payload)?;
        self.projection.apply(&event)?;
        Ok(())
    }

    /// Move an existing ref to a new node: validate then append
    /// `graph.ref_moved`.
    ///
    /// # Errors
    ///
    /// - [`GraphError::MissingNode`] if `to` does not exist.
    /// - [`GraphError::Log`] / [`GraphError::Canon`] on an append failure.
    pub fn move_ref(&mut self, name: &str, to: Ulid) -> Result<()> {
        if !self.projection.node_exists(to)? {
            return Err(GraphError::MissingNode { id: to });
        }
        let payload = RefMovedPayload {
            name: name.to_string(),
            to,
        };
        let event = self.append(crate::events::EVENT_REF_MOVED, &payload)?;
        self.projection.apply(&event)?;
        Ok(())
    }

    /// Read a node's [`NodeEnvelope`] from the projection.
    ///
    /// This reads the materialized envelope (the payload-independent contract).
    /// The envelope's `payload_schema_version` reflects the **stored** version;
    /// to read the upgraded *payload*, use
    /// [`get_payload`](GraphService::get_payload), which applies lazy
    /// upgrade-on-read without rewriting storage (CLAUDE.md C5).
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query/decode failure.
    pub fn get_node(&self, id: Ulid) -> Result<Option<NodeEnvelope>> {
        self.projection.get_node(id)
    }

    /// Read a node's payload, upgraded lazily to the current schema version.
    ///
    /// The stored payload row is **not** rewritten: the upgrade happens on the
    /// in-memory value via the migration registry (DESIGN §7.2, CLAUDE.md C5).
    /// Returns the upgraded payload paired with the version it now conforms to.
    /// If no migration is registered for the node's kind, the stored payload is
    /// returned as-is.
    ///
    /// # Errors
    ///
    /// - [`GraphError::Log`] on a query/decode failure.
    /// - [`GraphError::Log`] (wrapping a migration error) if a configured
    ///   upgrade fails.
    pub fn get_payload(&self, id: Ulid) -> Result<Option<(serde_json::Value, u16)>> {
        let Some((payload, stored_version)) = self.projection.raw_payload(id)? else {
            return Ok(None);
        };
        let Some(env) = self.projection.get_node(id)? else {
            return Ok(None);
        };
        let current = self.payload_schema_version_for(&env.kind);
        let (upgraded, version) = self
            .migrations
            .upgrade_to_current(&env.kind, stored_version, current, payload)
            .map_err(|e| GraphError::Log(format!("payload upgrade for {}: {e}", env.kind)))?;
        Ok(Some((upgraded, version)))
    }

    /// The one UI status token for a node, folding status + staleness.
    ///
    /// Returns `None` if the node does not exist; otherwise exactly one
    /// [`EffectiveStatus`] via `spork-status` (DESIGN §7.3). Total over every
    /// `(Lifecycle, is_stale)` pair, including `Cancelled`.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Log`] on a query failure.
    pub fn effective_status(&self, id: Ulid) -> Result<Option<EffectiveStatus>> {
        Ok(self
            .projection
            .get_node(id)?
            .map(|env| effective_status(env.status, env.is_stale)))
    }

    /// The current payload schema version the service considers current for a
    /// kind.
    ///
    /// Defaults to [`GRAPH_EVENT_SCHEMA_VERSION`] (one payload generation) unless
    /// the caller declared a higher version via
    /// [`set_payload_current_version`](GraphService::set_payload_current_version).
    /// The migration registry upgrades any stored payload below this on read; a
    /// kind with no registered migration simply reads its stored payload
    /// unchanged.
    fn payload_schema_version_for(&self, kind: &str) -> u16 {
        self.payload_current_versions
            .get(kind)
            .copied()
            .unwrap_or(GRAPH_EVENT_SCHEMA_VERSION)
    }

    /// Append a typed payload as an event through the F1 writer.
    fn append<T: serde::Serialize>(
        &self,
        event_type: &str,
        payload: &T,
    ) -> Result<spork_log::Event> {
        let value = serde_json::to_value(payload)
            .map_err(|e| GraphError::Canon(format!("serialize {event_type} payload: {e}")))?;
        let new_event = NewEvent::new(event_type, GRAPH_EVENT_SCHEMA_VERSION, value, SERVICE_ACTOR);
        Ok(self.writer.append(new_event)?)
    }
}

/// Pull an optional `"model"` string out of a node payload, if present.
///
/// The envelope's `model` is convenience attribution; a payload that records the
/// model used surfaces it onto the envelope column without the caller having to
/// pass it separately.
fn extract_model(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Build the one dogfooded built-in `"snapshot"` descriptor (DESIGN §7.1).
///
/// A [`Family::Mutating`] type that `owns_snapshot` and exposes a
/// [`PortKind::SnapshotRef`] out-port (so it satisfies the registry's contentRef
/// rule) and may originate the structural mutating-graph edges.
fn builtin_snapshot_descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: BUILTIN_SNAPSHOT_KIND.to_string(),
        type_version: Version::new(1, 0, 0),
        family: Family::Mutating,
        owns_snapshot: true,
        payload_schema: json!({
            "type": "object",
            "properties": {
                "origin": { "enum": ["auto_drift", "manual", "import"] },
                "driftSource": { "type": "string" }
            },
            "required": ["origin"]
        }),
        result_schema: None,
        allowed_edges: vec![
            EdgeType::ParentChild,
            EdgeType::Branch,
            EdgeType::DerivedFrom,
            EdgeType::MergeParent,
        ],
        ports: vec![Port {
            name: "snapshot".to_string(),
            direction: PortDirection::Out,
            kind: PortKind::SnapshotRef,
            schema: json!({ "type": "string", "description": "content-addressed tree hash" }),
        }],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec!["snapshot.write".to_string()],
        ui_contributions: json!({ "color": "#4f8cff", "icon": "camera", "displayName": "Snapshot" }),
        revoked_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_snapshot_descriptor_satisfies_the_contentref_rule() {
        let d = builtin_snapshot_descriptor();
        assert_eq!(d.id, BUILTIN_SNAPSHOT_KIND);
        assert_eq!(d.family, Family::Mutating);
        assert!(d.owns_snapshot);
        // The contentRef rule: an owns_snapshot type exposes a SnapshotRef
        // out-port (DESIGN §7.2). The registry would reject it otherwise.
        assert!(d.has_snapshot_out_port());
        // It registers cleanly through the public registry path.
        let mut reg = NodeTypeRegistry::new();
        reg.register(d).unwrap();
    }

    #[test]
    fn extract_model_reads_optional_model_field() {
        assert_eq!(
            extract_model(&json!({ "model": "claude" })),
            Some("claude".to_string())
        );
        assert_eq!(extract_model(&json!({})), None);
        assert_eq!(extract_model(&json!({ "model": 5 })), None);
    }
}
