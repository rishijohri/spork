//! The native [`AnthropicAdapter`] — the canonical &lt;-&gt; Anthropic Messages
//! mapping.
//!
//! This is the one real [`ProviderAdapter`](crate::ProviderAdapter)
//! implementation F4 ships (CLAUDE.md C3). It maps a
//! [`CanonicalTranscript`](crate::CanonicalTranscript) to and from the Anthropic
//! Messages JSON shape and contains **no HTTP** — the network transport that
//! would POST the [`to_wire`](AnthropicAdapter::to_wire) body and feed the
//! response to [`from_wire`](AnthropicAdapter::from_wire) is deferred to P6
//! (DESIGN.md §12.1, §12.2). The mapping is the genuinely hard part and is
//! covered by contract tests against handwritten Anthropic-shaped fixtures
//! (DESIGN.md §12.6).
//!
//! # The Anthropic shape, and how the canonical form maps onto it
//!
//! Anthropic's Messages API differs from the canonical form in exactly the ways
//! DESIGN.md §12.3 enumerates — system-prompt placement, role conventions,
//! tool-call id scheme, and content-block typing:
//!
//! - **System prompt placement**: the system instruction is a *top-level*
//!   `"system"` field, not a message. A canonical [`Role::System`] turn becomes
//!   that field; it is never emitted as a message.
//! - **Role conventions**: messages carry only `"user"` and `"assistant"`
//!   roles. A canonical [`Role::Tool`] turn is rendered as a `"user"` message
//!   whose content is `tool_result` blocks (that is where Anthropic expects
//!   tool output).
//! - **Content-block typing**: text is `{"type":"text","text":...}`; a tool
//!   call is `{"type":"tool_use","id":...,"name":...,"input":...}`; a tool
//!   result is `{"type":"tool_result","tool_use_id":...,"content":...,
//!   "is_error":...}`. The canonical [`ContentBlock`] variants map one-to-one.
//! - **Tool-call id scheme**: Anthropic correlates a `tool_result` to its
//!   `tool_use` by `tool_use_id`. The canonical
//!   [`ContentBlock::ToolResult::tool_call_id`] carries exactly that id, so the
//!   correlation survives the round trip — the lost-id failure mode of
//!   DESIGN.md §12.6 cannot occur here.
//! - **Opaque blocks**: an [`OpaqueProviderBlock`](crate::OpaqueProviderBlock)
//!   tagged `"anthropic"` is rendered back into the message content verbatim (it
//!   *is* a native Anthropic block such as a `thinking` block); a block tagged
//!   for another provider is not emitted on the wire.

use serde_json::{json, Map, Value};

use crate::{
    CanonicalTranscript, CanonicalTurn, CapSource, CapabilitySet, ContentBlock,
    OpaqueProviderBlock, ProviderAdapter, ProviderError, Role, ToolCalling, ANTHROPIC_PROVIDER_KEY,
};

/// The native Anthropic Messages adapter (mapping only; no HTTP).
///
/// Construct one with [`AnthropicAdapter::new`] for the default model, or
/// [`AnthropicAdapter::with_model`] to name a specific model key. The adapter is
/// cheap and stateless; clone it freely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicAdapter {
    model_key: String,
}

impl Default for AnthropicAdapter {
    fn default() -> Self {
        AnthropicAdapter::new()
    }
}

impl AnthropicAdapter {
    /// The model key used when none is specified.
    pub const DEFAULT_MODEL: &'static str = "claude-3-5-sonnet";

    /// Create an adapter for the default Anthropic model.
    #[must_use]
    pub fn new() -> Self {
        AnthropicAdapter::with_model(Self::DEFAULT_MODEL)
    }

    /// Create an adapter bound to a specific Anthropic `model_key`.
    #[must_use]
    pub fn with_model(model_key: impl Into<String>) -> Self {
        AnthropicAdapter {
            model_key: model_key.into(),
        }
    }

    /// The model key this adapter renders for.
    #[must_use]
    pub fn model_key(&self) -> &str {
        &self.model_key
    }

    /// Render one canonical content block into an Anthropic content-block JSON
    /// value.
    fn block_to_wire(block: &ContentBlock) -> Value {
        match block {
            ContentBlock::Text(text) => json!({ "type": "text", "text": text }),
            ContentBlock::ToolCall { id, name, input } => json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => json!({
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": content,
                "is_error": is_error,
            }),
        }
    }

    /// Render the content of one non-system turn into an Anthropic message
    /// object, appending any Anthropic-owned opaque blocks verbatim.
    fn turn_to_message(turn: &CanonicalTurn) -> Result<Value, ProviderError> {
        // Anthropic messages carry only user/assistant roles; a Tool turn is a
        // user message of tool_result blocks.
        let wire_role = match turn.role {
            Role::User | Role::Tool => "user",
            Role::Assistant => "assistant",
            Role::System => {
                return Err(ProviderError::UnmappableContent(
                    "system turn must be rendered as the top-level `system` field".to_string(),
                ));
            }
        };

        let mut content: Vec<Value> = turn.content.iter().map(Self::block_to_wire).collect();

        // Re-emit only Anthropic-native opaque blocks; foreign blocks never
        // reach the wire (they are dropped during projection, see ProviderProjection).
        for opaque in &turn.opaque {
            if opaque.provider == ANTHROPIC_PROVIDER_KEY {
                content.push(opaque.data.clone());
            }
        }

        Ok(json!({ "role": wire_role, "content": content }))
    }

    /// Parse one Anthropic content-block JSON value back into the canonical
    /// form, classifying anything non-portable as an
    /// [`OpaqueProviderBlock`].
    ///
    /// Returns `Ok(Ok(block))` for a portable block and `Ok(Err(opaque))` for a
    /// provider-specific block to attach to the turn's `opaque` list.
    #[allow(clippy::type_complexity)]
    fn block_from_wire(
        value: &Value,
    ) -> Result<Result<ContentBlock, OpaqueProviderBlock>, ProviderError> {
        let obj = value
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("content block is not an object".into()))?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::MalformedWire("content block has no `type`".into()))?;

        match kind {
            "text" => {
                let text = obj
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ProviderError::MalformedWire("text block has no string `text`".into())
                    })?
                    .to_string();
                Ok(Ok(ContentBlock::Text(text)))
            }
            "tool_use" => {
                let id = require_str(obj, "id", "tool_use")?;
                let name = require_str(obj, "name", "tool_use")?;
                let input = obj.get("input").cloned().unwrap_or(Value::Null);
                Ok(Ok(ContentBlock::ToolCall { id, name, input }))
            }
            "tool_result" => {
                let tool_call_id = require_str(obj, "tool_use_id", "tool_result")?;
                let content = obj.get("content").cloned().unwrap_or(Value::Null);
                let is_error = obj
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(Ok(ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                }))
            }
            // Any other block type (e.g. `thinking`) is a provider-specific
            // artifact: preserve it verbatim as an Anthropic-tagged opaque block.
            _ => Ok(Err(OpaqueProviderBlock {
                provider: ANTHROPIC_PROVIDER_KEY.to_string(),
                data: value.clone(),
            })),
        }
    }

    /// Parse the content array of one Anthropic message into canonical blocks
    /// and opaque blocks.
    fn content_from_wire(
        content: &Value,
    ) -> Result<(Vec<ContentBlock>, Vec<OpaqueProviderBlock>), ProviderError> {
        // Anthropic permits a bare string as shorthand for a single text block.
        if let Some(text) = content.as_str() {
            return Ok((vec![ContentBlock::Text(text.to_string())], Vec::new()));
        }
        let arr = content.as_array().ok_or_else(|| {
            ProviderError::MalformedWire(
                "message `content` is neither a string nor an array".into(),
            )
        })?;
        let mut blocks = Vec::new();
        let mut opaque = Vec::new();
        for item in arr {
            match Self::block_from_wire(item)? {
                Ok(block) => blocks.push(block),
                Err(op) => opaque.push(op),
            }
        }
        Ok((blocks, opaque))
    }

    /// Parse one Anthropic message object into a [`CanonicalTurn`].
    fn message_from_wire(message: &Value) -> Result<CanonicalTurn, ProviderError> {
        let obj = message
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("message is not an object".into()))?;
        let role_str = obj
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::MalformedWire("message has no `role`".into()))?;
        let content = obj
            .get("content")
            .ok_or_else(|| ProviderError::MalformedWire("message has no `content`".into()))?;
        let (blocks, opaque) = Self::content_from_wire(content)?;

        // A user message that is entirely tool_result blocks is a Tool turn in
        // the canonical form (that is how it was rendered out); otherwise the
        // wire role maps directly.
        let all_tool_results = !blocks.is_empty()
            && blocks
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }));
        let role = match role_str {
            "assistant" => Role::Assistant,
            "user" if all_tool_results => Role::Tool,
            "user" => Role::User,
            "system" => Role::System,
            other => {
                return Err(ProviderError::MalformedWire(format!(
                    "unknown message role `{other}`"
                )));
            }
        };

        // Surface a single tool_call_id at the turn level when the turn is one
        // tool result, mirroring how `tool_call_id` is read on the way out.
        let tool_call_id = if role == Role::Tool && blocks.len() == 1 {
            match &blocks[0] {
                ContentBlock::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            }
        } else {
            None
        };

        Ok(CanonicalTurn {
            role,
            content: blocks,
            tool_call_id,
            opaque,
        })
    }
}

/// Pull a required string field out of an object, or fail with a precise
/// malformed-wire message naming the block kind.
fn require_str(obj: &Map<String, Value>, key: &str, kind: &str) -> Result<String, ProviderError> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ProviderError::MalformedWire(format!("{kind} block has no string `{key}`")))
}

impl ProviderAdapter for AnthropicAdapter {
    fn describe(&self) -> CapabilitySet {
        // Static-table hint for a current frontier Claude model (DESIGN.md
        // §12.4: the table is a hint, refined by a P6 runtime probe).
        CapabilitySet {
            model_key: self.model_key.clone(),
            parallel_tool_calls: true,
            structured_output: true,
            vision: true,
            prompt_caching: true,
            tool_calling: ToolCalling::Native,
            source: CapSource::StaticTable,
        }
    }

    fn to_wire(&self, transcript: &CanonicalTranscript) -> Result<Value, ProviderError> {
        let mut system_parts: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for turn in &transcript.turns {
            if turn.role == Role::System {
                // Collect all system text into the top-level field. A system
                // turn carries only text in the portable form.
                for block in &turn.content {
                    match block {
                        ContentBlock::Text(t) => system_parts.push(t.clone()),
                        _ => {
                            return Err(ProviderError::UnmappableContent(
                                "system turn may contain only text".to_string(),
                            ));
                        }
                    }
                }
                continue;
            }
            messages.push(Self::turn_to_message(turn)?);
        }

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(self.model_key.clone()));
        if !system_parts.is_empty() {
            body.insert(
                "system".to_string(),
                Value::String(system_parts.join("\n\n")),
            );
        }
        body.insert("messages".to_string(), Value::Array(messages));
        Ok(Value::Object(body))
    }

    fn from_wire(&self, wire: Value) -> Result<CanonicalTranscript, ProviderError> {
        let obj = wire
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("wire payload is not an object".into()))?;

        let mut turns: Vec<CanonicalTurn> = Vec::new();

        // A top-level `system` string (request shape) becomes a leading System
        // turn so a request round-trips through the canonical form.
        if let Some(system) = obj.get("system") {
            let text = system.as_str().ok_or_else(|| {
                ProviderError::MalformedWire("`system` field must be a string".into())
            })?;
            turns.push(CanonicalTurn::text(Role::System, text));
        }

        if let Some(messages) = obj.get("messages") {
            // Request shape: a `messages` array.
            let arr = messages.as_array().ok_or_else(|| {
                ProviderError::MalformedWire("`messages` must be an array".into())
            })?;
            for message in arr {
                turns.push(Self::message_from_wire(message)?);
            }
        } else if obj.contains_key("role") && obj.contains_key("content") {
            // Response shape: a single message object (role + content), the way
            // the Anthropic Messages endpoint returns an assistant turn.
            turns.push(Self::message_from_wire(&wire)?);
        } else {
            return Err(ProviderError::MalformedWire(
                "wire payload has neither `messages` nor a `role`/`content` message".into(),
            ));
        }

        Ok(CanonicalTranscript::new(turns))
    }

    fn raw_passthrough(&self) -> bool {
        // The native Anthropic adapter exposes the raw-passthrough escape hatch
        // for features not yet modeled in the canonical form (DESIGN.md §12.2).
        true
    }
}
