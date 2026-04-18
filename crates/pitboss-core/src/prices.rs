//! USD price table for claude models. Update as pricing changes.
//!
//! Source: anthropic.com/pricing. Rates are per 1M tokens.
//!
//! **This is operator-facing; keep the lookup robust to unknown model names
//! (returns $0.00 rather than panicking) and keep the table readable.**

use crate::parser::TokenUsage;

struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

/// Pricing as of 2026-04 for supported Claude Code models. Match on
/// family prefix (opus / sonnet / haiku) rather than exact revision
/// strings — same-family revisions share pricing, so a new "claude-opus-4-8"
/// dropping tomorrow should still cost-estimate correctly without a
/// code change. Unknown families return `None` — callers render "—".
///
/// If Anthropic ever splits pricing within a family, add a more specific
/// branch BEFORE the generic family match.
fn rates_for(model: &str) -> Option<Rates> {
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

/// Returns estimated USD cost for a single tile's usage. Returns `None` if
/// the model isn't in the price table (caller renders "—" or similar).
#[must_use]
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
    use crate::parser::TokenUsage;

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
        assert!(cost_usd("gpt-4", &usage(100, 200)).is_none());
        assert!(cost_usd("llama-3", &usage(100, 200)).is_none());
    }

    #[test]
    fn dated_model_suffix_normalizes() {
        // e.g., "claude-haiku-4-5-20251001" should match the base rate.
        assert!(cost_usd("claude-haiku-4-5-20251001", &usage(100, 200)).is_some());
    }

    #[test]
    fn any_family_revision_matches_same_rates() {
        // Older + hypothetical-newer revisions should resolve to family
        // rates without needing code updates per-release.
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
        // 18 * 0.80/1M + 504 * 4.0/1M = 0.0000144 + 0.002016 ≈ $0.0020
        assert!((c - 0.00203).abs() < 1e-4, "got {c}");
    }
}
