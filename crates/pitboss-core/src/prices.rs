//! USD price table for AI model providers. Update as pricing changes.
//!
//! Rates are per 1M tokens. Unknown providers or model families return `None`
//! so callers can render an unknown-cost marker instead of inventing a value.

use crate::parser::TokenUsage;
use crate::provider::Provider;

struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

fn anthropic_rates_for(model: &str) -> Option<Rates> {
    let lc = model.to_ascii_lowercase();
    if lc.contains("opus") {
        Some(Rates {
            input: 15.0,
            output: 75.0,
            cache_read: 1.50,
            cache_write: 18.75,
        })
    } else if lc.contains("sonnet") {
        Some(Rates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write: 3.75,
        })
    } else if lc.contains("haiku") {
        Some(Rates {
            input: 0.80,
            output: 4.0,
            cache_read: 0.08,
            cache_write: 1.00,
        })
    } else {
        None
    }
}

fn openai_rates_for(model: &str) -> Option<Rates> {
    let lc = model.to_ascii_lowercase();
    if lc.starts_with("o1-mini") || lc.starts_with("o3-mini") {
        return Some(Rates {
            input: 1.10,
            output: 4.40,
            cache_read: 0.55,
            cache_write: 1.10,
        });
    }
    if lc.starts_with("o1") {
        return Some(Rates {
            input: 15.0,
            output: 60.0,
            cache_read: 7.50,
            cache_write: 15.0,
        });
    }
    if lc.starts_with("o3") {
        return Some(Rates {
            input: 2.0,
            output: 8.0,
            cache_read: 0.50,
            cache_write: 2.0,
        });
    }
    if lc.starts_with("o4-mini") || lc.starts_with("o4") {
        return Some(Rates {
            input: 1.10,
            output: 4.40,
            cache_read: 0.275,
            cache_write: 1.10,
        });
    }
    if lc.starts_with("gpt-4o-mini") {
        return Some(Rates {
            input: 0.15,
            output: 0.60,
            cache_read: 0.075,
            cache_write: 0.15,
        });
    }
    if lc.starts_with("gpt-4o") {
        return Some(Rates {
            input: 2.50,
            output: 10.0,
            cache_read: 1.25,
            cache_write: 2.50,
        });
    }
    if lc.starts_with("gpt-5") {
        return Some(Rates {
            input: 5.0,
            output: 15.0,
            cache_read: 0.625,
            cache_write: 5.0,
        });
    }
    if lc.starts_with("gpt-4") {
        return Some(Rates {
            input: 30.0,
            output: 60.0,
            cache_read: 15.0,
            cache_write: 30.0,
        });
    }
    None
}

fn rates_for(provider: &Provider, model: &str) -> Option<Rates> {
    match provider {
        Provider::Anthropic => anthropic_rates_for(model),
        Provider::OpenAi => openai_rates_for(model),
        Provider::Ollama => Some(Rates {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        }),
        Provider::Google
        | Provider::OpenRouter
        | Provider::Azure
        | Provider::Bedrock
        | Provider::Other(_) => None,
    }
}

#[allow(clippy::cast_precision_loss)]
fn compute_cost(rates: &Rates, usage: &TokenUsage) -> f64 {
    let f = |n: u64, rate_per_million: f64| (n as f64) * rate_per_million / 1_000_000.0;
    let reasoning = usage.reasoning.unwrap_or(0);
    f(usage.input, rates.input)
        + f(usage.output, rates.output)
        + f(usage.cache_read, rates.cache_read)
        + f(usage.cache_creation, rates.cache_write)
        + f(reasoning, rates.output)
}

/// Provider-keyed cost estimate for a single tile's token usage.
#[must_use]
pub fn cost_usd_v2(provider: &Provider, model: &str, usage: &TokenUsage) -> Option<f64> {
    let rates = rates_for(provider, model)?;
    Some(compute_cost(&rates, usage))
}

/// Returns estimated USD cost for a single Anthropic tile's usage.
///
/// Kept as a compatibility wrapper while call sites are migrated to
/// [`cost_usd_v2`].
#[must_use]
pub fn cost_usd(model: &str, usage: &TokenUsage) -> Option<f64> {
    cost_usd_v2(&Provider::Anthropic, model, usage)
}

/// Format a dollar amount as `"$0.02"` (always two decimals). Returns "—" for None.
#[must_use]
pub fn fmt_cost(cents_opt: Option<f64>) -> String {
    match cents_opt {
        Some(v) => format!("${v:.2}"),
        None => "\u{2014}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(i: u64, o: u64) -> TokenUsage {
        TokenUsage {
            input: i,
            output: o,
            cache_read: 0,
            cache_creation: 0,
            reasoning: None,
        }
    }

    fn usage_with_reasoning(i: u64, o: u64, r: u64) -> TokenUsage {
        TokenUsage {
            input: i,
            output: o,
            cache_read: 0,
            cache_creation: 0,
            reasoning: Some(r),
        }
    }

    #[test]
    fn haiku_cost_known() {
        let c = cost_usd("claude-haiku-4-5", &usage(1_000_000, 1_000_000)).unwrap();
        assert!((c - 4.80).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn sonnet_cost_known() {
        let c = cost_usd("claude-sonnet-4-6", &usage(1_000_000, 1_000_000)).unwrap();
        assert!((c - 18.0).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn opus_cost_known() {
        let c = cost_usd("claude-opus-4-7", &usage(1_000_000, 1_000_000)).unwrap();
        assert!((c - 90.0).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn unknown_model_returns_none() {
        assert!(cost_usd("gpt-4", &usage(100, 200)).is_none());
        assert!(cost_usd("llama-3", &usage(100, 200)).is_none());
    }

    #[test]
    fn dated_model_suffix_normalizes() {
        assert!(cost_usd("claude-haiku-4-5-20251001", &usage(100, 200)).is_some());
    }

    #[test]
    fn any_family_revision_matches_same_rates() {
        let c1 = cost_usd("claude-opus-4-7", &usage(1_000_000, 0)).unwrap();
        let c2 = cost_usd("claude-opus-4-5", &usage(1_000_000, 0)).unwrap();
        let c3 = cost_usd("claude-opus-4-9", &usage(1_000_000, 0)).unwrap();
        assert!((c1 - c2).abs() < 1e-9 && (c1 - c3).abs() < 1e-9);

        let s1 = cost_usd("claude-sonnet-4-6", &usage(1_000_000, 0)).unwrap();
        let s2 = cost_usd("claude-sonnet-4-4", &usage(1_000_000, 0)).unwrap();
        assert!((s1 - s2).abs() < 1e-9);
    }

    #[test]
    fn case_insensitive_family_match() {
        assert!(cost_usd("CLAUDE-OPUS-4-7", &usage(100, 200)).is_some());
        assert!(cost_usd("Haiku-Beta", &usage(100, 200)).is_some());
    }

    #[test]
    fn small_usage_is_small_cost() {
        let c = cost_usd("claude-haiku-4-5", &usage(18, 504)).unwrap();
        assert!((c - 0.00203).abs() < 1e-4, "got {c}");
    }

    #[test]
    fn gpt4o_cost_known() {
        let c = cost_usd_v2(&Provider::OpenAi, "gpt-4o", &usage(1_000_000, 1_000_000)).unwrap();
        assert!((c - 12.50).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn gpt4o_mini_distinct_from_gpt4o() {
        let c = cost_usd_v2(
            &Provider::OpenAi,
            "gpt-4o-mini",
            &usage(1_000_000, 1_000_000),
        )
        .unwrap();
        assert!((c - 0.75).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn reasoning_tokens_bill_at_output_rate() {
        let u = usage_with_reasoning(1_000_000, 1_000_000, 1_000_000);
        let c = cost_usd_v2(&Provider::OpenAi, "o1", &u).unwrap();
        assert!((c - 135.0).abs() < 1e-6, "got {c}");
    }

    #[test]
    fn ollama_is_zero_cost() {
        let c = cost_usd_v2(&Provider::Ollama, "llama3.1", &usage(1_000_000, 1_000_000));
        assert_eq!(c, Some(0.0));
    }

    #[test]
    fn provider_key_matters() {
        let u = usage(100, 200);
        assert!(cost_usd_v2(&Provider::Anthropic, "gpt-4o", &u).is_none());
        assert!(cost_usd_v2(&Provider::OpenAi, "claude-sonnet-4-6", &u).is_none());
    }

    #[test]
    fn fmt_cost_formats_dollars_with_two_decimals() {
        assert_eq!(fmt_cost(Some(1.234)), "$1.23");
        assert_eq!(fmt_cost(Some(0.0)), "$0.00");
        assert_eq!(fmt_cost(None), "\u{2014}");
    }
}
