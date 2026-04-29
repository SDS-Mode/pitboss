// Cost estimation for Claude usage. Mirrors `pitboss_core::prices`
// (crates/pitboss-core/src/prices.rs) — kept small enough that the
// duplication is cheaper than wiring an API endpoint or shoving
// computed cost into TaskRecord.
//
// Per-1M-token rates as of 2026-04 from anthropic.com/pricing.
// Match on family substring so a future "claude-opus-4-8" picks up
// the right rate without a code change. Pricing splits within a
// family would need a more specific branch ahead of the family
// match — none exist today.
//
// If this drifts from the Rust table, fix both. The table here is the
// SPA's source of truth for the run-detail Cost card and per-row
// estimates; any backend that ships a pre-computed cost should also
// flow through here so display stays consistent.

export interface TokenUsage {
  input?: number;
  output?: number;
  cache_read?: number;
  cache_creation?: number;
}

interface Rates {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
}

function ratesFor(model: string): Rates | null {
  const lc = model.toLowerCase();
  if (lc.includes('opus')) {
    return { input: 15.0, output: 75.0, cache_read: 1.5, cache_write: 18.75 };
  }
  if (lc.includes('sonnet')) {
    return { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 };
  }
  if (lc.includes('haiku')) {
    return { input: 0.8, output: 4.0, cache_read: 0.08, cache_write: 1.0 };
  }
  return null;
}

/**
 * Estimated USD cost for a single task's token usage. Returns null when
 * the model isn't in the price table — caller renders "—".
 */
export function costUsd(model: string | null | undefined, usage?: TokenUsage | null): number | null {
  if (!model || !usage) return null;
  const r = ratesFor(model);
  if (!r) return null;
  const f = (n: number | undefined, ratePerMillion: number): number =>
    ((n ?? 0) * ratePerMillion) / 1_000_000;
  return (
    f(usage.input, r.input) +
    f(usage.output, r.output) +
    f(usage.cache_read, r.cache_read) +
    f(usage.cache_creation, r.cache_write)
  );
}

/** Format `123.45` as `"$123.45"`, sub-cent values as `"$0.0123"`, null as `"—"`. */
export function fmtCost(v: number | null | undefined): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '—';
  if (v < 0.01) return `$${v.toFixed(4)}`;
  return `$${v.toFixed(2)}`;
}
