//! The **Snapshot** built-in node type — drift reconcile / manual / import
//! (DESIGN.md §6.2, §7.1, A.7 C-2).
//!
//! A Snapshot node is a *mutating* kind (`owns_snapshot = true`): it captures a
//! whole codebase state so clicking it restores that exact state, and it keeps
//! the DAG a true, gapless record (DESIGN.md §7.1, §7.4). It carries an
//! [`SnapshotOrigin`] discriminator:
//!
//! - [`SnapshotOrigin::AutoDrift`] — the Jujutsu-style auto-snapshot the
//!   `DriftDetector` materializes when it finds an unattributed working-tree
//!   change, so *all* codebase changes map to a node (DESIGN.md §7.4).
//! - [`SnapshotOrigin::Manual`] — a user-requested snapshot.
//! - [`SnapshotOrigin::Import`] — **external state ingested as a snapshot**.
//!   Import is *not* a separate node kind: per DESIGN.md A.7 C-2, "an import is
//!   not a separate kind but a Snapshot whose origin is an external source,"
//!   consistent with §6.2's `origin(auto_drift|manual|import)`. The `import_source`
//!   records where the external state came from.
//!
//! # The versioned payload (CLAUDE.md C5)
//!
//! [`SnapshotPayload`] is the schema-versioned ([`SNAPSHOT_PAYLOAD_VERSION`])
//! record, matching the §7.1 payload core: `origin`, an optional `drift_source`
//! (for auto-drift), and an optional `import_source{type, uri, git_ref}` (for
//! imports). The codebase `contentRef` is bound via the descriptor's
//! `owns_snapshot` SnapshotRef out-port and carried on the node envelope, not in
//! the payload.
//!
//! Design references: DESIGN.md §6.2 (origin field), §7.1 (taxonomy), §7.4
//! (drift reconcile), A.7 C-2 (import folded into Snapshot).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spork_graph::EdgeType;
use spork_registry::{Family, NodeTypeDescriptor, Port, PortDirection, PortKind, StalenessRule};

use crate::descriptor::{type_version, SNAPSHOT_OUT_PORT_NAME};

/// The stable type id (the node `kind`) of the Snapshot built-in.
pub const SNAPSHOT_KIND: &str = "snapshot";

/// The schema version stamped on a freshly built [`SnapshotPayload`].
pub const SNAPSHOT_PAYLOAD_VERSION: u16 = 1;

/// Where a Snapshot node's captured state came from (DESIGN.md §6.2,
/// `origin(auto_drift|manual|import)`).
///
/// Serializes `snake_case` so the stored payload reads `"auto_drift"` /
/// `"manual"` / `"import"`, exactly the §6.2 enumeration. Import is one *origin*
/// here, not a separate node kind (DESIGN.md A.7 C-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotOrigin {
    /// Materialized by the drift detector for an unattributed working-tree change.
    AutoDrift,
    /// A user-requested snapshot.
    Manual,
    /// External state ingested as a snapshot (an import — DESIGN.md A.7 C-2).
    Import,
}

impl SnapshotOrigin {
    /// Every origin, in declaration order.
    pub const ALL: [SnapshotOrigin; 3] = [
        SnapshotOrigin::AutoDrift,
        SnapshotOrigin::Manual,
        SnapshotOrigin::Import,
    ];

    /// The stable `snake_case` tag for this origin (matches the serde form).
    #[must_use]
    pub const fn as_tag(self) -> &'static str {
        match self {
            SnapshotOrigin::AutoDrift => "auto_drift",
            SnapshotOrigin::Manual => "manual",
            SnapshotOrigin::Import => "import",
        }
    }
}

/// Where external state came from, for a [`SnapshotOrigin::Import`] snapshot
/// (DESIGN.md §7.1 `importSource{type, uri, gitRef}?`).
///
/// `source_type` labels the import class (e.g. `"git"`, `"tarball"`,
/// `"directory"`); `uri` is the external location; `git_ref` is the optional Git
/// ref/commit when importing from Git (the lone imported, never Spork-computed,
/// id — DESIGN.md A.7 C-3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSource {
    /// The import class (e.g. `"git"`, `"tarball"`, `"directory"`).
    #[serde(rename = "type")]
    pub source_type: String,
    /// The external location the state was ingested from.
    pub uri: String,
    /// The Git ref/commit imported from, when `source_type == "git"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

impl ImportSource {
    /// Construct an import source from a type and a uri.
    #[must_use]
    pub fn new(source_type: impl Into<String>, uri: impl Into<String>) -> Self {
        ImportSource {
            source_type: source_type.into(),
            uri: uri.into(),
            git_ref: None,
        }
    }

    /// Set the Git ref/commit (builder style).
    #[must_use]
    pub fn with_git_ref(mut self, git_ref: impl Into<String>) -> Self {
        self.git_ref = Some(git_ref.into());
        self
    }
}

/// The schema-versioned payload of a Snapshot node (DESIGN.md §7.1).
///
/// See the [module docs](crate::snapshot). `drift_source` is present for
/// auto-drift snapshots (what the detector observed); `import_source` is present
/// for imports. A `manual` snapshot has neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPayload {
    /// The schema version of this payload ([`SNAPSHOT_PAYLOAD_VERSION`]).
    pub schema_version: u16,
    /// Where this snapshot's state came from.
    pub origin: SnapshotOrigin,
    /// What the drift detector observed, for an auto-drift snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_source: Option<String>,
    /// Where external state was ingested from, for an import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_source: Option<ImportSource>,
}

impl SnapshotPayload {
    /// Construct a manual snapshot payload.
    #[must_use]
    pub fn manual() -> Self {
        SnapshotPayload {
            schema_version: SNAPSHOT_PAYLOAD_VERSION,
            origin: SnapshotOrigin::Manual,
            drift_source: None,
            import_source: None,
        }
    }

    /// Construct an auto-drift snapshot payload recording what the detector saw.
    #[must_use]
    pub fn auto_drift(drift_source: impl Into<String>) -> Self {
        SnapshotPayload {
            schema_version: SNAPSHOT_PAYLOAD_VERSION,
            origin: SnapshotOrigin::AutoDrift,
            drift_source: Some(drift_source.into()),
            import_source: None,
        }
    }

    /// Construct an import snapshot payload (origin = import) ingesting
    /// `import_source` (DESIGN.md A.7 C-2: import is a Snapshot, not a new kind).
    #[must_use]
    pub fn import(import_source: ImportSource) -> Self {
        SnapshotPayload {
            schema_version: SNAPSHOT_PAYLOAD_VERSION,
            origin: SnapshotOrigin::Import,
            drift_source: None,
            import_source: Some(import_source),
        }
    }

    /// Render this payload to the JSON `payload` value the daemon stores.
    ///
    /// # Errors
    /// [`NodesError::Serialize`](crate::NodesError::Serialize) on an encoding
    /// failure.
    pub fn to_value(&self) -> crate::Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// Build the Snapshot [`NodeTypeDescriptor`] (DESIGN.md §7.1).
///
/// A [`Family::Mutating`] type that `owns_snapshot` and exposes a
/// [`PortKind::SnapshotRef`] out-port (the contentRef rule, DESIGN.md §7.2). The
/// payload schema admits all three origins (`auto_drift | manual | import`), so
/// the *same* kind carries an import (DESIGN.md A.7 C-2).
#[must_use]
pub fn descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: SNAPSHOT_KIND.to_string(),
        type_version: type_version(),
        family: Family::Mutating,
        owns_snapshot: true,
        payload_schema: json!({
            "type": "object",
            "properties": {
                "schema_version": { "type": "integer", "minimum": 1 },
                "origin": { "enum": ["auto_drift", "manual", "import"] },
                "drift_source": { "type": "string" },
                "import_source": {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string" },
                        "uri": { "type": "string" },
                        "git_ref": { "type": "string" }
                    },
                    "required": ["type", "uri"]
                }
            },
            "required": ["schema_version", "origin"]
        }),
        result_schema: None,
        allowed_edges: vec![
            EdgeType::ParentChild,
            EdgeType::Branch,
            EdgeType::DerivedFrom,
            EdgeType::MergeParent,
        ],
        ports: vec![Port {
            name: SNAPSHOT_OUT_PORT_NAME.to_string(),
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
    use spork_registry::NodeTypeRegistry;

    #[test]
    fn descriptor_is_mutating_and_owns_snapshot() {
        let d = descriptor();
        assert_eq!(d.id, SNAPSHOT_KIND);
        assert_eq!(d.family, Family::Mutating);
        assert!(d.owns_snapshot);
        assert!(d.has_snapshot_out_port());
        let mut reg = NodeTypeRegistry::new();
        reg.register(d).unwrap();
    }

    #[test]
    fn origin_set_is_stable_and_tags_match_serde() {
        assert_eq!(
            SnapshotOrigin::ALL,
            [
                SnapshotOrigin::AutoDrift,
                SnapshotOrigin::Manual,
                SnapshotOrigin::Import,
            ]
        );
        for o in SnapshotOrigin::ALL {
            let json = serde_json::to_string(&o).unwrap();
            assert_eq!(json, format!("\"{}\"", o.as_tag()));
        }
    }

    #[test]
    fn import_is_an_origin_not_a_separate_kind() {
        // DESIGN A.7 C-2: import ingests external state as origin=import on the
        // SAME snapshot kind, not a distinct node type.
        let p =
            SnapshotPayload::import(ImportSource::new("git", "https://x/y.git").with_git_ref("v1"));
        assert_eq!(p.origin, SnapshotOrigin::Import);
        assert_eq!(descriptor().id, SNAPSHOT_KIND); // the kind is still "snapshot"
        let v = p.to_value().unwrap();
        assert_eq!(v["origin"], "import");
        assert_eq!(v["import_source"]["type"], "git");
        assert_eq!(v["import_source"]["git_ref"], "v1");
    }

    #[test]
    fn manual_and_auto_drift_payloads_round_trip() {
        for p in [
            SnapshotPayload::manual(),
            SnapshotPayload::auto_drift("bash rm src/x.rs"),
        ] {
            assert_eq!(p.schema_version, SNAPSHOT_PAYLOAD_VERSION);
            let v = p.to_value().unwrap();
            let back: SnapshotPayload = serde_json::from_value(v).unwrap();
            assert_eq!(p, back);
        }
    }

    #[test]
    fn manual_payload_omits_optional_sources() {
        let v = SnapshotPayload::manual().to_value().unwrap();
        assert!(v.get("drift_source").is_none());
        assert!(v.get("import_source").is_none());
        assert_eq!(v["origin"], "manual");
    }
}
