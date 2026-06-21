//! Spork P6 cost accounting — cache-aware spend, attributed per node.
//!
//! A model turn reports token [`Usage`]; the [`CostAccountant`] maps it through a
//! per-model [`ModelPricing`] table into the canonical
//! [`CostRecord`](spork_graph::CostRecord) the node envelope carries, so the
//! timeline becomes an auditable cost ledger (DESIGN §5.4, §12.5). It models the
//! cache economics that make Spork's lineage-aware context worthwhile: a cache
//! **read** is cheap (≈0.1× the input price) and a cache **write** carries a
//! premium (≈1.25–2× input), so re-using a stable context prefix across sibling
//! branches is a real, reportable saving (DESIGN §12.5, §13.2).
//!
//! # Why integer micro-USD
//!
//! Money is recorded in **micro-USD** (millionths of a dollar) as integers, and
//! prices are quoted per **million tokens** (`per_mtok`), so every figure is a
//! `u64` and the resulting [`CostRecord`] round-trips byte-identically under
//! `spork-canon` (which forbids floats in identity-bearing data). Cache
//! multipliers are expressed in **permille** (parts per thousand: `100` = 0.1×,
//! `1250` = 1.25×) so the whole calculation is exact integer arithmetic, done in
//! `u128` to avoid overflow before narrowing to the stored `u64`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use spork_graph::CostRecord;

/// The token usage a single model turn reports.
///
/// `input_tokens` is the *uncached* prompt; `cache_read_tokens` and
/// `cache_write_tokens` are the prompt portions served from / written to the
/// provider's prompt cache, priced differently (DESIGN §12.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached prompt/input tokens (full input price).
    pub input_tokens: u64,
    /// Completion/output tokens produced.
    pub output_tokens: u64,
    /// Prompt tokens served from the provider's cache (cheap read).
    pub cache_read_tokens: u64,
    /// Prompt tokens written to the provider's cache (premium write).
    pub cache_write_tokens: u64,
}

impl Usage {
    /// A plain (no-cache) usage of `input`/`output` tokens.
    #[must_use]
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }
    }

    /// Total prompt tokens (uncached + cache read + cache write).
    #[must_use]
    pub fn total_input_tokens(self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }
}

/// Per-model pricing in integer micro-USD per million tokens, with cache
/// multipliers in permille of the input price (DESIGN §12.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// The model this prices (e.g. `"claude-3-5-sonnet"`).
    pub model_key: String,
    /// Micro-USD per million uncached input tokens.
    pub input_micro_usd_per_mtok: u64,
    /// Micro-USD per million output tokens.
    pub output_micro_usd_per_mtok: u64,
    /// Cache-read price as permille of the input price (e.g. `100` = 0.1×).
    pub cache_read_permille: u64,
    /// Cache-write price as permille of the input price (e.g. `1250` = 1.25×).
    pub cache_write_permille: u64,
}

impl ModelPricing {
    /// Free pricing (a local server or an unknown model): every figure zero.
    #[must_use]
    pub fn free(model_key: impl Into<String>) -> Self {
        ModelPricing {
            model_key: model_key.into(),
            input_micro_usd_per_mtok: 0,
            output_micro_usd_per_mtok: 0,
            cache_read_permille: 0,
            cache_write_permille: 0,
        }
    }
}

/// `tokens × micro_usd_per_mtok ÷ 1_000_000`, in `u128` then narrowed to `u64`.
fn cost_of(tokens: u64, micro_usd_per_mtok: u64) -> u64 {
    let micros = (tokens as u128) * (micro_usd_per_mtok as u128) / 1_000_000u128;
    u64::try_from(micros).unwrap_or(u64::MAX)
}

/// Permille of a base price: `base × permille ÷ 1000`.
fn apply_permille(base_micro_usd_per_mtok: u64, permille: u64) -> u64 {
    let v = (base_micro_usd_per_mtok as u128) * (permille as u128) / 1000u128;
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// The cost accountant: a per-model pricing table that turns [`Usage`] into a
/// canonical [`CostRecord`] (DESIGN §5.4, §12.5).
#[derive(Debug, Clone, Default)]
pub struct CostAccountant {
    pricing: HashMap<String, ModelPricing>,
}

impl CostAccountant {
    /// An empty accountant (every unknown model prices as free).
    #[must_use]
    pub fn empty() -> Self {
        CostAccountant {
            pricing: HashMap::new(),
        }
    }

    /// An accountant seeded with built-in model pricing (cloud models priced;
    /// local/CLI free). Figures are representative list prices in micro-USD/Mtok.
    #[must_use]
    pub fn with_builtin_pricing() -> Self {
        let mut acc = CostAccountant::empty();
        // Anthropic Claude: $3 in / $15 out per Mtok; cache read 0.1×, write 1.25×.
        acc.insert(ModelPricing {
            model_key: "claude-3-5-sonnet".into(),
            input_micro_usd_per_mtok: 3_000_000,
            output_micro_usd_per_mtok: 15_000_000,
            cache_read_permille: 100,
            cache_write_permille: 1250,
        });
        // OpenAI gpt-4o: $2.5 in / $10 out; cached input 0.5×, no write premium.
        acc.insert(ModelPricing {
            model_key: "gpt-4o".into(),
            input_micro_usd_per_mtok: 2_500_000,
            output_micro_usd_per_mtok: 10_000_000,
            cache_read_permille: 500,
            cache_write_permille: 1000,
        });
        // Local + CLI agents: free at the seam (no per-token spend).
        acc.insert(ModelPricing::free("llama3.1"));
        acc.insert(ModelPricing::free("copilot-cli"));
        acc
    }

    /// Insert or replace a model's pricing.
    pub fn insert(&mut self, pricing: ModelPricing) {
        self.pricing.insert(pricing.model_key.clone(), pricing);
    }

    /// The pricing for `model_key`, or free pricing if unknown.
    #[must_use]
    pub fn pricing_for(&self, model_key: &str) -> ModelPricing {
        self.pricing
            .get(model_key)
            .cloned()
            .unwrap_or_else(|| ModelPricing::free(model_key))
    }

    /// The cost of one turn's `usage` under `model_key`'s pricing, as the
    /// canonical [`CostRecord`] the node envelope carries.
    ///
    /// `CostRecord::input_tokens` is the *total* prompt (uncached + cache read +
    /// write); `output_tokens` is the completion; `micro_usd` is the summed spend
    /// across uncached input, output, cache reads (discounted) and cache writes
    /// (premium).
    #[must_use]
    pub fn cost_for(&self, model_key: &str, usage: &Usage) -> CostRecord {
        let p = self.pricing_for(model_key);
        let input_cost = cost_of(usage.input_tokens, p.input_micro_usd_per_mtok);
        let output_cost = cost_of(usage.output_tokens, p.output_micro_usd_per_mtok);
        let cache_read_cost = cost_of(
            usage.cache_read_tokens,
            apply_permille(p.input_micro_usd_per_mtok, p.cache_read_permille),
        );
        let cache_write_cost = cost_of(
            usage.cache_write_tokens,
            apply_permille(p.input_micro_usd_per_mtok, p.cache_write_permille),
        );
        let micro_usd = input_cost
            .saturating_add(output_cost)
            .saturating_add(cache_read_cost)
            .saturating_add(cache_write_cost);
        CostRecord {
            input_tokens: usage.total_input_tokens(),
            output_tokens: usage.output_tokens,
            micro_usd,
        }
    }

    /// The cache **saving** for `usage` under `model_key`: what the cache-read
    /// tokens would have cost at full input price minus what they actually cost
    /// at the discounted cache-read rate (DESIGN §12.5 — the figure the per-branch
    /// ledger breaks out). Zero when nothing was cache-read or reads aren't
    /// discounted.
    #[must_use]
    pub fn cache_savings_micro_usd(&self, model_key: &str, usage: &Usage) -> u64 {
        let p = self.pricing_for(model_key);
        let full = cost_of(usage.cache_read_tokens, p.input_micro_usd_per_mtok);
        let discounted = cost_of(
            usage.cache_read_tokens,
            apply_permille(p.input_micro_usd_per_mtok, p.cache_read_permille),
        );
        full.saturating_sub(discounted)
    }

    /// Sum a set of per-node cost records into one branch/aggregate total
    /// (DESIGN §12.5 — the per-branch ledger).
    #[must_use]
    pub fn aggregate(records: &[CostRecord]) -> CostRecord {
        records.iter().fold(
            CostRecord {
                input_tokens: 0,
                output_tokens: 0,
                micro_usd: 0,
            },
            |acc, r| CostRecord {
                input_tokens: acc.input_tokens.saturating_add(r.input_tokens),
                output_tokens: acc.output_tokens.saturating_add(r.output_tokens),
                micro_usd: acc.micro_usd.saturating_add(r.micro_usd),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_a_plain_turn() {
        let acc = CostAccountant::with_builtin_pricing();
        // 1,000 in @ $3/Mtok = 3,000 micro; 500 out @ $15/Mtok = 7,500 micro.
        let cost = acc.cost_for("claude-3-5-sonnet", &Usage::new(1_000, 500));
        assert_eq!(cost.input_tokens, 1_000);
        assert_eq!(cost.output_tokens, 500);
        assert_eq!(cost.micro_usd, 3_000 + 7_500);
    }

    #[test]
    fn cache_read_is_cheap_and_write_is_premium() {
        let acc = CostAccountant::with_builtin_pricing();
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,  // 1 Mtok read
            cache_write_tokens: 1_000_000, // 1 Mtok write
        };
        let cost = acc.cost_for("claude-3-5-sonnet", &usage);
        // read = $3 × 0.1 = $0.30 = 300_000 micro; write = $3 × 1.25 = 3_750_000.
        assert_eq!(cost.micro_usd, 300_000 + 3_750_000);
        // total prompt tokens counted.
        assert_eq!(cost.input_tokens, 2_000_000);
    }

    #[test]
    fn cache_savings_is_full_minus_discounted() {
        let acc = CostAccountant::with_builtin_pricing();
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0,
        };
        // full = $3 = 3_000_000; discounted = $0.30 = 300_000; saving = 2_700_000.
        assert_eq!(
            acc.cache_savings_micro_usd("claude-3-5-sonnet", &usage),
            3_000_000 - 300_000
        );
    }

    #[test]
    fn local_and_unknown_models_are_free() {
        let acc = CostAccountant::with_builtin_pricing();
        let local = acc.cost_for("llama3.1", &Usage::new(10_000, 5_000));
        assert_eq!(local.micro_usd, 0);
        let unknown = acc.cost_for("some/new-model", &Usage::new(10_000, 5_000));
        assert_eq!(unknown.micro_usd, 0);
        assert_eq!(unknown.input_tokens, 10_000); // tokens still counted
    }

    #[test]
    fn aggregate_sums_a_branch() {
        let acc = CostAccountant::with_builtin_pricing();
        let a = acc.cost_for("claude-3-5-sonnet", &Usage::new(1_000, 500));
        let b = acc.cost_for("gpt-4o", &Usage::new(2_000, 1_000));
        let total = CostAccountant::aggregate(&[a.clone(), b.clone()]);
        assert_eq!(total.input_tokens, 3_000);
        assert_eq!(total.output_tokens, 1_500);
        assert_eq!(total.micro_usd, a.micro_usd + b.micro_usd);
    }

    #[test]
    fn cost_record_is_integer_and_canon_safe() {
        // The produced record is all-integer (micro-USD), so it serializes
        // through the canonical encoder that forbids floats.
        let acc = CostAccountant::with_builtin_pricing();
        let cost = acc.cost_for("gpt-4o", &Usage::new(123_456, 7_890));
        let json = serde_json::to_string(&cost).unwrap();
        assert!(!json.contains('.'), "no floats in a CostRecord: {json}");
    }
}
