# Spotlight #06: Envelope cap rejection

## What it demonstrates

This spotlight exercises **manifest-level budget cap enforcement** with clean rejection semantics in a depth-2 dispatch.

**Scenario:** An operator sets `max_sublead_budget_usd = 3.0` as a safety rail on the manifest. A root lead attempts to spawn a sub-lead with `budget_usd = 5.0` (either by bad design or runaway generation). The cap enforcement rejects the spawn cleanly:

- No sub-tree `LayerState` is registered
- No budget reservation is made
- Root gets a clear MCP error message naming the cap
- Root can retry with a compliant budget and succeed

The spotlight proves:

1. **Cap rejection is pre-state**: When the envelope budget exceeds `max_sublead_budget_usd`, the `spawn_sublead` MCP call returns an error **before any state mutation happens** (no half-spawned sub-lead left behind).

2. **Error message is actionable**: The error message contains "exceeds per-sublead cap" or similar, allowing the operator or Claude model to understand the constraint and retry with a smaller budget.

3. **Clean state after rejection**: After a rejected spawn attempt:
   - `state.subleads.read().await.is_empty()` (no partial registration)
   - `*state.root.reserved_usd.lock().await == 0.0` (no phantom reservation)

4. **Successful retry with compliant budget**: When the same root lead retries with `budget_usd = 2.0` (within the 3.0 cap):
   - The spawn succeeds
   - Sub-lead IS registered
   - Reserved budget reflects the $2 envelope

## How to run

This spotlight is verified via an **in-process integration test**:

```
cargo test --test dogfood_fake_flows dogfood_envelope_cap_rejection
```

The test:
1. Constructs a `DispatchState` with `max_sublead_budget_usd = 3.0` baked into the manifest
2. Attempts to spawn a sub-lead with `budget_usd = 5.0` → expects MCP error
3. Verifies no partial state was registered and no budget reserved
4. Retries with `budget_usd = 2.0` → expects success
5. Verifies the sub-lead IS registered and reserved budget is $2

## Why cap enforcement matters

Budget caps are critical safety rails at scale:
- **Runaway generation protection**: A misbehaving LLM or prompt could spawn sub-leads with excessive budgets, exhausting the operator's total budget
- **Deterministic rejection**: Cap enforcement is declarative and deterministic, not dependent on approval policies or operator response
- **Pre-state rejection**: Failing before state mutation prevents half-spawned sub-leads and phantom reservations, keeping the dispatch state clean

## Files

| File | Purpose |
|---|---|
| `manifest.toml` | Reference manifest (documentation only — tests use in-code config) |
| `README.md` | This file |
| `expected-observables.md` | Plain-English description of expected behavior |
| `run.sh` | Prints instructions pointing to the cargo test invocation |
