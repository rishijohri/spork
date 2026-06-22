//! Per-provider token-usage extraction from a wire response.
//!
//! Each provider reports usage under a different shape. The
//! [`CostAccountant`](spork_cost::CostAccountant) prices a single
//! provider-neutral [`Usage`]; this module is the small adapter that reads each
//! provider's response object into that shape (DESIGN §12.5). A response with no
//! usage block (e.g. a bare CLI agent that reports none) prices as zero — tokens
//! counted only when the provider reports them, never invented.

use serde_json::Value;
use spork_cost::Usage;
use spork_provider::{
    AGGREGATOR_PROVIDER_KEY, ANTHROPIC_PROVIDER_KEY, CLI_PROVIDER_KEY, LOCAL_PROVIDER_KEY,
    OPENAI_PROVIDER_KEY,
};

/// Read a `u64` field from an object, defaulting to 0 when absent or non-numeric.
fn u64_field(obj: &Value, key: &str) -> u64 {
    obj.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Extract the token [`Usage`] a `provider` reported in its `wire` response.
///
/// Recognizes the OpenAI-compatible shape (OpenAI, OpenRouter, local servers) and
/// the Anthropic shape; an unrecognized provider or a response with no usage
/// block yields a zero [`Usage`] (priced free), so cost is only ever attributed
/// from numbers the model actually returned.
#[must_use]
pub fn extract_usage(provider: &str, wire: &Value) -> Usage {
    match provider {
        ANTHROPIC_PROVIDER_KEY => anthropic_usage(wire),
        OPENAI_PROVIDER_KEY | LOCAL_PROVIDER_KEY | AGGREGATOR_PROVIDER_KEY => openai_usage(wire),
        // A CLI agent may carry an OpenAI-ish usage block or none at all.
        CLI_PROVIDER_KEY => openai_usage(wire),
        _ => Usage::default(),
    }
}

/// OpenAI-compatible: `usage.{prompt_tokens, completion_tokens}`, with
/// `usage.prompt_tokens_details.cached_tokens` counted as a cache read. The
/// reported `prompt_tokens` is the *total* prompt (it includes the cached
/// portion), so the uncached input is `prompt_tokens − cached`.
fn openai_usage(wire: &Value) -> Usage {
    let Some(usage) = wire.get("usage") else {
        return Usage::default();
    };
    let prompt = u64_field(usage, "prompt_tokens");
    let completion = u64_field(usage, "completion_tokens");
    let cached = usage
        .get("prompt_tokens_details")
        .map(|d| u64_field(d, "cached_tokens"))
        .unwrap_or(0);
    Usage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: completion,
        cache_read_tokens: cached,
        cache_write_tokens: 0,
    }
}

/// Anthropic: `usage.{input_tokens, output_tokens, cache_read_input_tokens,
/// cache_creation_input_tokens}`. Anthropic reports the uncached input separately
/// from the cache read/write, so each maps directly.
fn anthropic_usage(wire: &Value) -> Usage {
    let Some(usage) = wire.get("usage") else {
        return Usage::default();
    };
    Usage {
        input_tokens: u64_field(usage, "input_tokens"),
        output_tokens: u64_field(usage, "output_tokens"),
        cache_read_tokens: u64_field(usage, "cache_read_input_tokens"),
        cache_write_tokens: u64_field(usage, "cache_creation_input_tokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_splits_cached_from_uncached_prompt() {
        let wire = json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "prompt_tokens_details": { "cached_tokens": 300 }
            }
        });
        let u = extract_usage(OPENAI_PROVIDER_KEY, &wire);
        assert_eq!(u.input_tokens, 700); // 1000 total − 300 cached
        assert_eq!(u.cache_read_tokens, 300);
        assert_eq!(u.output_tokens, 200);
        assert_eq!(u.cache_write_tokens, 0);
    }

    #[test]
    fn openai_without_cache_details_counts_full_prompt() {
        let wire = json!({ "usage": { "prompt_tokens": 50, "completion_tokens": 10 } });
        let u = extract_usage(LOCAL_PROVIDER_KEY, &wire);
        assert_eq!(u.input_tokens, 50);
        assert_eq!(u.cache_read_tokens, 0);
        assert_eq!(u.output_tokens, 10);
    }

    #[test]
    fn anthropic_maps_cache_read_and_write() {
        let wire = json!({
            "usage": {
                "input_tokens": 400,
                "output_tokens": 120,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 500
            }
        });
        let u = extract_usage(ANTHROPIC_PROVIDER_KEY, &wire);
        assert_eq!(u.input_tokens, 400);
        assert_eq!(u.output_tokens, 120);
        assert_eq!(u.cache_read_tokens, 1000);
        assert_eq!(u.cache_write_tokens, 500);
    }

    #[test]
    fn missing_usage_block_is_zero() {
        assert_eq!(
            extract_usage(OPENAI_PROVIDER_KEY, &json!({ "choices": [] })),
            Usage::default()
        );
        assert_eq!(
            extract_usage(CLI_PROVIDER_KEY, &json!({ "lines": [] })),
            Usage::default()
        );
    }

    #[test]
    fn unknown_provider_is_zero() {
        assert_eq!(
            extract_usage(
                "some-future-provider",
                &json!({ "usage": { "prompt_tokens": 9 } })
            ),
            Usage::default()
        );
    }
}
