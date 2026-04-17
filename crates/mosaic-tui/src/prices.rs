//! USD price table for claude models. Update as pricing changes.
//!
//! Source: anthropic.com/pricing. Rates are per 1M tokens.
//!
//! **This is operator-facing; keep the lookup robust to unknown model names
//! (returns $0.00 rather than panicking) and keep the table readable.**

use mosaic_core::parser::TokenUsage;

struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

/// Pricing as of 2026-04 for supported Claude Code models. Unknown models
/// return `None` — callers should treat that as "cost unknown" in the UI.
fn rates_for(model: &str) -> Option<Rates> {
    // Normalize: trim any trailing "-20NN1001"-style revision suffix.
    // Model names have 4 segments (e.g. "claude-haiku-4-5"); take exactly 4.
    let base = model.split('-').take(4).collect::<Vec<_>>().join("-");
    match base.as_str() {
        "claude-opus-4-7" => Some(Rates {
            input: 15.0,
            output: 75.0,
            cache_read: 1.50,
            cache_write: 18.75,
        }),
        "claude-sonnet-4-6" => Some(Rates {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write: 3.75,
        }),
        "claude-haiku-4-5" => Some(Rates {
            input: 0.80,
            output: 4.0,
            cache_read: 0.08,
            cache_write: 1.00,
        }),
        _ => None,
    }
}

/// Returns estimated USD cost for a single tile's usage. Returns `None` if
/// the model isn't in the price table (caller renders "—" or similar).
pub fn cost_usd(model: &str, usage: &TokenUsage) -> Option<f64> {
    let r = rates_for(model)?;
    #[allow(clippy::cast_precision_loss)]
    let f = |n: u64, rate_per_million: f64| (n as f64) * rate_per_million / 1_000_000.0;
    Some(
        f(usage.input, r.input)
            + f(usage.output, r.output)
            + f(usage.cache_read, r.cache_read)
            + f(usage.cache_creation, r.cache_write),
    )
}

/// Format a dollar amount as `"$0.02"` (always two decimals). Returns "—" for None.
pub fn fmt_cost(cents_opt: Option<f64>) -> String {
    match cents_opt {
        Some(v) => format!("${v:.2}"),
        None => "\u{2014}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaic_core::parser::TokenUsage;

    fn usage(i: u64, o: u64) -> TokenUsage {
        TokenUsage {
            input: i,
            output: o,
            cache_read: 0,
            cache_creation: 0,
        }
    }

    #[test]
    fn haiku_cost_known() {
        // 1M input + 1M output on haiku = 0.80 + 4.00 = 4.80
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
        assert!(cost_usd("claude-unknown-x-y", &usage(100, 200)).is_none());
    }

    #[test]
    fn dated_model_suffix_normalizes() {
        // e.g., "claude-haiku-4-5-20251001" should match the base rate.
        assert!(cost_usd("claude-haiku-4-5-20251001", &usage(100, 200)).is_some());
    }

    #[test]
    fn small_usage_is_small_cost() {
        let c = cost_usd("claude-haiku-4-5", &usage(18, 504)).unwrap();
        // 18 * 0.80/1M + 504 * 4.0/1M = 0.0000144 + 0.002016 ≈ $0.0020
        assert!((c - 0.00203).abs() < 1e-4, "got {c}");
    }
}
