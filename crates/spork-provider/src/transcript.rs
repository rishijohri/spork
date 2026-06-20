//! The provider-neutral [`CanonicalTranscript`] and its parts.
//!
//! Prior turns are stored *provider-agnostically* and re-rendered into a target
//! provider's wire format on each request, so a node can hot-swap models within
//! a session without rewriting its history (DESIGN.md §12.3). This module is the
//! frozen, schema-versioned shape of that stored history: a sequence of
//! [`CanonicalTurn`]s, each a role plus portable [`ContentBlock`]s plus an
//! [`OpaqueProviderBlock`] escape hatch for provider-specific artifacts that do
//! not map across vendors.
//!
//! The transcript is content-addressed: it `Serialize`s through the canonical
//! encoder so an identical conversation hashes identically and can bind to a
//! snapshot and restore alongside its code (DESIGN.md §6.1, §12.3).

use serde::{Deserialize, Serialize};

/// The frozen schema version of [`CanonicalTranscript`] (CLAUDE.md C5).
///
/// Persisted and hashed alongside the transcript, so the shape can evolve
/// additively in a later generation without silently reinterpreting an existing
/// stored conversation (the no-domino seam, DESIGN.md A.7).
pub const CANONICAL_TRANSCRIPT_SCHEMA_VERSION: u16 = 1;

/// A conversation role, normalized across providers.
///
/// Providers disagree on role conventions (some carry the system prompt as a
/// top-level field, some as a first message; tool results are sometimes a
/// distinct role and sometimes a user message). Spork stores one neutral role
/// and each adapter maps it to the provider's convention in
/// [`to_wire`](crate::ProviderAdapter::to_wire) (DESIGN.md §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The system / developer instruction context.
    System,
    /// A message authored by the human (or the orchestrating agent acting as
    /// the user).
    User,
    /// A message authored by the model.
    Assistant,
    /// A tool's result, fed back to the model.
    Tool,
}

/// One portable unit of conversation content.
///
/// These are the content kinds every supported provider can represent, so they
/// survive a cross-provider projection intact. Anything a single provider
/// understands but others cannot (extended-thinking blocks, reasoning tokens)
/// is carried as an [`OpaqueProviderBlock`] instead and dropped on a
/// cross-provider projection (DESIGN.md §12.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text(String),

    /// A model request to invoke a tool.
    ///
    /// Carries the provider-neutral call `id` (each adapter maps it to that
    /// provider's id scheme), the tool `name`, and the JSON `input`. Losing a
    /// `ToolCall` id is the canonical normalization-drift corruption
    /// (DESIGN.md §12.6), so the id is a first-class field, never inferred.
    ToolCall {
        /// The call id this result will be correlated against.
        id: String,
        /// The tool's name.
        name: String,
        /// The tool's JSON-shaped input arguments.
        input: serde_json::Value,
    },

    /// The result of a previously requested tool call.
    ToolResult {
        /// The id of the [`ContentBlock::ToolCall`] this answers.
        tool_call_id: String,
        /// The result payload (text or structured JSON).
        content: serde_json::Value,
        /// Whether the tool reported an error.
        is_error: bool,
    },
}

/// A provider-specific artifact that does not map across vendors.
///
/// Claude extended-thinking blocks and OpenAI reasoning tokens are stored here,
/// tagged with the `provider` that produced them. On a *same-provider*
/// projection they survive; on a *cross-provider* projection they are dropped
/// and a `lossyProjection` warning is recorded on the turn, because a
/// chain-of-thought the next turn depended on cannot be faithfully carried
/// across a model swap (DESIGN.md §12.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpaqueProviderBlock {
    /// The provider key that owns this block (e.g. `"anthropic"`). A projection
    /// to any *other* provider drops it.
    pub provider: String,
    /// The opaque provider-native payload, preserved verbatim for a
    /// same-provider re-render.
    pub data: serde_json::Value,
}

/// One turn of the canonical conversation.
///
/// A turn is a role, its portable [`ContentBlock`]s, an optional correlating
/// `tool_call_id` (set when the whole turn *is* a single tool result), and any
/// provider-`opaque` blocks attached to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalTurn {
    /// Who authored this turn.
    pub role: Role,
    /// The portable content of the turn, in order.
    pub content: Vec<ContentBlock>,
    /// When this entire turn is a tool result, the id of the call it answers.
    ///
    /// `None` for ordinary text turns. Tool results expressed as
    /// [`ContentBlock::ToolResult`] inside `content` carry their own id; this
    /// field is the turn-level shorthand some providers use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Provider-specific artifacts attached to this turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opaque: Vec<OpaqueProviderBlock>,
}

impl CanonicalTurn {
    /// Construct a portable text turn with no tool correlation and no opaque
    /// blocks — the common case.
    ///
    /// # Example
    /// ```
    /// use spork_provider::{CanonicalTurn, ContentBlock, Role};
    /// let t = CanonicalTurn::text(Role::User, "hello");
    /// assert_eq!(t.role, Role::User);
    /// assert_eq!(t.content, vec![ContentBlock::Text("hello".into())]);
    /// ```
    #[must_use]
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        CanonicalTurn {
            role,
            content: vec![ContentBlock::Text(text.into())],
            tool_call_id: None,
            opaque: Vec::new(),
        }
    }
}

/// A provider-neutral conversation: the unit Spork stores, hashes, and projects.
///
/// The transcript is the single source of truth for a node's conversation. It is
/// schema-versioned (CLAUDE.md C5) and content-addressable: serializing it
/// through the canonical encoder yields stable identity bytes (DESIGN.md §6.1),
/// which is how a conversation binds to a code snapshot and restores with it
/// (DESIGN.md §12.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalTranscript {
    /// The schema version of this transcript shape.
    pub schema_version: u16,
    /// The turns of the conversation, oldest first.
    pub turns: Vec<CanonicalTurn>,
}

impl CanonicalTranscript {
    /// Construct a transcript stamped with the current schema version.
    #[must_use]
    pub fn new(turns: Vec<CanonicalTurn>) -> Self {
        CanonicalTranscript {
            schema_version: CANONICAL_TRANSCRIPT_SCHEMA_VERSION,
            turns,
        }
    }

    /// The content-address hash of this transcript over its canonical bytes.
    ///
    /// This is the identity a node binds to its snapshot: two byte-identical
    /// conversations hash identically and dedup, and the hash is what
    /// `spork-restore` keys a conversation+code restore on (DESIGN.md §6.1,
    /// §12.3).
    ///
    /// # Errors
    /// Returns [`ProviderError::Canon`](crate::ProviderError::Canon) only if
    /// the transcript somehow contains a value the canonical encoder rejects
    /// (a float); the typed schema here never introduces one.
    pub fn content_hash(&self) -> Result<spork_hash::Hash, crate::ProviderError> {
        let bytes = spork_canon::canonicalize(self)?;
        Ok(spork_hash::hash_bytes(&bytes))
    }
}

impl Default for CanonicalTranscript {
    fn default() -> Self {
        CanonicalTranscript::new(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_block_serde_shape_is_canon_safe() {
        // External tagging renders Text as {"text": "..."}, which canonicalizes
        // (no float, no untagged primitive newtype).
        let v = serde_json::to_value(ContentBlock::Text("hi".into())).unwrap();
        assert_eq!(v, json!({ "text": "hi" }));
        assert!(spork_canon::canonicalize(&v).is_ok());
    }

    #[test]
    fn tool_blocks_serde_round_trip() {
        let call = ContentBlock::ToolCall {
            id: "id1".into(),
            name: "n".into(),
            input: json!({"a": 1}),
        };
        let result = ContentBlock::ToolResult {
            tool_call_id: "id1".into(),
            content: json!("ok"),
            is_error: false,
        };
        for block in [call, result] {
            let s = serde_json::to_string(&block).unwrap();
            let back: ContentBlock = serde_json::from_str(&s).unwrap();
            assert_eq!(block, back);
        }
    }

    #[test]
    fn empty_turn_fields_are_omitted_from_json() {
        // A plain text turn omits tool_call_id and opaque (skip_serializing_if).
        let turn = CanonicalTurn::text(Role::User, "hi");
        let v = serde_json::to_value(&turn).unwrap();
        assert!(v.get("tool_call_id").is_none());
        assert!(v.get("opaque").is_none());
    }

    #[test]
    fn default_transcript_is_versioned_and_empty() {
        let t = CanonicalTranscript::default();
        assert_eq!(t.schema_version, CANONICAL_TRANSCRIPT_SCHEMA_VERSION);
        assert!(t.turns.is_empty());
    }
}
