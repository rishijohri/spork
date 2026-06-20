//! The **Codebase-Edit** built-in node type (DESIGN.md §6.2, §7.1).
//!
//! An Edit node is the headline *mutating* kind: it owns a content-addressed
//! snapshot (`owns_snapshot = true`) and binds **both** a codebase `contentRef`
//! (its snapshot) and a `conversation_ref`, so a restore swaps code and
//! conversation transactionally (DESIGN.md §6.4, §7.2). Its descriptor therefore
//! exposes a [`PortKind::SnapshotRef`] out-port — the registry's contentRef rule
//! (DESIGN.md §7.2) — exactly as a third-party mutating type would.
//!
//! # The versioned payload (CLAUDE.md C5)
//!
//! [`EditPayload`] is the schema-versioned ([`EDIT_PAYLOAD_VERSION`]) record an
//! Edit node carries:
//!
//! - `diff_summary` — a short human/agent description of the change.
//! - `files_changed` — the repository-relative paths the edit touched (this is
//!   the change scope the auto-run Sanity hook intersects against, DESIGN.md
//!   §8.2).
//! - `tool_calls` — the agentic tool calls that produced the edit (DESIGN.md
//!   §6.2 payload essentials).
//! - `context_sources` — the lineage/context sources fed to the model
//!   (DESIGN.md §6.2, §6.6 lineage-aware context).
//! - `conversation_ref` — the content hash of the bound conversation transcript,
//!   so restore is dual (code + conversation) (DESIGN.md §6.4, §7.2). Optional so
//!   a bare edit with no recorded conversation is still representable; when
//!   present it is what the dual-restore guard moves alongside the snapshot.
//!
//! The on-the-wire `payload_schema` the descriptor declares is the JSON Schema
//! the daemon validates an incoming payload against (DESIGN.md §6.2); the typed
//! [`EditPayload`] is the in-process builder that produces a value matching it.
//!
//! Design references: DESIGN.md §6.2 (envelope + payload), §6.4 (dual restore),
//! §7.1 (the built-in taxonomy), §7.2 (mutating/observing split, contentRef
//! rule).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spork_graph::EdgeType;
use spork_hash::Hash;
use spork_registry::{Family, NodeTypeDescriptor, Port, PortDirection, PortKind, StalenessRule};

use crate::descriptor::{type_version, SNAPSHOT_OUT_PORT_NAME};

/// The stable type id (the node `kind`) of the Codebase-Edit built-in.
pub const EDIT_KIND: &str = "codebase-edit";

/// The schema version stamped on a freshly built [`EditPayload`] (CLAUDE.md C5).
pub const EDIT_PAYLOAD_VERSION: u16 = 1;

/// One agentic tool call recorded on an Edit node.
///
/// The agent's edit is produced by a sequence of tool calls (read file, apply
/// patch, run command, …); recording them on the node is part of the §6.2
/// payload essentials and feeds handoff generation. `name` is the tool's stable
/// name; `summary` is a short human-readable description of what the call did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The tool's stable name (e.g. `"apply_patch"`, `"run_command"`).
    pub name: String,
    /// A short human-readable description of what this call did.
    pub summary: String,
}

impl ToolCall {
    /// Construct a tool call from a name and a summary.
    #[must_use]
    pub fn new(name: impl Into<String>, summary: impl Into<String>) -> Self {
        ToolCall {
            name: name.into(),
            summary: summary.into(),
        }
    }
}

/// One lineage/context source fed to the model that produced an Edit.
///
/// Spork is lineage-aware (DESIGN.md §6.6): an edit's context is assembled from
/// prior nodes, files, and handoff documents. `kind` labels the source class
/// (e.g. `"node"`, `"file"`, `"handoff"`) and `reference` is its address (a node
/// id, a path, a content hash) — kept as a string so any source class is
/// representable without a contract change (CLAUDE.md C3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSource {
    /// The class of context source (e.g. `"node"`, `"file"`, `"handoff"`).
    pub kind: String,
    /// The source's address (node id, path, or content hash).
    pub reference: String,
}

impl ContextSource {
    /// Construct a context source from a kind and a reference.
    #[must_use]
    pub fn new(kind: impl Into<String>, reference: impl Into<String>) -> Self {
        ContextSource {
            kind: kind.into(),
            reference: reference.into(),
        }
    }
}

/// The schema-versioned payload of a Codebase-Edit node.
///
/// See the [module docs](crate::edit) for the field semantics. The struct derives
/// serde so it round-trips to/from the JSON `payload` the daemon stores, and it
/// stamps its own [`schema_version`](EditPayload::schema_version) (CLAUDE.md C5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPayload {
    /// The schema version of this payload ([`EDIT_PAYLOAD_VERSION`] for fresh
    /// values).
    pub schema_version: u16,
    /// A short description of the change.
    pub diff_summary: String,
    /// Repository-relative paths the edit changed (the change scope).
    pub files_changed: Vec<String>,
    /// The agentic tool calls that produced the edit.
    pub tool_calls: Vec<ToolCall>,
    /// The lineage/context sources fed to the model.
    pub context_sources: Vec<ContextSource>,
    /// The content hash of the bound conversation transcript, if any (the second
    /// half of the dual restore, DESIGN.md §6.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_ref: Option<Hash>,
}

impl EditPayload {
    /// Construct an Edit payload with the given diff summary and changed files,
    /// stamping the current [`EDIT_PAYLOAD_VERSION`].
    #[must_use]
    pub fn new(diff_summary: impl Into<String>, files_changed: Vec<String>) -> Self {
        EditPayload {
            schema_version: EDIT_PAYLOAD_VERSION,
            diff_summary: diff_summary.into(),
            files_changed,
            tool_calls: Vec::new(),
            context_sources: Vec::new(),
            conversation_ref: None,
        }
    }

    /// Append a tool call (builder style).
    #[must_use]
    pub fn with_tool_call(mut self, call: ToolCall) -> Self {
        self.tool_calls.push(call);
        self
    }

    /// Append a context source (builder style).
    #[must_use]
    pub fn with_context_source(mut self, source: ContextSource) -> Self {
        self.context_sources.push(source);
        self
    }

    /// Bind the conversation transcript content hash (builder style).
    ///
    /// This is the `conversation_ref` the dual-restore guard moves alongside the
    /// snapshot, so code and conversation always restore together (DESIGN.md
    /// §6.4).
    #[must_use]
    pub fn with_conversation_ref(mut self, conversation_ref: Hash) -> Self {
        self.conversation_ref = Some(conversation_ref);
        self
    }

    /// Render this payload to the JSON `payload` value the daemon stores.
    ///
    /// # Errors
    /// [`NodesError::Serialize`](crate::NodesError::Serialize) if the value cannot
    /// be encoded (not possible for this float-free struct in practice).
    pub fn to_value(&self) -> crate::Result<Value> {
        Ok(serde_json::to_value(self)?)
    }
}

/// The JSON Schema the Edit descriptor declares for its payload (DESIGN.md §6.2).
///
/// The daemon validates an incoming `node.create` payload against this schema; a
/// [`EditPayload`] always satisfies it by construction.
fn payload_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "schema_version": { "type": "integer", "minimum": 1 },
            "diff_summary": { "type": "string" },
            "files_changed": { "type": "array", "items": { "type": "string" } },
            "tool_calls": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "summary": { "type": "string" }
                    },
                    "required": ["name", "summary"]
                }
            },
            "context_sources": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string" },
                        "reference": { "type": "string" }
                    },
                    "required": ["kind", "reference"]
                }
            },
            "conversation_ref": { "type": "string", "description": "content hash of the bound transcript" }
        },
        "required": ["schema_version", "diff_summary", "files_changed", "tool_calls", "context_sources"]
    })
}

/// Build the Codebase-Edit [`NodeTypeDescriptor`] (DESIGN.md §7.1).
///
/// A [`Family::Mutating`] type that `owns_snapshot` and exposes a
/// [`PortKind::SnapshotRef`] out-port (so it satisfies the registry's contentRef
/// rule, DESIGN.md §7.2). It may originate the structural mutating-graph edges
/// plus [`EdgeType::Validates`] / [`EdgeType::Checks`] / [`EdgeType::Stresses`],
/// since observing nodes attach *to* an Edit (DESIGN.md §6.3).
#[must_use]
pub fn descriptor() -> NodeTypeDescriptor {
    NodeTypeDescriptor {
        id: EDIT_KIND.to_string(),
        type_version: type_version(),
        family: Family::Mutating,
        owns_snapshot: true,
        payload_schema: payload_schema(),
        result_schema: None,
        allowed_edges: vec![
            EdgeType::ParentChild,
            EdgeType::Branch,
            EdgeType::DerivedFrom,
            EdgeType::MergeParent,
        ],
        ports: vec![
            Port {
                name: SNAPSHOT_OUT_PORT_NAME.to_string(),
                direction: PortDirection::Out,
                kind: PortKind::SnapshotRef,
                schema: json!({ "type": "string", "description": "content-addressed tree hash" }),
            },
            Port {
                name: "conversation".to_string(),
                direction: PortDirection::Out,
                kind: PortKind::Json,
                schema: json!({ "type": "string", "description": "content hash of the conversation transcript" }),
            },
        ],
        staleness_rule: StalenessRule::WhenAncestorChanges,
        capabilities_required: vec!["snapshot.write".to_string()],
        ui_contributions: json!({ "color": "#22c55e", "icon": "pencil", "displayName": "Edit" }),
        revoked_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spork_hash::hash_bytes;
    use spork_registry::NodeTypeRegistry;

    #[test]
    fn descriptor_is_mutating_and_owns_snapshot() {
        let d = descriptor();
        assert_eq!(d.id, EDIT_KIND);
        assert_eq!(d.family, Family::Mutating);
        assert!(d.owns_snapshot);
        // The contentRef rule: a SnapshotRef out-port is present (DESIGN §7.2).
        assert!(d.has_snapshot_out_port());
    }

    #[test]
    fn descriptor_registers_through_the_public_registry() {
        let mut reg = NodeTypeRegistry::new();
        reg.register(descriptor()).unwrap();
        assert!(reg.resolve(EDIT_KIND, None).is_some());
    }

    #[test]
    fn payload_is_versioned_and_round_trips() {
        let payload = EditPayload::new("add feature", vec!["src/lib.rs".into()])
            .with_tool_call(ToolCall::new("apply_patch", "edited src/lib.rs"))
            .with_context_source(ContextSource::new("node", "01H..."))
            .with_conversation_ref(hash_bytes(b"transcript"));
        assert_eq!(payload.schema_version, EDIT_PAYLOAD_VERSION);

        let value = payload.to_value().unwrap();
        let back: EditPayload = serde_json::from_value(value).unwrap();
        assert_eq!(payload, back);
        assert_eq!(back.conversation_ref, Some(hash_bytes(b"transcript")));
    }

    #[test]
    fn payload_binds_both_content_and_conversation_refs() {
        // The §7.2 dual binding: an Edit binds a snapshot (via owns_snapshot on
        // the descriptor) and a conversation_ref (in the payload).
        let payload = EditPayload::new("x", vec![]).with_conversation_ref(hash_bytes(b"conv"));
        assert!(payload.conversation_ref.is_some());
        assert!(descriptor().owns_snapshot);
    }

    #[test]
    fn payload_without_conversation_omits_the_field() {
        let payload = EditPayload::new("x", vec![]);
        let value = payload.to_value().unwrap();
        assert!(value.get("conversation_ref").is_none());
    }

    #[test]
    fn files_changed_is_the_change_scope() {
        let payload = EditPayload::new("x", vec!["a.rs".into(), "b.rs".into()]);
        assert_eq!(payload.files_changed, vec!["a.rs", "b.rs"]);
    }
}
