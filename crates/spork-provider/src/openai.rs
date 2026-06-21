//! The [`OpenAiAdapter`] — the canonical <-> OpenAI Chat Completions mapping.
//!
//! P6 adds the OpenAI-compatible [`ProviderAdapter`](crate::ProviderAdapter)
//! implementation behind the F4-frozen port (CLAUDE.md C3). Like the native
//! [`AnthropicAdapter`](crate::AnthropicAdapter) it is **mapping only** — there
//! is no HTTP here; the network transport that POSTs the
//! [`to_wire`](OpenAiAdapter::to_wire) body and feeds the response to
//! [`from_wire`](OpenAiAdapter::from_wire) is a separate transport seam. The
//! mapping is the genuinely hard, testable part and is covered by contract tests
//! against handwritten OpenAI-shaped fixtures (DESIGN.md §12.1, §12.6).
//!
//! This same wire shape serves every "OpenAI-compatible" endpoint — OpenAI,
//! OpenRouter, vLLM, and most local servers — so one adapter covers the family;
//! *where* a given endpoint runs (its [`Locality`](crate::Locality)) is a router
//! concern, not the adapter's.
//!
//! # The OpenAI shape, and how the canonical form maps onto it
//!
//! OpenAI's Chat Completions API differs from the canonical form in exactly the
//! ways DESIGN.md §12.3 enumerates — and differently from Anthropic:
//!
//! - **System prompt placement**: OpenAI carries the system instruction as a
//!   `{"role":"system"}` *message* in the `messages` array (not a top-level
//!   field as Anthropic does). A canonical [`Role::System`] turn becomes a system
//!   message in place.
//! - **Role conventions**: `system` / `user` / `assistant` / `tool` are all
//!   first-class message roles. A canonical [`Role::Tool`] turn becomes one or
//!   more `{"role":"tool", "tool_call_id":...}` messages.
//! - **Tool calls live OUTSIDE content**: an assistant tool call is not a content
//!   block — it is an entry in a separate `tool_calls` array on the assistant
//!   message, and its `function.arguments` is a **JSON-encoded string**, not a
//!   JSON object. This stringify/parse is the OpenAI-specific quirk this adapter
//!   gets right so a tool-call's arguments survive the round trip (DESIGN.md
//!   §12.6).
//! - **Tool-call id scheme**: OpenAI correlates a tool result to its call via
//!   `tool_call_id`. The canonical [`ContentBlock::ToolResult::tool_call_id`]
//!   carries exactly that id.
//! - **Tool result content**: a `{"role":"tool"}` message's `content` is a
//!   string. The canonical [`ContentBlock::ToolResult::content`] is a JSON value,
//!   so it is JSON-encoded on the way out and parsed back on the way in, giving
//!   an exact round trip for both text and structured results.
//! - **Response shape**: a completion comes back as
//!   `{"choices":[{"message":{...}}]}`; [`from_wire`](OpenAiAdapter::from_wire)
//!   reads `choices[0].message` as the assistant turn.
//! - **Opaque blocks**: a non-portable artifact (OpenAI `reasoning`/
//!   `reasoning_content`) is captured as an
//!   [`OpaqueProviderBlock`](crate::OpaqueProviderBlock) tagged
//!   [`OPENAI_PROVIDER_KEY`](crate::OPENAI_PROVIDER_KEY); foreign opaque blocks
//!   are never emitted on the wire.

use serde_json::{json, Map, Value};

use crate::{
    CanonicalTranscript, CanonicalTurn, CapSource, CapabilitySet, ContentBlock,
    OpaqueProviderBlock, ProviderAdapter, ProviderError, Role, ToolCalling, OPENAI_PROVIDER_KEY,
};

/// The OpenAI Chat Completions adapter (mapping only; no HTTP).
///
/// Construct one with [`OpenAiAdapter::new`] for the default model or
/// [`OpenAiAdapter::with_model`] to name a specific model key. The adapter is
/// cheap and stateless; clone it freely. It serves any OpenAI-compatible
/// endpoint; locality/privacy is decided by the router, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiAdapter {
    model_key: String,
    /// Whether the served model exposes native tool-calling. When `false`, the
    /// adapter still maps `tool_calls` faithfully on the wire, but `describe`
    /// reports [`ToolCalling::JsonEmulated`] so the router engages the
    /// JSON-in-prompt fallback (DESIGN.md §12.4).
    native_tools: bool,
}

impl Default for OpenAiAdapter {
    fn default() -> Self {
        OpenAiAdapter::new()
    }
}

impl OpenAiAdapter {
    /// The model key used when none is specified.
    pub const DEFAULT_MODEL: &'static str = "gpt-4o";

    /// Create an adapter for the default OpenAI model.
    #[must_use]
    pub fn new() -> Self {
        OpenAiAdapter::with_model(Self::DEFAULT_MODEL)
    }

    /// Create an adapter bound to a specific OpenAI-compatible `model_key`.
    #[must_use]
    pub fn with_model(model_key: impl Into<String>) -> Self {
        OpenAiAdapter {
            model_key: model_key.into(),
            native_tools: true,
        }
    }

    /// Set whether the served model has native tool-calling (default `true`).
    ///
    /// A local OpenAI-compatible server fronting a model without tool support
    /// sets this `false`, so the router reads [`ToolCalling::JsonEmulated`] from
    /// [`describe`](OpenAiAdapter::describe) and drives tools via a JSON-in-prompt
    /// protocol (DESIGN.md §12.4).
    #[must_use]
    pub fn with_native_tools(mut self, native: bool) -> Self {
        self.native_tools = native;
        self
    }

    /// The model key this adapter renders for.
    #[must_use]
    pub fn model_key(&self) -> &str {
        &self.model_key
    }

    /// Render one assistant turn into an OpenAI assistant message, splitting
    /// text (which becomes `content`) from tool calls (which become the separate
    /// `tool_calls` array, with JSON-encoded `arguments`).
    fn assistant_to_message(turn: &CanonicalTurn) -> Result<Value, ProviderError> {
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        for block in &turn.content {
            match block {
                ContentBlock::Text(t) => text_parts.push(t.clone()),
                ContentBlock::ToolCall { id, name, input } => tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        // OpenAI requires the arguments as a JSON-ENCODED STRING.
                        "arguments": serde_json::to_string(input)
                            .map_err(|e| ProviderError::UnmappableContent(
                                format!("tool-call input is not JSON-encodable: {e}")
                            ))?,
                    },
                })),
                ContentBlock::ToolResult { .. } => {
                    return Err(ProviderError::UnmappableContent(
                        "tool_result block cannot appear in an assistant turn".to_string(),
                    ));
                }
            }
        }

        let mut msg = Map::new();
        msg.insert("role".to_string(), json!("assistant"));
        // OpenAI accepts null content alongside tool_calls; otherwise join text.
        if text_parts.is_empty() && !tool_calls.is_empty() {
            msg.insert("content".to_string(), Value::Null);
        } else {
            msg.insert("content".to_string(), json!(text_parts.join("")));
        }
        if !tool_calls.is_empty() {
            msg.insert("tool_calls".to_string(), Value::Array(tool_calls));
        }
        // Re-emit only OpenAI-native opaque blocks (e.g. reasoning) verbatim
        // under their recorded key; foreign opaque blocks never reach the wire.
        for opaque in &turn.opaque {
            if opaque.provider == OPENAI_PROVIDER_KEY {
                if let Some(obj) = opaque.data.as_object() {
                    for (k, v) in obj {
                        msg.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
            }
        }
        Ok(Value::Object(msg))
    }

    /// Render a [`Role::Tool`] turn into one OpenAI `tool` message per result
    /// block (OpenAI carries one result per message, keyed by `tool_call_id`).
    fn tool_turn_to_messages(turn: &CanonicalTurn) -> Result<Vec<Value>, ProviderError> {
        let mut out = Vec::new();
        for block in &turn.content {
            match block {
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    let mut msg = Map::new();
                    msg.insert("role".to_string(), json!("tool"));
                    msg.insert("tool_call_id".to_string(), json!(tool_call_id));
                    // Tool content is a string on the wire; JSON-encode the value
                    // so any shape (text or structured) round-trips exactly.
                    msg.insert(
                        "content".to_string(),
                        json!(serde_json::to_string(content).map_err(|e| {
                            ProviderError::UnmappableContent(format!(
                                "tool_result content is not JSON-encodable: {e}"
                            ))
                        })?),
                    );
                    if *is_error {
                        // Not part of the core OpenAI schema, but preserved so the
                        // error flag survives the round trip.
                        msg.insert("is_error".to_string(), json!(true));
                    }
                    out.push(Value::Object(msg));
                }
                _ => {
                    return Err(ProviderError::UnmappableContent(
                        "a tool turn may contain only tool_result blocks".to_string(),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Render a system or user turn (text only) into one OpenAI message.
    fn text_turn_to_message(turn: &CanonicalTurn, role: &str) -> Result<Value, ProviderError> {
        let mut parts: Vec<String> = Vec::new();
        for block in &turn.content {
            match block {
                ContentBlock::Text(t) => parts.push(t.clone()),
                _ => {
                    return Err(ProviderError::UnmappableContent(format!(
                        "a {role} turn may contain only text"
                    )));
                }
            }
        }
        Ok(json!({ "role": role, "content": parts.join("") }))
    }

    /// Parse one OpenAI message object into a [`CanonicalTurn`].
    fn message_from_wire(message: &Value) -> Result<CanonicalTurn, ProviderError> {
        let obj = message
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("message is not an object".into()))?;
        let role_str = obj
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::MalformedWire("message has no `role`".into()))?;

        match role_str {
            "system" => Ok(CanonicalTurn::text(Role::System, content_text(obj)?)),
            "user" => Ok(CanonicalTurn::text(Role::User, content_text(obj)?)),
            "tool" => {
                let tool_call_id = require_str(obj, "tool_call_id", "tool")?;
                let raw = content_text(obj)?;
                let content = serde_json::from_str(&raw).unwrap_or(Value::String(raw));
                let is_error = obj
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(CanonicalTurn {
                    role: Role::Tool,
                    content: vec![ContentBlock::ToolResult {
                        tool_call_id: tool_call_id.clone(),
                        content,
                        is_error,
                    }],
                    tool_call_id: Some(tool_call_id),
                    opaque: Vec::new(),
                })
            }
            "assistant" => Self::assistant_from_wire(obj),
            other => Err(ProviderError::MalformedWire(format!(
                "unknown message role `{other}`"
            ))),
        }
    }

    /// Parse an OpenAI assistant message (text + `tool_calls` + opaque reasoning).
    fn assistant_from_wire(obj: &Map<String, Value>) -> Result<CanonicalTurn, ProviderError> {
        let mut content: Vec<ContentBlock> = Vec::new();

        // Text content (may be null when the turn is only tool calls).
        if let Some(c) = obj.get("content") {
            if !c.is_null() {
                let text = c
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| c.to_string());
                if !text.is_empty() {
                    content.push(ContentBlock::Text(text));
                }
            }
        }

        // Tool calls live in a separate array; arguments is a JSON string.
        if let Some(calls) = obj.get("tool_calls") {
            let arr = calls.as_array().ok_or_else(|| {
                ProviderError::MalformedWire("`tool_calls` must be an array".into())
            })?;
            for call in arr {
                let cobj = call.as_object().ok_or_else(|| {
                    ProviderError::MalformedWire("tool_call is not an object".into())
                })?;
                let id = require_str(cobj, "id", "tool_call")?;
                let func = cobj
                    .get("function")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        ProviderError::MalformedWire("tool_call has no `function` object".into())
                    })?;
                let name = require_str(func, "name", "function")?;
                let args_str = func
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ProviderError::MalformedWire(
                            "function `arguments` must be a JSON-encoded string".into(),
                        )
                    })?;
                // Arguments is a JSON-encoded string; parse it back to a value.
                let input = serde_json::from_str(args_str).map_err(|e| {
                    ProviderError::MalformedWire(format!(
                        "function `arguments` is not valid JSON: {e}"
                    ))
                })?;
                content.push(ContentBlock::ToolCall { id, name, input });
            }
        }

        // Non-portable assistant fields (reasoning) become an opaque block so
        // they survive a same-provider re-render and drop on a cross-provider one.
        let mut opaque = Vec::new();
        let mut reasoning = Map::new();
        for key in ["reasoning", "reasoning_content"] {
            if let Some(v) = obj.get(key) {
                if !v.is_null() {
                    reasoning.insert(key.to_string(), v.clone());
                }
            }
        }
        if !reasoning.is_empty() {
            opaque.push(OpaqueProviderBlock {
                provider: OPENAI_PROVIDER_KEY.to_string(),
                data: Value::Object(reasoning),
            });
        }

        Ok(CanonicalTurn {
            role: Role::Assistant,
            content,
            tool_call_id: None,
            opaque,
        })
    }
}

/// Read a message's `content` as text (OpenAI content is a string, or an array
/// of `{type:"text",text}` parts for multimodal messages — text parts joined).
fn content_text(obj: &Map<String, Value>) -> Result<String, ProviderError> {
    let content = obj
        .get("content")
        .ok_or_else(|| ProviderError::MalformedWire("message has no `content`".into()))?;
    if let Some(s) = content.as_str() {
        return Ok(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for part in arr {
            if let Some(t) = part.get("text").and_then(Value::as_str) {
                parts.push(t.to_string());
            }
        }
        return Ok(parts.join(""));
    }
    if content.is_null() {
        return Ok(String::new());
    }
    Err(ProviderError::MalformedWire(
        "message `content` is neither a string nor an array".into(),
    ))
}

/// Pull a required string field, or fail with a precise malformed-wire message.
fn require_str(obj: &Map<String, Value>, key: &str, kind: &str) -> Result<String, ProviderError> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ProviderError::MalformedWire(format!("{kind} has no string `{key}`")))
}

impl ProviderAdapter for OpenAiAdapter {
    fn describe(&self) -> CapabilitySet {
        // Static-table hint for a current OpenAI-compatible frontier model
        // (DESIGN.md §12.4: a hint, refined by the P6 runtime probe).
        CapabilitySet {
            model_key: self.model_key.clone(),
            parallel_tool_calls: self.native_tools,
            structured_output: true,
            vision: true,
            prompt_caching: false,
            tool_calling: if self.native_tools {
                ToolCalling::Native
            } else {
                ToolCalling::JsonEmulated
            },
            source: CapSource::StaticTable,
        }
    }

    fn to_wire(&self, transcript: &CanonicalTranscript) -> Result<Value, ProviderError> {
        let mut messages: Vec<Value> = Vec::new();
        for turn in &transcript.turns {
            match turn.role {
                Role::System => messages.push(Self::text_turn_to_message(turn, "system")?),
                Role::User => messages.push(Self::text_turn_to_message(turn, "user")?),
                Role::Assistant => messages.push(Self::assistant_to_message(turn)?),
                Role::Tool => messages.extend(Self::tool_turn_to_messages(turn)?),
            }
        }
        Ok(json!({ "model": self.model_key, "messages": messages }))
    }

    fn from_wire(&self, wire: Value) -> Result<CanonicalTranscript, ProviderError> {
        let obj = wire
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("wire payload is not an object".into()))?;

        // Response shape: {"choices":[{"message":{...}}]} -> the assistant turn.
        if let Some(choices) = obj.get("choices") {
            let arr = choices
                .as_array()
                .ok_or_else(|| ProviderError::MalformedWire("`choices` must be an array".into()))?;
            let first = arr
                .first()
                .ok_or_else(|| ProviderError::MalformedWire("`choices` is empty".into()))?;
            let message = first
                .get("message")
                .ok_or_else(|| ProviderError::MalformedWire("choice has no `message`".into()))?;
            return Ok(CanonicalTranscript::new(vec![Self::message_from_wire(
                message,
            )?]));
        }

        // Request shape: {"messages":[...]}.
        if let Some(messages) = obj.get("messages") {
            let arr = messages.as_array().ok_or_else(|| {
                ProviderError::MalformedWire("`messages` must be an array".into())
            })?;
            let mut turns = Vec::with_capacity(arr.len());
            for message in arr {
                turns.push(Self::message_from_wire(message)?);
            }
            return Ok(CanonicalTranscript::new(turns));
        }

        // A bare single message object (role + content).
        if obj.contains_key("role") {
            return Ok(CanonicalTranscript::new(vec![Self::message_from_wire(
                &wire,
            )?]));
        }

        Err(ProviderError::MalformedWire(
            "wire payload has neither `choices`, `messages`, nor a `role` message".into(),
        ))
    }

    fn raw_passthrough(&self) -> bool {
        // The OpenAI-compatible adapter exposes the raw-passthrough escape hatch
        // for endpoint-specific features not yet modeled canonically (§12.2).
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> CanonicalTranscript {
        CanonicalTranscript::new(vec![
            CanonicalTurn::text(Role::System, "be terse"),
            CanonicalTurn::text(Role::User, "add a guard"),
            CanonicalTurn {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text("on it".into()),
                    ContentBlock::ToolCall {
                        id: "call_1".into(),
                        name: "edit".into(),
                        input: json!({ "path": "a.ts", "n": 3 }),
                    },
                ],
                tool_call_id: None,
                opaque: Vec::new(),
            },
            CanonicalTurn {
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "call_1".into(),
                    content: json!({ "ok": true }),
                    is_error: false,
                }],
                tool_call_id: Some("call_1".into()),
                opaque: Vec::new(),
            },
        ])
    }

    #[test]
    fn system_is_a_message_not_top_level() {
        let wire = OpenAiAdapter::new().to_wire(&sample()).unwrap();
        // Unlike Anthropic, there is no top-level `system` field.
        assert!(wire.get("system").is_none());
        let messages = wire["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "be terse");
    }

    #[test]
    fn tool_call_arguments_are_json_encoded_string() {
        let wire = OpenAiAdapter::new().to_wire(&sample()).unwrap();
        let messages = wire["messages"].as_array().unwrap();
        let assistant = &messages[2];
        assert_eq!(assistant["role"], "assistant");
        let call = &assistant["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "edit");
        // arguments MUST be a string, and re-parse to the original input.
        let args = call["function"]["arguments"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(args).unwrap(),
            json!({ "path": "a.ts", "n": 3 })
        );
    }

    #[test]
    fn tool_result_is_a_tool_message_keyed_by_id() {
        let wire = OpenAiAdapter::new().to_wire(&sample()).unwrap();
        let messages = wire["messages"].as_array().unwrap();
        let tool = messages.last().unwrap();
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "call_1");
    }

    #[test]
    fn round_trips_through_wire_and_back() {
        let adapter = OpenAiAdapter::new();
        let original = sample();
        let wire = adapter.to_wire(&original).unwrap();
        let back = adapter.from_wire(wire).unwrap();
        assert_eq!(back, original, "to_wire -> from_wire must be identity");
    }

    #[test]
    fn parses_a_response_choice() {
        // A completion response shape: choices[0].message.
        let response = json!({
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "done",
                    "tool_calls": [{
                        "id": "c2",
                        "type": "function",
                        "function": { "name": "run", "arguments": "{\"x\":1}" }
                    }]
                }
            }]
        });
        let t = OpenAiAdapter::new().from_wire(response).unwrap();
        assert_eq!(t.turns.len(), 1);
        let turn = &t.turns[0];
        assert_eq!(turn.role, Role::Assistant);
        assert_eq!(turn.content[0], ContentBlock::Text("done".into()));
        assert_eq!(
            turn.content[1],
            ContentBlock::ToolCall {
                id: "c2".into(),
                name: "run".into(),
                input: json!({ "x": 1 }),
            }
        );
    }

    #[test]
    fn assistant_reasoning_becomes_opaque_openai_block() {
        let response = json!({
            "role": "assistant",
            "content": "hi",
            "reasoning_content": "let me think"
        });
        let t = OpenAiAdapter::new().from_wire(response).unwrap();
        let turn = &t.turns[0];
        assert_eq!(turn.opaque.len(), 1);
        assert_eq!(turn.opaque[0].provider, OPENAI_PROVIDER_KEY);
        assert_eq!(turn.opaque[0].data["reasoning_content"], "let me think");
    }

    #[test]
    fn malformed_wire_is_rejected_loudly() {
        let adapter = OpenAiAdapter::new();
        // arguments must be a string, not an object.
        let bad = json!({
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{ "id": "x", "type": "function",
                    "function": { "name": "n", "arguments": { "not": "a string" } } }]
            }]
        });
        assert!(matches!(
            adapter.from_wire(bad),
            Err(ProviderError::MalformedWire(_))
        ));
    }

    #[test]
    fn describe_reports_json_emulated_when_not_native() {
        let a = OpenAiAdapter::with_model("local/foo").with_native_tools(false);
        assert_eq!(a.describe().tool_calling, ToolCalling::JsonEmulated);
        assert!(!a.describe().parallel_tool_calls);
    }
}
