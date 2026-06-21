//! The [`CliAdapter`] — the canonical <-> agent-CLI JSONL mapping.
//!
//! Some "providers" are not HTTP endpoints but **command-line agents** (e.g. the
//! GitHub Copilot CLI) driven over a pipe with a newline-delimited JSON
//! transcript (DESIGN.md §12.1, §12.6 treats a CLI agent as a provider behind the
//! same port). This adapter maps a [`CanonicalTranscript`](crate::CanonicalTranscript)
//! to and from that JSONL shape. Like the other adapters it is **mapping only** —
//! the subprocess/pipe transport that writes the lines to the CLI's stdin and
//! reads its stdout is a separate seam.
//!
//! # The JSONL shape
//!
//! Each conversation turn is one JSON object (one line on the wire); a payload is
//! `{"lines": [<turn>, ...]}` (an envelope so a single response line round-trips
//! too). A turn is `{"role": "system|user|assistant|tool", "content": [block,...]}`
//! where each block is one of:
//!
//! - `{"type":"text","text": "..."}`
//! - `{"type":"tool_call","id":"...","name":"...","input": <json>}`
//! - `{"type":"tool_result","tool_call_id":"...","content": <json>,"is_error": <bool>}`
//!
//! Unlike OpenAI, tool-call `input` stays a JSON value (not a stringified
//! string); unlike Anthropic, the system prompt is an ordinary `system` line, not
//! a top-level field. Tool-call ids are carried verbatim so the correlation the
//! agent loop depends on survives the round trip (DESIGN.md §12.6).

use serde_json::{json, Map, Value};

use crate::{
    CanonicalTranscript, CanonicalTurn, CapSource, CapabilitySet, ContentBlock, ProviderAdapter,
    ProviderError, Role, ToolCalling,
};

/// The agent-CLI JSONL adapter (mapping only; no subprocess).
///
/// Construct with [`CliAdapter::new`] for the default agent or
/// [`CliAdapter::with_model`] to name a specific CLI agent/model key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliAdapter {
    model_key: String,
}

impl Default for CliAdapter {
    fn default() -> Self {
        CliAdapter::new()
    }
}

impl CliAdapter {
    /// The model/agent key used when none is specified.
    pub const DEFAULT_MODEL: &'static str = "copilot-cli";

    /// Create an adapter for the default CLI agent.
    #[must_use]
    pub fn new() -> Self {
        CliAdapter::with_model(Self::DEFAULT_MODEL)
    }

    /// Create an adapter bound to a specific CLI agent/model key.
    #[must_use]
    pub fn with_model(model_key: impl Into<String>) -> Self {
        CliAdapter {
            model_key: model_key.into(),
        }
    }

    /// The model/agent key this adapter renders for.
    #[must_use]
    pub fn model_key(&self) -> &str {
        &self.model_key
    }

    fn role_str(role: Role) -> &'static str {
        match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }

    fn role_from_str(s: &str) -> Result<Role, ProviderError> {
        match s {
            "system" => Ok(Role::System),
            "user" => Ok(Role::User),
            "assistant" => Ok(Role::Assistant),
            "tool" => Ok(Role::Tool),
            other => Err(ProviderError::MalformedWire(format!(
                "unknown JSONL role `{other}`"
            ))),
        }
    }

    fn block_to_wire(block: &ContentBlock) -> Value {
        match block {
            ContentBlock::Text(t) => json!({ "type": "text", "text": t }),
            ContentBlock::ToolCall { id, name, input } => json!({
                "type": "tool_call",
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
                "tool_call_id": tool_call_id,
                "content": content,
                "is_error": is_error,
            }),
        }
    }

    fn block_from_wire(value: &Value) -> Result<ContentBlock, ProviderError> {
        let obj = value
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("JSONL block is not an object".into()))?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| ProviderError::MalformedWire("JSONL block has no `type`".into()))?;
        match kind {
            "text" => Ok(ContentBlock::Text(require_str(obj, "text", "text")?)),
            "tool_call" => Ok(ContentBlock::ToolCall {
                id: require_str(obj, "id", "tool_call")?,
                name: require_str(obj, "name", "tool_call")?,
                input: obj.get("input").cloned().unwrap_or(Value::Null),
            }),
            "tool_result" => Ok(ContentBlock::ToolResult {
                tool_call_id: require_str(obj, "tool_call_id", "tool_result")?,
                content: obj.get("content").cloned().unwrap_or(Value::Null),
                is_error: obj
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            other => Err(ProviderError::MalformedWire(format!(
                "unknown JSONL block type `{other}`"
            ))),
        }
    }

    fn turn_to_wire(turn: &CanonicalTurn) -> Value {
        let content: Vec<Value> = turn.content.iter().map(Self::block_to_wire).collect();
        json!({ "role": Self::role_str(turn.role), "content": content })
    }

    fn turn_from_wire(value: &Value) -> Result<CanonicalTurn, ProviderError> {
        let obj = value
            .as_object()
            .ok_or_else(|| ProviderError::MalformedWire("JSONL turn is not an object".into()))?;
        let role = Self::role_from_str(
            obj.get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| ProviderError::MalformedWire("JSONL turn has no `role`".into()))?,
        )?;
        let content_arr = obj
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::MalformedWire("JSONL turn has no `content` array".into())
            })?;
        let mut content = Vec::with_capacity(content_arr.len());
        for block in content_arr {
            content.push(Self::block_from_wire(block)?);
        }
        // A single tool_result turn carries its id at the turn level too, matching
        // how the other adapters surface it.
        let tool_call_id = if role == Role::Tool && content.len() == 1 {
            match &content[0] {
                ContentBlock::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            }
        } else {
            None
        };
        Ok(CanonicalTurn {
            role,
            content,
            tool_call_id,
            opaque: Vec::new(),
        })
    }
}

fn require_str(obj: &Map<String, Value>, key: &str, kind: &str) -> Result<String, ProviderError> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ProviderError::MalformedWire(format!("{kind} block has no string `{key}`")))
}

impl ProviderAdapter for CliAdapter {
    fn describe(&self) -> CapabilitySet {
        // A CLI agent is driven by text; tool use is the agent's own concern, so
        // from Spork's seam it is a json-emulated, no-prompt-cache provider
        // (DESIGN.md §12.4). Static hint, refined by a P6 probe if available.
        CapabilitySet {
            model_key: self.model_key.clone(),
            parallel_tool_calls: false,
            structured_output: false,
            vision: false,
            prompt_caching: false,
            tool_calling: ToolCalling::JsonEmulated,
            source: CapSource::StaticTable,
        }
    }

    fn to_wire(&self, transcript: &CanonicalTranscript) -> Result<Value, ProviderError> {
        let lines: Vec<Value> = transcript.turns.iter().map(Self::turn_to_wire).collect();
        Ok(json!({ "model": self.model_key, "lines": lines }))
    }

    fn from_wire(&self, wire: Value) -> Result<CanonicalTranscript, ProviderError> {
        // Accept either the {"lines":[...]} envelope, a bare array of turns, or a
        // single bare turn object (one response line).
        let turns_val: Vec<Value> = if let Some(lines) = wire.get("lines") {
            lines
                .as_array()
                .ok_or_else(|| ProviderError::MalformedWire("`lines` must be an array".into()))?
                .clone()
        } else if let Some(arr) = wire.as_array() {
            arr.clone()
        } else if wire.get("role").is_some() {
            vec![wire]
        } else {
            return Err(ProviderError::MalformedWire(
                "JSONL payload has neither `lines`, an array, nor a `role` turn".into(),
            ));
        };
        let mut turns = Vec::with_capacity(turns_val.len());
        for t in &turns_val {
            turns.push(Self::turn_from_wire(t)?);
        }
        Ok(CanonicalTranscript::new(turns))
    }

    fn raw_passthrough(&self) -> bool {
        // A CLI agent has no canonical "raw request" passthrough; it is driven
        // entirely through the JSONL transcript.
        false
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
                content: vec![ContentBlock::ToolCall {
                    id: "call_1".into(),
                    name: "edit".into(),
                    input: json!({ "path": "a.ts" }),
                }],
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
    fn emits_jsonl_lines_with_neutral_roles() {
        let wire = CliAdapter::new().to_wire(&sample()).unwrap();
        let lines = wire["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["role"], "system");
        assert_eq!(lines[0]["content"][0]["type"], "text");
    }

    #[test]
    fn tool_call_input_stays_a_json_value_not_a_string() {
        let wire = CliAdapter::new().to_wire(&sample()).unwrap();
        let call = &wire["lines"][2]["content"][0];
        assert_eq!(call["type"], "tool_call");
        assert_eq!(call["id"], "call_1");
        // Unlike OpenAI, input is a JSON object here, not a stringified string.
        assert!(call["input"].is_object());
        assert_eq!(call["input"]["path"], "a.ts");
    }

    #[test]
    fn round_trips_through_wire_and_back() {
        let adapter = CliAdapter::new();
        let original = sample();
        let wire = adapter.to_wire(&original).unwrap();
        let back = adapter.from_wire(wire).unwrap();
        assert_eq!(back, original, "to_wire -> from_wire must be identity");
    }

    #[test]
    fn parses_a_bare_single_turn_line() {
        let line = json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }] });
        let t = CliAdapter::new().from_wire(line).unwrap();
        assert_eq!(t.turns.len(), 1);
        assert_eq!(t.turns[0].role, Role::Assistant);
        assert_eq!(t.turns[0].content[0], ContentBlock::Text("done".into()));
    }

    #[test]
    fn describe_is_json_emulated() {
        assert_eq!(
            CliAdapter::new().describe().tool_calling,
            ToolCalling::JsonEmulated
        );
    }

    #[test]
    fn malformed_block_type_is_rejected() {
        let bad = json!({ "lines": [{ "role": "user", "content": [{ "type": "mystery" }] }] });
        assert!(matches!(
            CliAdapter::new().from_wire(bad),
            Err(ProviderError::MalformedWire(_))
        ));
    }
}
