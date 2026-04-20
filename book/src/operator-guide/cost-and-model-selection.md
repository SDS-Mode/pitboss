# Cost & model selection

`budget_usd` is a hard guardrail, but it only protects you after the fact. Good operators set it based on a prior estimate of what the run should cost — and pick models that match the reasoning demands of each role. This page covers how to calibrate both.

> **Pricing note:** The figures in this page are approximate snapshots from early 2026. Anthropic's pricing changes over time. Before committing to a budget for a production run, verify current rates at [https://www.anthropic.com/pricing](https://www.anthropic.com/pricing).

---

## The two model pricing tiers (as of 2026)

| Model | Input ($/MTok) | Output ($/MTok) | Best for |
|---|---|---|---|
| claude-haiku-4-5 | ~$1 | ~$5 | High-volume simple decisions, leads orchestrating small jobs, cheap first-pass triage |
| claude-sonnet-4-6 | ~$3 | ~$15 | Most general-purpose reasoning and code work; default for workers that need to think |
| claude-opus-4-7 | ~$15 | ~$75 | Deep reasoning on genuinely hard problems; rarely necessary for routine code or text |

These are approximate figures. Verify at [https://www.anthropic.com/pricing](https://www.anthropic.com/pricing) before calibrating production budgets.

---

## Calibrating budget_usd from typical worker costs

Worker cost is driven by token volume — how many tokens the worker reads (input) and generates (output). The table below gives rough orders of magnitude for common task types. Measure your own workloads; these are starting points, not guarantees.

| Task type | Typical token usage | Approx cost — Haiku 4.5 | Approx cost — Sonnet 4.6 |
|---|---|---|---|
| Small code review (one file, <500 LOC) | 5K–15K tokens | $0.02–$0.08 | $0.06–$0.25 |
| Medium audit (a few files + structured report) | 15K–50K tokens | $0.08–$0.30 | $0.25–$1.00 |
| Large refactor (many files, multi-turn) | 50K–200K tokens | $0.30–$1.50 | $1–$5 |
| Continuous test loop (with reprompts) | 100K–500K tokens | $0.50–$3 | $2–$10 |

### Reservation overhead

When you call `spawn_worker`, pitboss reserves the worker's `estimated_cost_usd` against the run's budget *before* the worker starts. On completion, the reservation is released and the actual spend is recorded. This prevents budget overruns but has a practical consequence: if your estimate is too low, `spawn_worker` fails ("budget exceeded") before the worker has done anything. If too high, other workers can't spawn until the first one finishes and releases its reservation.

**Practical defaults:**

- Set `estimated_cost_usd` per worker at **1.5× your typical actual cost** for that task type. The 50% margin accounts for variance in input size and response verbosity.
- Set `budget_usd` at **1.2× the sum of all expected worker estimates**. The 20% margin covers the lead's own token use and any workers that run over their estimate.

Example for a run with 5 medium-audit workers (Sonnet 4.6, expected ~$0.50 each):

```
estimated_cost_usd per worker = $0.50 × 1.5 = $0.75
Sum of estimates = 5 × $0.75 = $3.75
budget_usd = $3.75 × 1.2 = $4.50
```

Cross-reference: [Manifest schema → budget_usd](./manifest-schema.md) for the full reservation accounting semantics.

---

## The "Haiku-as-lead, Sonnet-as-worker" pattern

The most common cost-effective pattern for hierarchical runs:

- The lead's job is **tool dispatch and simple decisions** — which worker to spawn next, when to stop, how to aggregate results. This is not demanding reasoning. Haiku handles it well at roughly one-fifth the input cost of Sonnet.
- Workers do the **actual reasoning** — reading code, writing reports, producing patches. Sonnet's stronger tool use and reasoning quality is worth the cost premium at the task level.

A typical depth-1 run with 5 workers under this pattern:
- Lead (Haiku, ~$0.05 total): reads the task list, spawns workers, waits, summarizes
- 5 workers (Sonnet, ~$0.20 each = ~$1.00 total)
- Total: ~$1.05

The alternative — Sonnet-as-lead with Sonnet-as-worker — costs the same or more, but wastes Sonnet's reasoning capacity on loop bookkeeping the lead doesn't need.

### When *not* to use this pattern

Use Sonnet (or higher) for the lead when:

- **The lead makes complex strategic decisions.** If the lead needs to interpret cross-worker results and decide whether to change approach mid-run, Haiku's reasoning limitations become a bottleneck.
- **The lead is synthesizing across many outputs into a unified artifact.** Writing a coherent 10-section report that synthesizes 20 worker results is reasoning-heavy work. Use Sonnet.
- **The plan requires backtracking.** Multi-step plans where the lead must detect failures and re-plan benefit from Sonnet's stronger context handling.

Haiku-as-lead works best when the lead's decision tree is shallow: "for each item, spawn a worker; wait for all; write a summary." Anything more complex, consider Sonnet.

---

## Sub-leads and cost compounding

With depth-2 sub-leads (v0.6+), costs multiply across tiers:

```
Root lead (1 session)
├── Sub-lead A (1 session)
│   ├── Worker A1 (1 session)
│   ├── Worker A2 (1 session)
│   └── Worker A3 (1 session)
├── Sub-lead B (1 session)
│   └── ...
└── Sub-lead C (1 session)
    └── ...
```

A run with 1 root + 3 sub-leads + 4 workers each = 16 sessions. At Haiku-only pricing for all sessions, even 50K token/session average is 800K tokens, which costs ~$0.80–$4 depending on input/output split. At Sonnet pricing the same run is $3–$16.

Sub-lead cost control mechanisms in the manifest:

```toml
[run]
budget_usd = 20.00

[[lead]]
id                     = "root"
model                  = "claude-haiku-4-5"
allow_subleads         = true
max_subleads           = 4
max_sublead_budget_usd = 3.00   # hard ceiling per sub-lead
max_workers_across_tree = 12    # bounds peak concurrency (and peak spend rate)

[lead.sublead_defaults]
budget_usd        = 2.00        # default envelope per sub-lead
max_workers       = 3
lead_timeout_secs = 1800
```

`sublead_defaults` means you don't have to specify budget on every `spawn_sublead` call. `max_sublead_budget_usd` is a hard ceiling enforced by pitboss — the root lead cannot accidentally grant more even if its prompt instructs it to.

See [Depth-2 sub-leads](./depth-2-subleads.md) for the full sub-lead envelope model.

---

## Reservation overhead in practice

Understanding the reservation lifecycle helps diagnose "budget exceeded" failures that appear before workers have done any work.

**The lifecycle:**

1. `spawn_worker` is called with `estimated_cost_usd = X`
2. Pitboss checks: `spent + reserved + X ≤ budget_usd`. If not, spawn fails.
3. If the check passes, X is added to `reserved_usd` and the worker starts.
4. On completion, X is released from `reserved_usd` and actual spend is added to `spent_usd`.

**Common pitfalls:**

- **Over-reserving ("to be safe"):** Setting `estimated_cost_usd` high means each worker blocks a large slice of the budget. With 5 workers each estimated at $2 and a $6 budget, only 3 workers can be in-flight at once — the fourth and fifth spawn calls fail until a slot releases. The run serializes instead of parallelizing.
- **Under-reserving:** If a worker's actual spend exceeds its estimate, pitboss reconciles on completion. This won't crash the worker mid-task, but the `spent_usd` will exceed `budget_usd` on reconciliation. Pitboss records the overrun but does not retroactively kill the offending worker.
- **Reservation leak:** In rare cases (subprocess crash before reconciliation, pre-Phase-4 race conditions), a reservation stays held after the worker is gone. The reservation releases on pitboss restart. Use `pitboss attach` to monitor `reserved_usd` in real time and spot this pattern.

**Mitigations:**

- Derive estimates from observed history. Run 3–5 representative tasks manually, measure actual token usage, set `estimated_cost_usd` at 1.5× the observed mean.
- Monitor with `pitboss attach` during the first few runs of a new manifest to validate your estimate calibration.
- Set `budget_usd` with 20% headroom over the sum of expected estimates.

---

## See also

- [Manifest schema](./manifest-schema.md) — `budget_usd`, `estimated_cost_usd`, `max_sublead_budget_usd` field reference
- [Depth-2 sub-leads](./depth-2-subleads.md) — sub-lead envelope mechanics and `sublead_defaults`
- [Defense-in-depth → Cost firewall pattern](../security/defense-in-depth.md) — using per-sub-lead budget envelopes as a security control against runaway spend
