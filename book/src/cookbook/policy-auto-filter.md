# Spotlight #05: Approval policy auto-filter

**Source:** [`examples/dogfood/fake/05-policy-auto-filter/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/fake/05-policy-auto-filter)

## What it demonstrates

This spotlight exercises **`[[approval_policy]]` declarative auto-filtering** in a depth-2 dispatch.

**Scenario:** An operator configures a deterministic policy to auto-approve routine tool-use from a trusted sub-lead (S1) while blocking plan-category approvals — reducing approval noise at scale.

The spotlight proves four things:

1. **Rule-based auto-approval**: When S1 requests tool-use approval, Rule 1 (`actor = root→S1`, `category = ToolUse`) immediately approves without operator involvement.

2. **Actor-specific filtering**: S2 makes an identical tool-use request, but since Rule 1 only matches `root→S1`, S2's request falls through to the operator queue.

3. **Category-based blocking**: When S1 submits a plan approval, Rule 2 (`category = Plan`, `action = block`) forces operator review regardless of actor.

4. **Reduced operator noise**: Only non-matching requests land in the operator's queue. S1's routine tool-use never alerts the operator.

## The policy configuration

```toml
[[approval_policy]]
match = { actor = "root→S1", category = "ToolUse" }
action = "auto_approve"

[[approval_policy]]
match = { category = "Plan" }
action = "block"
```

First-match-wins. A request from S1 with `category = ToolUse` matches Rule 1 → auto-approved. A request from S1 with `category = Plan` does not match Rule 1 (wrong category) → falls to Rule 2 → blocked for operator.

## How to run

```bash
cargo test --test dogfood_fake_flows dogfood_policy_auto_filter
```

The test:
- S1's auto-approved request does **not** appear in the operator queue
- S2's request and S1's plan approval **do** appear in the queue
- Queue entries exist only for non-matching rules

The [`run.sh`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/05-policy-auto-filter/run.sh) script prints instructions pointing to this cargo invocation.

## Why deterministic rules (not LLM-evaluated)

Approval policies are Rust-evaluated — always deterministic, zero latency, auditable. This makes them suitable for high-volume auto-approvals (e.g., "trust S1's read-only operations") while LLM-based approval (via `propose_plan` or `request_approval` with a rich `plan`) handles judgment calls that genuinely need human review.

## Key assertions

See [`expected-observables.md`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/05-policy-auto-filter/expected-observables.md) for the full expected behavior.

## Related concepts

- [Approvals](../operator-guide/approvals.md) — `[[approval_policy]]` reference
- [MCP approvals](../mcp-reference/approvals.md) — `request_approval` / `propose_plan` tool reference
