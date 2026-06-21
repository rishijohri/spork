//! Contract tests for the F4 model seam (DESIGN.md §5.4, §12.1-§12.3).
//!
//! These exercise the four required behaviors from the F4 contract plus
//! adjacent edge cases, and validate the Anthropic mapping against handwritten
//! Anthropic-shaped JSON fixtures (no HTTP anywhere).

use serde_json::{json, Value};

use spork_provider::{
    AnthropicAdapter, CanonicalTranscript, CanonicalTurn, CapSource, CapabilitySet, ContentBlock,
    Locality, ModelRouter, ModelSelector, OpaqueProviderBlock, PrivacyClass, ProviderAdapter,
    ProviderError, ProviderProjection, Role, SingleProviderRouter, ToolCalling,
    ANTHROPIC_PROVIDER_KEY, CANONICAL_TRANSCRIPT_SCHEMA_VERSION,
};

fn load_fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

/// A transcript with all portable content kinds: system, user, an assistant
/// tool call, and a tool result.
fn portable_transcript() -> CanonicalTranscript {
    CanonicalTranscript::new(vec![
        CanonicalTurn::text(Role::System, "You are a careful coding assistant."),
        CanonicalTurn::text(Role::User, "List the files in the repo root."),
        CanonicalTurn {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text("I'll list them.".into()),
                ContentBlock::ToolCall {
                    id: "toolu_01".into(),
                    name: "list_dir".into(),
                    input: json!({ "path": "." }),
                },
            ],
            tool_call_id: None,
            opaque: vec![],
        },
        CanonicalTurn {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "toolu_01".into(),
                content: json!("Cargo.toml\nsrc\n"),
                is_error: false,
            }],
            tool_call_id: Some("toolu_01".into()),
            opaque: vec![],
        },
    ])
}

// ----- Required test 1: lossless round trip for portable content -------------

#[test]
fn portable_transcript_round_trips_through_wire() {
    let adapter = AnthropicAdapter::new();
    let original = portable_transcript();

    let wire = adapter.to_wire(&original).expect("to_wire");
    let back = adapter.from_wire(wire).expect("from_wire");

    // Portable content survives the round trip byte-for-byte at the canonical level.
    assert_eq!(
        back, original,
        "portable content must round-trip losslessly"
    );
}

#[test]
fn round_trip_is_stable_under_repeated_projection() {
    let adapter = AnthropicAdapter::new();
    let original = portable_transcript();
    let once = adapter
        .from_wire(adapter.to_wire(&original).unwrap())
        .unwrap();
    let twice = adapter.from_wire(adapter.to_wire(&once).unwrap()).unwrap();
    assert_eq!(once, twice);
    assert_eq!(once, original);
}

// ----- Anthropic wire mapping against fixtures -------------------------------

#[test]
fn to_wire_matches_handwritten_request_fixture() {
    let adapter = AnthropicAdapter::new();
    let wire = adapter.to_wire(&portable_transcript()).unwrap();

    // System prompt lifted to the top-level field, never a message.
    assert_eq!(wire["system"], json!("You are a careful coding assistant."));
    assert!(
        wire["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != json!("system")),
        "system turn must not appear as a message"
    );

    // Tool turn rendered as a `user` message of tool_result blocks.
    let messages = wire["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], json!("user"));
    assert_eq!(last["content"][0]["type"], json!("tool_result"));
    assert_eq!(last["content"][0]["tool_use_id"], json!("toolu_01"));

    // Assistant tool call uses the `tool_use` block with the id preserved.
    let assistant = &messages[1];
    assert_eq!(assistant["content"][1]["type"], json!("tool_use"));
    assert_eq!(assistant["content"][1]["id"], json!("toolu_01"));
    assert_eq!(assistant["content"][1]["name"], json!("list_dir"));
}

#[test]
fn from_wire_parses_handwritten_request_fixture() {
    let adapter = AnthropicAdapter::new();
    let wire = load_fixture("anthropic_request.json");
    let transcript = adapter.from_wire(wire).unwrap();

    assert_eq!(
        transcript.schema_version,
        CANONICAL_TRANSCRIPT_SCHEMA_VERSION
    );
    // system + user + assistant + tool turn.
    assert_eq!(transcript.turns.len(), 4);
    assert_eq!(transcript.turns[0].role, Role::System);
    assert_eq!(transcript.turns[1].role, Role::User);
    assert_eq!(transcript.turns[2].role, Role::Assistant);
    // A user message of only tool_result blocks is classified as a Tool turn.
    assert_eq!(transcript.turns[3].role, Role::Tool);
    assert_eq!(
        transcript.turns[3].tool_call_id.as_deref(),
        Some("toolu_01")
    );
}

#[test]
fn from_wire_parses_response_shape_single_message() {
    let adapter = AnthropicAdapter::new();
    let wire = load_fixture("anthropic_response.json");
    let transcript = adapter.from_wire(wire).unwrap();

    // A bare role+content message (the response shape) becomes one turn.
    assert_eq!(transcript.turns.len(), 1);
    let turn = &transcript.turns[0];
    assert_eq!(turn.role, Role::Assistant);
    assert!(matches!(turn.content[0], ContentBlock::Text(_)));
    match &turn.content[1] {
        ContentBlock::ToolCall { id, name, .. } => {
            assert_eq!(id, "toolu_99");
            assert_eq!(name, "write_file");
        }
        other => panic!("expected tool call, got {other:?}"),
    }
}

#[test]
fn from_wire_preserves_unknown_block_as_opaque_anthropic() {
    let adapter = AnthropicAdapter::new();
    let wire = load_fixture("anthropic_response_thinking.json");
    let transcript = adapter.from_wire(wire).unwrap();

    let turn = &transcript.turns[0];
    // The `thinking` block is not portable; it is captured as an opaque,
    // anthropic-tagged block, while the text remains a portable block.
    assert_eq!(turn.content, vec![ContentBlock::Text("Hello!".into())]);
    assert_eq!(turn.opaque.len(), 1);
    assert_eq!(turn.opaque[0].provider, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(turn.opaque[0].data["type"], json!("thinking"));
}

#[test]
fn anthropic_opaque_block_re_renders_to_wire_verbatim() {
    let adapter = AnthropicAdapter::new();
    // A turn carrying a native anthropic opaque block re-emits it on the wire.
    let transcript = CanonicalTranscript::new(vec![CanonicalTurn {
        role: Role::Assistant,
        content: vec![ContentBlock::Text("Hi".into())],
        tool_call_id: None,
        opaque: vec![OpaqueProviderBlock {
            provider: ANTHROPIC_PROVIDER_KEY.into(),
            data: json!({"type": "thinking", "thinking": "...", "signature": "s"}),
        }],
    }]);
    let wire = adapter.to_wire(&transcript).unwrap();
    let content = wire["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[1]["type"], json!("thinking"));
}

#[test]
fn to_wire_omits_system_when_absent() {
    let adapter = AnthropicAdapter::new();
    let transcript = CanonicalTranscript::new(vec![CanonicalTurn::text(Role::User, "hi")]);
    let wire = adapter.to_wire(&transcript).unwrap();
    assert!(wire.get("system").is_none());
    assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn from_wire_rejects_malformed_payloads() {
    let adapter = AnthropicAdapter::new();

    // Not an object.
    assert!(matches!(
        adapter.from_wire(json!([])),
        Err(ProviderError::MalformedWire(_))
    ));
    // Neither messages nor a role/content message.
    assert!(matches!(
        adapter.from_wire(json!({"model": "x"})),
        Err(ProviderError::MalformedWire(_))
    ));
    // A tool_result with no tool_use_id loses the correlation -> rejected.
    let bad = json!({
        "role": "user",
        "content": [{ "type": "tool_result", "content": "x" }]
    });
    assert!(matches!(
        adapter.from_wire(bad),
        Err(ProviderError::MalformedWire(_))
    ));
    // An unknown content-block type with no `type` key -> rejected.
    let bad = json!({ "role": "assistant", "content": [{ "text": "x" }] });
    assert!(matches!(
        adapter.from_wire(bad),
        Err(ProviderError::MalformedWire(_))
    ));
}

// ----- Required test 2: cross-provider projection drops opaque + warns -------

#[test]
fn cross_provider_projection_drops_foreign_opaque_with_warning() {
    let transcript = CanonicalTranscript::new(vec![CanonicalTurn {
        role: Role::Assistant,
        content: vec![ContentBlock::Text("answer".into())],
        tool_call_id: None,
        opaque: vec![OpaqueProviderBlock {
            provider: ANTHROPIC_PROVIDER_KEY.into(),
            data: json!({"type": "thinking", "thinking": "secret reasoning"}),
        }],
    }]);

    // Projecting onto a different provider drops the anthropic block and warns.
    let projected = ProviderProjection::new().project(&transcript, "openai");
    assert!(projected.is_lossy());
    assert_eq!(projected.warnings.len(), 1);
    assert_eq!(
        projected.warnings[0].dropped_provider,
        ANTHROPIC_PROVIDER_KEY
    );
    assert_eq!(projected.warnings[0].turn_index, 0);
    assert_eq!(projected.warnings[0].warning, "lossyProjection");
    assert!(projected.transcript.turns[0].opaque.is_empty());
    // Portable content is untouched.
    assert_eq!(
        projected.transcript.turns[0].content,
        transcript.turns[0].content
    );
}

#[test]
fn same_provider_projection_is_lossless() {
    let transcript = CanonicalTranscript::new(vec![CanonicalTurn {
        role: Role::Assistant,
        content: vec![ContentBlock::Text("answer".into())],
        tool_call_id: None,
        opaque: vec![OpaqueProviderBlock {
            provider: ANTHROPIC_PROVIDER_KEY.into(),
            data: json!({"type": "thinking"}),
        }],
    }]);
    let projected = ProviderProjection::new().project(&transcript, ANTHROPIC_PROVIDER_KEY);
    assert!(!projected.is_lossy());
    assert_eq!(projected.transcript, transcript);
}

#[test]
fn projection_keeps_only_target_provider_blocks() {
    let transcript = CanonicalTranscript::new(vec![CanonicalTurn {
        role: Role::Assistant,
        content: vec![],
        tool_call_id: None,
        opaque: vec![
            OpaqueProviderBlock {
                provider: "anthropic".into(),
                data: json!(1),
            },
            OpaqueProviderBlock {
                provider: "openai".into(),
                data: json!(2),
            },
            OpaqueProviderBlock {
                provider: "anthropic".into(),
                data: json!(3),
            },
        ],
    }]);
    let projected = ProviderProjection::new().project(&transcript, "anthropic");
    assert_eq!(projected.warnings.len(), 1); // only the openai block dropped
    assert_eq!(projected.transcript.turns[0].opaque.len(), 2);
    assert!(projected.transcript.turns[0]
        .opaque
        .iter()
        .all(|b| b.provider == "anthropic"));
}

// ----- Required test 3: router resolves Anthropic, refuses LocalOnly ---------

#[test]
fn router_resolves_every_selector_to_anthropic() {
    let router = SingleProviderRouter::new();
    for selector in [
        ModelSelector::default_model(),
        ModelSelector::pinned("claude-3-opus"),
    ] {
        let resolved = router.resolve(&selector, PrivacyClass::Any).unwrap();
        assert_eq!(resolved.provider, ANTHROPIC_PROVIDER_KEY);
        assert_eq!(resolved.locality, Locality::FirstPartyCloud);
    }
}

#[test]
fn router_refuses_local_only_routed_to_cloud() {
    let router = SingleProviderRouter::new();
    let err = router
        .resolve(&ModelSelector::default_model(), PrivacyClass::LocalOnly)
        .unwrap_err();
    match err {
        ProviderError::PrivacyViolation {
            requested,
            provider,
        } => {
            assert_eq!(requested, "local_only");
            assert_eq!(provider, ANTHROPIC_PROVIDER_KEY);
        }
        other => panic!("expected PrivacyViolation, got {other:?}"),
    }
}

#[test]
fn router_permits_no_third_party_aggregator_for_first_party_cloud() {
    let router = SingleProviderRouter::new();
    // Anthropic is first-party cloud, not an aggregator, so this is permitted.
    let resolved = router
        .resolve(
            &ModelSelector::default_model(),
            PrivacyClass::NoThirdPartyAggregator,
        )
        .unwrap();
    assert_eq!(resolved.provider, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(resolved.privacy, PrivacyClass::NoThirdPartyAggregator);
}

#[test]
fn router_pinned_selector_carries_through_to_capabilities() {
    let router = SingleProviderRouter::new();
    let resolved = router
        .resolve(&ModelSelector::pinned("claude-x"), PrivacyClass::Any)
        .unwrap();
    assert_eq!(resolved.capabilities.model_key, "claude-x");
}

#[test]
fn router_has_no_fallback_in_v1() {
    let router = SingleProviderRouter::new();
    let resolved = router
        .resolve(&ModelSelector::default_model(), PrivacyClass::Any)
        .unwrap();
    assert!(router.next_fallback(&resolved).is_none());
}

// ----- Required test 4: CapabilitySet describe is stable --------------------

#[test]
fn describe_is_stable() {
    let adapter = AnthropicAdapter::with_model("claude-3-5-sonnet");
    let a = adapter.describe();
    let b = adapter.describe();
    assert_eq!(a, b);
    assert_eq!(
        a,
        CapabilitySet {
            model_key: "claude-3-5-sonnet".into(),
            parallel_tool_calls: true,
            structured_output: true,
            vision: true,
            prompt_caching: true,
            tool_calling: ToolCalling::Native,
            source: CapSource::StaticTable,
        }
    );
    assert!(adapter.raw_passthrough());
}

// ----- Privacy policy unit coverage -----------------------------------------

#[test]
fn privacy_class_permits_matrix() {
    use Locality::*;
    use PrivacyClass::*;
    assert!(LocalOnly.permits(Local));
    assert!(!LocalOnly.permits(FirstPartyCloud));
    assert!(!LocalOnly.permits(ThirdPartyAggregator));

    assert!(NoThirdPartyAggregator.permits(Local));
    assert!(NoThirdPartyAggregator.permits(FirstPartyCloud));
    assert!(!NoThirdPartyAggregator.permits(ThirdPartyAggregator));

    assert!(Any.permits(Local));
    assert!(Any.permits(FirstPartyCloud));
    assert!(Any.permits(ThirdPartyAggregator));
}

// ----- Content-addressing / serde round trips -------------------------------

#[test]
fn transcript_content_hash_is_stable_and_content_addressed() {
    let a = portable_transcript();
    let b = portable_transcript();
    assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());

    // A different conversation hashes differently.
    let c = CanonicalTranscript::new(vec![CanonicalTurn::text(Role::User, "different")]);
    assert_ne!(a.content_hash().unwrap(), c.content_hash().unwrap());
}

#[test]
fn transcript_serde_round_trips() {
    let original = portable_transcript();
    let json = serde_json::to_string(&original).unwrap();
    let back: CanonicalTranscript = serde_json::from_str(&json).unwrap();
    assert_eq!(original, back);
}

#[test]
fn model_selector_serde_round_trips_and_is_versioned() {
    // P6 widened the selector to v2 (pinned | policy | inheritFromParent).
    let sel = ModelSelector::pinned("claude-x");
    let json = serde_json::to_value(&sel).unwrap();
    assert_eq!(json["schema_version"], json!(2));
    // A pinned selector serializes byte-identically to the v1 shape: no `mode`
    // key (skip_serializing_if), so the widening is a no-domino addition.
    assert!(json.get("mode").is_none(), "pinned selector omits `mode`");
    let back: ModelSelector = serde_json::from_value(json).unwrap();
    assert_eq!(sel, back);
}

#[test]
fn v1_shaped_selector_migrates_forward_to_pinned() {
    // A stored v1 selector (no `mode` field) reads as Pinned — the forward
    // migration the schema-version bump promised (CLAUDE.md C5).
    let v1 = json!({ "schema_version": 1, "model_key": "claude-x" });
    let sel: ModelSelector = serde_json::from_value(v1).unwrap();
    assert_eq!(sel.model_key, "claude-x");
    assert!(sel.mode.is_pinned());
}

#[test]
fn privacy_class_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_value(PrivacyClass::LocalOnly).unwrap(),
        json!("local_only")
    );
    assert_eq!(
        serde_json::to_value(PrivacyClass::NoThirdPartyAggregator).unwrap(),
        json!("no_third_party_aggregator")
    );
}
