# Spotlight #05: Approval policy auto-filter

## What it demonstrates

This spotlight exercises **approval policy auto-filtering** in a depth-2 dispatch.

**Scenario:** An operator configures a deterministic policy to auto-approve
routine tool-use from a trusted sub-lead (S1) while blocking plan-category
approvals, reducing approval noise at scale.

The spotlight proves:

1. **Rule-based auto-approval**: When S1 requests tool-use approval, the policy
   matcher evaluates Rule 1 (actor = `root→S1`, category = `ToolUse`) and
   immediately approves the request **without operator involvement**.

2. **Actor-specific filtering**: S2 makes an identical tool-use request, but
   since the policy only matched S1's actor path, the request falls through to
   the operator's approval queue (Rule 1 does not match S2).

3. **Category-based blocking**: When S1 submits a plan for approval, Rule 2
   (category = `Plan`, action = `Block`) forces operator review, regardless of
   actor. Plan approvals are always queued — no auto-action.

4. **Reduced operator noise**: Only non-matching requests land in the operator's
   queue. S1's routine tool-use never alerts the operator; S1's plan and S2's
   tool-use require explicit operator review.

## How to run

This spotlight is verified via an **in-process integration test**:

```
cargo test --test dogfood_fake_flows dogfood_policy_auto_filter
```

The test constructs a `DispatchState`, configures the policy matcher after
spawning sub-leads, then verifies that:
- S1's auto-approved request does **not** enqueue
- S2's request and S1's plan approval **do** enqueue
- Queue entries exist only for non-matching rules

## Why policy (vs LLM-evaluated)

Approval policies are **declarative TOML rules**, not LLM-evaluated. They are:
- **Deterministic**: Same inputs always produce the same outcome
- **Fast**: No API call or model latency
- **Auditable**: Rules are declared upfront, not implicit in prompt behavior

This makes policies ideal for routine auto-approvals (e.g., "trust S1's
read-only operations") while LLM-based approvals (via proposal or traditional
means) handle judgment calls.

## Files

| File | Purpose |
|---|---|
| `manifest.toml` | Reference manifest (documentation only — tests use in-code config) |
| `README.md` | This file |
| `expected-observables.md` | Plain-English description of expected behavior |
| `run.sh` | Prints instructions pointing to the cargo test invocation |
