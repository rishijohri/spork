//! Spork node-type registry.
//!
//! Every node kind in Spork — both the built-in taxonomy and any user-defined
//! type — is described by a versioned [`NodeTypeDescriptor`] and registered
//! through one shared [`NodeTypeRegistry`]. A descriptor declares its family
//! (mutating / observing / context), its typed ports, its payload and result
//! schemas, the edge types it may originate, its staleness rule, and whether it
//! owns a snapshot. The registry enforces the contract at registration time
//! (e.g. an `owns_snapshot` type must expose a snapshot-ref out-port) and
//! retains every registered version, so older versions stay resolvable after
//! newer ones are added (mixed-version resolution).
//!
//! This realizes the extensibility and type-registry model in DESIGN.md §6.5
//! ("Merge and extensibility"), the built-in taxonomy in DESIGN.md §7.1 ("The
//! built-in taxonomy") and the mutating/observing split in §7.2 ("The node
//! envelope and the mutating/observing split"), the capabilities and
//! typed-ports model in DESIGN.md §9.1 ("The two-plane split and the tiered
//! executor model") and §9.2 ("Capabilities, replayable derivations, and typed
//! ports"), and the schema-driven node types in DESIGN.md §14.5 ("Selection,
//! Diffs & Schema-Driven Node Types").
//!
//! # The contract the registry enforces
//!
//! * **`owns_snapshot` implies a content ref (DESIGN §7.2).** A mutating type
//!   that claims `owns_snapshot` must produce a content-addressed snapshot
//!   reference, expressed here as an *out* [`Port`] of kind
//!   [`PortKind::SnapshotRef`]. A descriptor that claims `owns_snapshot` without
//!   such a port is "a malformed custom type that claims `ownsSnapshot` without
//!   producing a `contentRef`" and is **rejected at registration time**
//!   ([`RegistryError::OwnsSnapshotMismatch`]).
//! * **Versioned, never replaced (DESIGN §9.2).** "Every `DagNode` stores its
//!   `typeVersion` so the UI can render mixed versions." Registering a new
//!   version of an existing `id` *adds* a descriptor; it never overwrites an
//!   older one. A node created under `id@1.0.0` therefore still resolves and
//!   restores after `id@2.0.0` is registered. Re-registering the *exact* same
//!   `(id, version)` is rejected ([`RegistryError::DuplicateVersion`]).
//! * **Closed engine, open extension (DESIGN §6.5, §7.1, §14.5).** "All of these
//!   are registered through the same `NodeTypeRegistry` that user-defined types
//!   use — we dogfood the extension point rather than special-casing built-ins."
//!   This crate offers exactly one public registration path; built-ins go
//!   through it like anyone else.
//!
//! # Schema versioning (C5)
//!
//! [`NodeTypeDescriptor`] and its component value types are persisted /
//! shareable artifacts (a descriptor is "data, not a frontend release",
//! DESIGN §14.5), so the crate carries an explicit [`DESCRIPTOR_SCHEMA_VERSION`]
//! tagging the wire form. The per-type `type_version` ([`semver::Version`]) is a
//! *separate* axis: it versions the node *type's* payload/result/port contract,
//! while [`DESCRIPTOR_SCHEMA_VERSION`] versions the descriptor envelope itself.
//!
//! # Example
//!
//! ```
//! use semver::Version;
//! use serde_json::json;
//! use spork_edges::EdgeType;
//! use spork_registry::{
//!     Family, NodeTypeDescriptor, NodeTypeRegistry, Port, PortDirection, PortKind,
//!     StalenessRule,
//! };
//!
//! // A mutating "snapshot" type that owns a snapshot must expose a SnapshotRef
//! // out-port — exactly the rule a third-party type obeys.
//! let descriptor = NodeTypeDescriptor {
//!     id: "snapshot".into(),
//!     type_version: Version::new(1, 0, 0),
//!     family: Family::Mutating,
//!     owns_snapshot: true,
//!     payload_schema: json!({ "type": "object" }),
//!     result_schema: None,
//!     allowed_edges: vec![EdgeType::ParentChild],
//!     ports: vec![Port {
//!         name: "snapshot".into(),
//!         direction: PortDirection::Out,
//!         kind: PortKind::SnapshotRef,
//!         schema: json!({ "type": "string" }),
//!     }],
//!     staleness_rule: StalenessRule::WhenAncestorChanges,
//!     capabilities_required: vec!["snapshot.write".into()],
//!     ui_contributions: json!({}),
//!     revoked_provenance: None,
//! };
//!
//! let mut registry = NodeTypeRegistry::new();
//! registry.register(descriptor).unwrap();
//!
//! // Resolve the highest version with `None`.
//! let resolved = registry.resolve("snapshot", None).unwrap();
//! assert_eq!(resolved.type_version, Version::new(1, 0, 0));
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;

use semver::Version;
use serde::{Deserialize, Serialize};
use spork_edges::EdgeType;
use thiserror::Error;

/// Schema version of the persisted [`NodeTypeDescriptor`] envelope.
///
/// Per constraint C5, every persisted/shareable struct carries a schema version
/// with a registered forward migration from its first commit. This tags the
/// descriptor *envelope* wire form (DESIGN §14.5: a descriptor is "data, not a
/// frontend release"). It is **distinct** from a descriptor's per-type
/// [`NodeTypeDescriptor::type_version`], which versions the node type's
/// payload/result/port contract (DESIGN §9.2).
pub const DESCRIPTOR_SCHEMA_VERSION: u16 = 1;

/// The family a node type belongs to.
///
/// The taxonomy is organized into three families that share a common node
/// envelope but differ in their relationship to the codebase (DESIGN §6.2,
/// §7.1):
///
/// * [`Mutating`](Family::Mutating) — owns a snapshot (`owns_snapshot = true`);
///   clicking one materializes the exact codebase state and it can be a branch
///   point (Codebase-Edit, Snapshot, Merge).
/// * [`Observing`](Family::Observing) — attaches durable, comparable results to
///   a parent's snapshot and never mutates it (Validation/Test, Stress-Test,
///   Sanity/Pattern-Check). Spork's white-space differentiator.
/// * [`Context`](Family::Context) — carries no snapshot at all; feeds
///   handoff-document generation and per-node model attribution (Plan,
///   Conversation).
///
/// `Family` is part of the [`NodeTypeDescriptor`] contract and is surfaced on
/// the node envelope so the engine can branch *generically* on
/// `owns_snapshot` rather than per-kind (DESIGN §7.2). Its serialized form is
/// `snake_case` so it is stable and human-legible in stored descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// Owns a snapshot; clicking restores exact codebase state. Can branch.
    Mutating,
    /// Attaches append-only results to a parent's snapshot; never mutates it.
    Observing,
    /// Carries no snapshot; feeds handoff generation and model attribution.
    Context,
}

impl Family {
    /// Every family, in declaration order.
    ///
    /// The frozen family set (DESIGN §6.2, §7.1). Iterating it lets UIs build a
    /// legend without hardcoding the list and lets tests assert the set is
    /// stable.
    pub const ALL: [Family; 3] = [Family::Mutating, Family::Observing, Family::Context];

    /// The stable, persisted `snake_case` tag for this family.
    ///
    /// Matches the serde representation; provided as a `const fn` so callers can
    /// build messages and tables without a serializer round-trip.
    #[must_use]
    pub const fn as_tag(self) -> &'static str {
        match self {
            Family::Mutating => "mutating",
            Family::Observing => "observing",
            Family::Context => "context",
        }
    }
}

impl core::fmt::Display for Family {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_tag())
    }
}

/// The direction of a typed [`Port`] relative to its node.
///
/// Nodes chain (Edit → Sanity → Validation → Stress), so each type declares
/// typed input/output ports validated at the edges (DESIGN §9.2). [`In`] ports
/// declare what a node consumes; [`Out`] ports declare what it produces. The
/// `owns_snapshot` content-ref rule is expressed as an [`Out`] port of kind
/// [`PortKind::SnapshotRef`] (see [`NodeTypeRegistry::register`]).
///
/// [`In`]: PortDirection::In
/// [`Out`]: PortDirection::Out
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortDirection {
    /// An input the node consumes.
    In,
    /// An output the node produces.
    Out,
}

impl PortDirection {
    /// Every port direction, in declaration order.
    pub const ALL: [PortDirection; 2] = [PortDirection::In, PortDirection::Out];
}

/// The kind of value a typed [`Port`] carries.
///
/// "A JSON-Schema port system, plus a special `snapshotRef` port kind carrying
/// a content-addressed tree hash" (DESIGN §9.2):
///
/// * [`Json`](PortKind::Json) — an ordinary JSON value validated against the
///   port's `schema`.
/// * [`SnapshotRef`](PortKind::SnapshotRef) — the special content-addressed
///   tree-hash reference. A mutating type that owns a snapshot expresses its
///   `contentRef` as an [`PortDirection::Out`] port of this kind; the registry
///   enforces that link at registration (DESIGN §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    /// An ordinary JSON value validated against the port `schema`.
    Json,
    /// A content-addressed tree-hash reference (the `snapshotRef` port kind).
    SnapshotRef,
}

impl PortKind {
    /// Every port kind, in declaration order.
    pub const ALL: [PortKind; 2] = [PortKind::Json, PortKind::SnapshotRef];
}

/// A single typed input or output port on a node type.
///
/// Ports are how node types chain safely: the UI offers only valid attachments,
/// the agent reasons about which prior-node context it needs, and artifacts stay
/// comparable across branches (DESIGN §9.2). Each port has a stable `name`, a
/// [`PortDirection`], a [`PortKind`], and a JSON-Schema (`schema`) describing the
/// value it carries.
///
/// A descriptor with `owns_snapshot == true` must include at least one
/// [`PortDirection::Out`] port of kind [`PortKind::SnapshotRef`]; see
/// [`NodeTypeRegistry::register`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// The port's stable, type-local name (e.g. `"snapshot"`, `"report"`).
    pub name: String,
    /// Whether this is an input or output port.
    pub direction: PortDirection,
    /// The kind of value the port carries.
    pub kind: PortKind,
    /// The JSON Schema the port value is validated against.
    pub schema: serde_json::Value,
}

/// When a node of this type becomes stale.
///
/// Staleness is the second, orthogonal axis of a node's lifecycle (DESIGN §7.3):
/// a result is *stale* when it no longer reflects the current snapshot, even if
/// the run itself passed. The rule a type declares lets the engine cheaply
/// invalidate a subtree by walking `child_ids` on a mutating-node change
/// *without* recomputing outcomes. This is a small, deliberately frozen enum:
///
/// * [`Never`](StalenessRule::Never) — results never go stale (e.g. a pure
///   Context type with no snapshot dependency).
/// * [`WhenAncestorChanges`](StalenessRule::WhenAncestorChanges) — stale when an
///   ancestor's snapshot changes (the default for snapshot-bound results).
/// * [`WhenInputsChange`](StalenessRule::WhenInputsChange) — stale when this
///   node's declared inputs change (finer-grained than ancestor walking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StalenessRule {
    /// Results of this type never become stale.
    Never,
    /// Stale when an ancestor's snapshot changes.
    WhenAncestorChanges,
    /// Stale when this node's declared inputs change.
    WhenInputsChange,
}

impl StalenessRule {
    /// Every staleness rule, in declaration order.
    pub const ALL: [StalenessRule; 3] = [
        StalenessRule::Never,
        StalenessRule::WhenAncestorChanges,
        StalenessRule::WhenInputsChange,
    ];
}

/// A reserved provenance-revocation flag (DESIGN §9.2; reserved for P8).
///
/// Marketplace trust includes the ability to *revoke* a node type's provenance
/// (e.g. a signing key compromise) so dependents can be flagged. The field is
/// **reserved**: it is constructible and serializes as part of the descriptor
/// envelope, but it is not consulted anywhere in Phase F2 — it exists so the
/// persisted [`NodeTypeDescriptor`] shape is forward-compatible without a schema
/// bump when revocation lands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationFlag {
    /// A human-readable reason the type's provenance was revoked.
    pub reason: String,
}

/// A versioned, declarative description of one node type.
///
/// This is the "data, not a frontend release" artifact (DESIGN §14.5): a node
/// type is fully described by this struct, which the registry validates and
/// retains, and which the Node-Details panel, legend, and node card render. The
/// engine stays closed to modification but open to extension because both
/// built-ins and user-defined types are described by — and registered through —
/// this single contract (DESIGN §6.5, §7.1).
///
/// # Fields
///
/// * `id` — the stable type identifier (the node `kind`); shared across all
///   versions of a type.
/// * `type_version` — the semver version of this type's payload/result/port
///   contract. Bumping the major version is a breaking schema change (DESIGN
///   §9.2). Distinct from [`DESCRIPTOR_SCHEMA_VERSION`].
/// * `family` — the node [`Family`] (DESIGN §6.2, §7.1).
/// * `owns_snapshot` — whether nodes of this type own a content-addressed
///   snapshot. If `true`, the type **must** expose an [`PortDirection::Out`]
///   port of kind [`PortKind::SnapshotRef`] (DESIGN §7.2); enforced by
///   [`NodeTypeRegistry::register`].
/// * `payload_schema` — JSON Schema for the node payload (DESIGN §6.2).
/// * `result_schema` — JSON Schema for attached results, if the type emits any
///   (observing types). `None` for types with no result artifact.
/// * `allowed_edges` — the edge types a node of this kind may *originate*
///   (DESIGN §6.3, §6.5).
/// * `ports` — the typed input/output ports (DESIGN §9.2).
/// * `staleness_rule` — when results of this type go stale (DESIGN §7.3).
/// * `capabilities_required` — the capability vocabulary this type's runner
///   needs (DESIGN §9.2, §15.2); deny-by-default.
/// * `ui_contributions` — opaque, sandbox-rendered UI contribution data
///   (color/icon/fields/actions); kept as a JSON value so the panel is fully
///   data-driven (DESIGN §14.5).
/// * `revoked_provenance` — reserved revocation flag (DESIGN §9.2; P8).
///
/// The struct derives serde so a descriptor is serializable and shareable (a
/// marketplace artifact); it is tagged by [`DESCRIPTOR_SCHEMA_VERSION`] (C5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeTypeDescriptor {
    /// The stable type identifier (the node `kind`).
    pub id: String,
    /// The semver version of this type's payload/result/port contract.
    pub type_version: Version,
    /// The node family.
    pub family: Family,
    /// Whether nodes of this type own a content-addressed snapshot.
    pub owns_snapshot: bool,
    /// JSON Schema for the node payload.
    pub payload_schema: serde_json::Value,
    /// JSON Schema for attached results, if any.
    pub result_schema: Option<serde_json::Value>,
    /// The edge types a node of this kind may originate.
    pub allowed_edges: Vec<EdgeType>,
    /// The typed input/output ports.
    pub ports: Vec<Port>,
    /// When results of this type go stale.
    pub staleness_rule: StalenessRule,
    /// The capability vocabulary this type's runner needs.
    pub capabilities_required: Vec<String>,
    /// Opaque, data-driven UI contribution data.
    pub ui_contributions: serde_json::Value,
    /// Reserved provenance-revocation flag (P8); unused in F2.
    pub revoked_provenance: Option<RevocationFlag>,
}

impl NodeTypeDescriptor {
    /// Does this descriptor expose an [`PortDirection::Out`] port of kind
    /// [`PortKind::SnapshotRef`]?
    ///
    /// This is the "produces a `contentRef`" predicate from DESIGN §7.2. A
    /// descriptor that claims `owns_snapshot` is well-formed only if this holds;
    /// [`NodeTypeRegistry::register`] enforces the implication.
    #[must_use]
    pub fn has_snapshot_out_port(&self) -> bool {
        self.ports
            .iter()
            .any(|p| p.direction == PortDirection::Out && p.kind == PortKind::SnapshotRef)
    }
}

/// Errors raised when registering a [`NodeTypeDescriptor`].
///
/// `#[non_exhaustive]` so additional rejection reasons can be added additively
/// without breaking downstream matchers (constraint C2 — no domino).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// The descriptor claims `owns_snapshot` but exposes no out-port of kind
    /// [`PortKind::SnapshotRef`].
    ///
    /// This is the §7.2 rule: "a malformed custom type that claims `ownsSnapshot`
    /// without producing a `contentRef` is rejected at registration time."
    #[error(
        "node type {id} declares owns_snapshot but exposes no SnapshotRef out-port (the contentRef rule, DESIGN §7.2)"
    )]
    OwnsSnapshotMismatch {
        /// The `id` of the offending descriptor.
        id: String,
    },

    /// A descriptor with the exact same `(id, version)` is already registered.
    ///
    /// Descriptors are retained and versioned, never replaced (DESIGN §9.2); a
    /// new version is fine, but re-registering an exact existing `(id, version)`
    /// is rejected so resolution stays an unambiguous function.
    #[error("node type {id}@{version} is already registered (duplicate exact version)")]
    DuplicateVersion {
        /// The `id` of the offending descriptor.
        id: String,
        /// The version that was already registered.
        version: String,
    },
}

/// The shared, versioned registry of node-type descriptors.
///
/// One registry serves every node kind: built-ins and user-defined types alike
/// register through [`register`](NodeTypeRegistry::register) — the engine is
/// closed to modification but open to extension (DESIGN §6.5, §7.1, §14.5).
///
/// # Mixed-version resolution
///
/// Descriptors are **retained, never replaced** (DESIGN §9.2). Registering a new
/// version of an existing `id` adds it alongside the older versions, so a node
/// created under an older `type_version` still resolves and restores after a
/// newer version is registered. [`resolve`](NodeTypeRegistry::resolve) with a
/// `None` version returns the highest registered version; with `Some(v)` it
/// returns that exact version if present.
///
/// Internally the registry is a map from `id` to a [`BTreeMap`] keyed by
/// [`semver::Version`], so "highest version" is an O(log n) max lookup and
/// resolution order is deterministic.
#[derive(Debug, Clone, Default)]
pub struct NodeTypeRegistry {
    /// `id -> (version -> descriptor)`. The inner `BTreeMap` keeps versions
    /// ordered so the highest is the last key.
    by_id: BTreeMap<String, BTreeMap<Version, NodeTypeDescriptor>>,
}

impl NodeTypeRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
        }
    }

    /// Register a node-type descriptor, validating the F2 contract.
    ///
    /// # Validation
    ///
    /// * If `owns_snapshot` is `true` but the descriptor exposes no
    ///   [`PortDirection::Out`] port of kind [`PortKind::SnapshotRef`], returns
    ///   [`RegistryError::OwnsSnapshotMismatch`] (the contentRef rule, DESIGN
    ///   §7.2).
    /// * If a descriptor with the exact same `(id, type_version)` is already
    ///   registered, returns [`RegistryError::DuplicateVersion`]. A *different*
    ///   version of an existing `id` is accepted and retained alongside the
    ///   others (DESIGN §9.2).
    ///
    /// On success the descriptor is stored and resolvable.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] when either validation rule above fails; the
    /// registry is left unchanged in that case.
    pub fn register(&mut self, descriptor: NodeTypeDescriptor) -> Result<(), RegistryError> {
        if descriptor.owns_snapshot && !descriptor.has_snapshot_out_port() {
            return Err(RegistryError::OwnsSnapshotMismatch {
                id: descriptor.id.clone(),
            });
        }

        let versions = self.by_id.entry(descriptor.id.clone()).or_default();
        if versions.contains_key(&descriptor.type_version) {
            return Err(RegistryError::DuplicateVersion {
                id: descriptor.id.clone(),
                version: descriptor.type_version.to_string(),
            });
        }

        versions.insert(descriptor.type_version.clone(), descriptor);
        Ok(())
    }

    /// Resolve a descriptor by `id` and optional exact `version`.
    ///
    /// * `version == None` returns the **highest** registered version of `id`
    ///   (the current type contract), or `None` if `id` is unknown.
    /// * `version == Some(v)` returns the descriptor for that exact version, or
    ///   `None` if `id` or that specific version is not registered.
    ///
    /// Because descriptors are retained, an older exact version stays resolvable
    /// after a newer one is registered (DESIGN §9.2).
    #[must_use]
    pub fn resolve(&self, id: &str, version: Option<&Version>) -> Option<&NodeTypeDescriptor> {
        let versions = self.by_id.get(id)?;
        match version {
            Some(v) => versions.get(v),
            // BTreeMap iterates in ascending key order; the last is the highest.
            None => versions.values().next_back(),
        }
    }

    /// List every registered descriptor.
    ///
    /// All versions of all ids are returned, ordered by `id` then ascending
    /// `version` (the deterministic [`BTreeMap`] iteration order). This is what
    /// a data-driven legend / type browser folds over (DESIGN §14.5).
    #[must_use]
    pub fn list(&self) -> Vec<&NodeTypeDescriptor> {
        self.by_id
            .values()
            .flat_map(|versions| versions.values())
            .collect()
    }

    /// The number of distinct registered `(id, version)` descriptors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.values().map(BTreeMap::len).sum()
    }

    /// Whether the registry holds no descriptors.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.values().all(BTreeMap::is_empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal valid descriptor builder for tests.
    ///
    /// Defaults to a non-snapshot-owning Context type; callers flip
    /// `owns_snapshot` / add ports as needed.
    fn descriptor(id: &str, version: Version) -> NodeTypeDescriptor {
        NodeTypeDescriptor {
            id: id.to_string(),
            type_version: version,
            family: Family::Context,
            owns_snapshot: false,
            payload_schema: json!({ "type": "object" }),
            result_schema: None,
            allowed_edges: vec![EdgeType::ParentChild],
            ports: vec![],
            staleness_rule: StalenessRule::Never,
            capabilities_required: vec![],
            ui_contributions: json!({}),
            revoked_provenance: None,
        }
    }

    /// The canonical SnapshotRef out-port that satisfies the contentRef rule.
    fn snapshot_out_port() -> Port {
        Port {
            name: "snapshot".into(),
            direction: PortDirection::Out,
            kind: PortKind::SnapshotRef,
            schema: json!({ "type": "string" }),
        }
    }

    /// A well-formed mutating, snapshot-owning descriptor (the "snapshot" type).
    fn snapshot_descriptor(version: Version) -> NodeTypeDescriptor {
        NodeTypeDescriptor {
            family: Family::Mutating,
            owns_snapshot: true,
            staleness_rule: StalenessRule::WhenAncestorChanges,
            ports: vec![snapshot_out_port()],
            capabilities_required: vec!["snapshot.write".into()],
            ..descriptor("snapshot", version)
        }
    }

    // ---- frozen value sets --------------------------------------------------

    #[test]
    fn family_set_is_stable_and_complete() {
        assert_eq!(
            Family::ALL,
            [Family::Mutating, Family::Observing, Family::Context]
        );
        assert_eq!(Family::ALL.len(), 3);
        for f in Family::ALL {
            assert_eq!(f.to_string(), f.as_tag());
        }
    }

    #[test]
    fn family_serde_round_trips_via_snake_case_tags() {
        let pairs = [
            (Family::Mutating, "\"mutating\""),
            (Family::Observing, "\"observing\""),
            (Family::Context, "\"context\""),
        ];
        for (variant, json_tag) in pairs {
            assert_eq!(serde_json::to_string(&variant).unwrap(), json_tag);
            let back: Family = serde_json::from_str(json_tag).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn port_direction_and_kind_sets_are_stable() {
        assert_eq!(PortDirection::ALL, [PortDirection::In, PortDirection::Out]);
        assert_eq!(PortKind::ALL, [PortKind::Json, PortKind::SnapshotRef]);
    }

    #[test]
    fn staleness_rule_set_is_stable_and_complete() {
        assert_eq!(
            StalenessRule::ALL,
            [
                StalenessRule::Never,
                StalenessRule::WhenAncestorChanges,
                StalenessRule::WhenInputsChange,
            ]
        );
    }

    #[test]
    fn schema_version_present() {
        assert_eq!(DESCRIPTOR_SCHEMA_VERSION, 1);
    }

    // ---- the contentRef / owns_snapshot rule -------------------------------

    #[test]
    fn owns_snapshot_without_snapshot_ref_out_port_is_rejected() {
        let mut registry = NodeTypeRegistry::new();
        // Claims owns_snapshot but has no ports at all.
        let bad = NodeTypeDescriptor {
            family: Family::Mutating,
            owns_snapshot: true,
            ..descriptor("edit", Version::new(1, 0, 0))
        };
        let err = registry.register(bad).unwrap_err();
        assert_eq!(
            err,
            RegistryError::OwnsSnapshotMismatch { id: "edit".into() }
        );
        // Registry unchanged on rejection.
        assert!(registry.is_empty());
    }

    #[test]
    fn owns_snapshot_with_only_an_in_snapshot_ref_port_is_rejected() {
        let mut registry = NodeTypeRegistry::new();
        // A SnapshotRef port exists, but it is an *input*, not an output — does
        // not satisfy "produces a contentRef".
        let bad = NodeTypeDescriptor {
            family: Family::Mutating,
            owns_snapshot: true,
            ports: vec![Port {
                name: "parent_snapshot".into(),
                direction: PortDirection::In,
                kind: PortKind::SnapshotRef,
                schema: json!({ "type": "string" }),
            }],
            ..descriptor("edit", Version::new(1, 0, 0))
        };
        assert_eq!(
            registry.register(bad).unwrap_err(),
            RegistryError::OwnsSnapshotMismatch { id: "edit".into() }
        );
    }

    #[test]
    fn owns_snapshot_with_only_a_json_out_port_is_rejected() {
        let mut registry = NodeTypeRegistry::new();
        // An out-port exists but it is a plain Json port, not SnapshotRef.
        let bad = NodeTypeDescriptor {
            family: Family::Mutating,
            owns_snapshot: true,
            ports: vec![Port {
                name: "summary".into(),
                direction: PortDirection::Out,
                kind: PortKind::Json,
                schema: json!({ "type": "object" }),
            }],
            ..descriptor("edit", Version::new(1, 0, 0))
        };
        assert_eq!(
            registry.register(bad).unwrap_err(),
            RegistryError::OwnsSnapshotMismatch { id: "edit".into() }
        );
    }

    #[test]
    fn owns_snapshot_with_snapshot_ref_out_port_is_accepted() {
        let mut registry = NodeTypeRegistry::new();
        registry
            .register(snapshot_descriptor(Version::new(1, 0, 0)))
            .unwrap();
        assert_eq!(registry.len(), 1);
        let d = registry.resolve("snapshot", None).unwrap();
        assert!(d.owns_snapshot);
        assert!(d.has_snapshot_out_port());
    }

    #[test]
    fn non_owning_type_needs_no_snapshot_ref_port() {
        let mut registry = NodeTypeRegistry::new();
        // Observing type with no ports at all is fine — it owns no snapshot.
        let observing = NodeTypeDescriptor {
            family: Family::Observing,
            result_schema: Some(json!({ "type": "object" })),
            staleness_rule: StalenessRule::WhenAncestorChanges,
            ..descriptor("validation", Version::new(1, 0, 0))
        };
        registry.register(observing).unwrap();
        assert_eq!(registry.len(), 1);
    }

    // ---- duplicate exact version rejection ---------------------------------

    #[test]
    fn duplicate_exact_version_is_rejected() {
        let mut registry = NodeTypeRegistry::new();
        registry
            .register(snapshot_descriptor(Version::new(1, 0, 0)))
            .unwrap();
        let err = registry
            .register(snapshot_descriptor(Version::new(1, 0, 0)))
            .unwrap_err();
        assert_eq!(
            err,
            RegistryError::DuplicateVersion {
                id: "snapshot".into(),
                version: "1.0.0".into(),
            }
        );
        // Still exactly one descriptor; the second did not overwrite the first.
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn distinct_prerelease_and_release_versions_coexist() {
        let mut registry = NodeTypeRegistry::new();
        let pre = Version::parse("1.0.0-rc.1").unwrap();
        let rel = Version::new(1, 0, 0);
        registry.register(snapshot_descriptor(pre.clone())).unwrap();
        registry.register(snapshot_descriptor(rel.clone())).unwrap();
        assert_eq!(registry.len(), 2);
        // Release outranks the prerelease (semver ordering).
        assert_eq!(
            registry.resolve("snapshot", None).unwrap().type_version,
            rel
        );
        assert!(registry.resolve("snapshot", Some(&pre)).is_some());
    }

    // ---- mixed-version retention & resolution ------------------------------

    #[test]
    fn registering_two_versions_keeps_both_resolvable() {
        let mut registry = NodeTypeRegistry::new();
        let v1 = Version::new(1, 0, 0);
        let v2 = Version::new(2, 0, 0);
        registry.register(snapshot_descriptor(v1.clone())).unwrap();
        registry.register(snapshot_descriptor(v2.clone())).unwrap();

        // Both versions retained.
        assert_eq!(registry.len(), 2);
        // resolve(None) -> highest version.
        assert_eq!(registry.resolve("snapshot", None).unwrap().type_version, v2);
        // Each exact version still resolvable.
        assert_eq!(
            registry
                .resolve("snapshot", Some(&v1))
                .unwrap()
                .type_version,
            v1
        );
        assert_eq!(
            registry
                .resolve("snapshot", Some(&v2))
                .unwrap()
                .type_version,
            v2
        );
    }

    #[test]
    fn new_version_does_not_replace_old_regardless_of_registration_order() {
        // Register the *higher* version first, then the lower one.
        let mut registry = NodeTypeRegistry::new();
        let v1 = Version::new(1, 0, 0);
        let v2 = Version::new(2, 0, 0);
        registry.register(snapshot_descriptor(v2.clone())).unwrap();
        registry.register(snapshot_descriptor(v1.clone())).unwrap();

        assert_eq!(registry.len(), 2);
        // Highest is still v2 even though it was registered first.
        assert_eq!(registry.resolve("snapshot", None).unwrap().type_version, v2);
        assert!(registry.resolve("snapshot", Some(&v1)).is_some());
    }

    #[test]
    fn many_versions_resolve_to_the_highest() {
        let mut registry = NodeTypeRegistry::new();
        for (maj, min) in [(1, 0), (1, 4), (2, 0), (1, 9), (3, 1)] {
            registry
                .register(snapshot_descriptor(Version::new(maj, min, 0)))
                .unwrap();
        }
        assert_eq!(registry.len(), 5);
        assert_eq!(
            registry.resolve("snapshot", None).unwrap().type_version,
            Version::new(3, 1, 0)
        );
    }

    // ---- resolve / list edges ----------------------------------------------

    #[test]
    fn resolve_unknown_id_is_none() {
        let registry = NodeTypeRegistry::new();
        assert!(registry.resolve("nope", None).is_none());
        assert!(registry
            .resolve("nope", Some(&Version::new(1, 0, 0)))
            .is_none());
    }

    #[test]
    fn resolve_unknown_version_of_known_id_is_none() {
        let mut registry = NodeTypeRegistry::new();
        registry
            .register(snapshot_descriptor(Version::new(1, 0, 0)))
            .unwrap();
        assert!(registry
            .resolve("snapshot", Some(&Version::new(9, 9, 9)))
            .is_none());
    }

    #[test]
    fn list_is_ordered_by_id_then_version() {
        let mut registry = NodeTypeRegistry::new();
        // Insert out of order across two ids.
        registry
            .register(snapshot_descriptor(Version::new(2, 0, 0)))
            .unwrap();
        registry
            .register(snapshot_descriptor(Version::new(1, 0, 0)))
            .unwrap();
        let observing = NodeTypeDescriptor {
            family: Family::Observing,
            ..descriptor("validation", Version::new(1, 0, 0))
        };
        registry.register(observing).unwrap();

        let listed: Vec<(&str, String)> = registry
            .list()
            .iter()
            .map(|d| (d.id.as_str(), d.type_version.to_string()))
            .collect();
        // "snapshot" < "validation" by id; within snapshot, ascending version.
        assert_eq!(
            listed,
            vec![
                ("snapshot", "1.0.0".to_string()),
                ("snapshot", "2.0.0".to_string()),
                ("validation", "1.0.0".to_string()),
            ]
        );
    }

    #[test]
    fn empty_registry_reports_empty() {
        let registry = NodeTypeRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.list().is_empty());
        // Default == new().
        assert!(NodeTypeRegistry::default().is_empty());
    }

    // ---- descriptor serde / schema / reserved fields ------------------------

    #[test]
    fn descriptor_serde_round_trips() {
        let d = snapshot_descriptor(Version::new(1, 2, 3));
        let json_str = serde_json::to_string(&d).unwrap();
        let back: NodeTypeDescriptor = serde_json::from_str(&json_str).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn revocation_flag_is_constructible_and_serializes() {
        let mut d = snapshot_descriptor(Version::new(1, 0, 0));
        d.revoked_provenance = Some(RevocationFlag {
            reason: "key compromise".into(),
        });
        let json_str = serde_json::to_string(&d).unwrap();
        assert!(json_str.contains("key compromise"));
        let back: NodeTypeDescriptor = serde_json::from_str(&json_str).unwrap();
        assert_eq!(back.revoked_provenance.unwrap().reason, "key compromise");
    }

    #[test]
    fn has_snapshot_out_port_predicate() {
        let with = snapshot_descriptor(Version::new(1, 0, 0));
        assert!(with.has_snapshot_out_port());
        let without = descriptor("ctx", Version::new(1, 0, 0));
        assert!(!without.has_snapshot_out_port());
    }

    #[test]
    fn registry_error_messages_are_informative() {
        let m = RegistryError::OwnsSnapshotMismatch { id: "edit".into() }.to_string();
        assert!(m.contains("edit"));
        assert!(m.contains("SnapshotRef"));
        let d = RegistryError::DuplicateVersion {
            id: "snapshot".into(),
            version: "1.0.0".into(),
        }
        .to_string();
        assert!(d.contains("snapshot"));
        assert!(d.contains("1.0.0"));
    }
}
