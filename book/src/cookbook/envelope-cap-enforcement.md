# Spotlight #06: Envelope cap enforcement

**Source:** [`examples/dogfood/fake/06-envelope-cap-rejection/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/fake/06-envelope-cap-rejection)

## What it demonstrates

This spotlight exercises **manifest-level budget cap enforcement** with clean rejection semantics in a depth-2 dispatch.

**Scenario:** An operator sets `max_sublead_budget_usd = 3.0` as a safety rail. A root lead attempts to spawn a sub-lead with `budget_usd = 5.0` (either by bad design or runaway generation). The cap enforcement rejects the spawn cleanly — no partial state, no phantom reservation.

The spotlight proves four things:

1. **Cap rejection is pre-state**: When the envelope budget exceeds `max_sublead_budget_usd`, `spawn_sublead` returns an error **before any state mutation**. No half-spawned sub-lead is registered.

2. **Error message is actionable**: The error names the cap ("exceeds per-sublead cap"), allowing Claude to understand the constraint and retry with a smaller budget.

3. **Clean state after rejection**: After a rejected spawn:
   - `state.subleads` is empty (no partial registration)
   - `reserved_usd == 0` (no phantom reservation)

4. **Successful retry with compliant budget**: Retrying with `budget_usd = 2.0` (within the 3.0 cap) succeeds; the sub-lead is registered and `reserved_usd = 2.0`.

## The manifest cap

```toml
[[lead]]
allow_subleads = true
max_sublead_budget_usd = 3.0
```

## How to run

```bash
cargo test --test dogfood_fake_flows dogfood_envelope_cap_rejection
```

The test:
1. Constructs a `DispatchState` with `max_sublead_budget_usd = 3.0`
2. Attempts `spawn_sublead(budget_usd=5.0)` → expects MCP error
3. Verifies no partial state and no budget reserved
4. Retries with `spawn_sublead(budget_usd=2.0)` → expects success

The [`run.sh`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/06-envelope-cap-rejection/run.sh) script prints instructions pointing to this cargo invocation.

## Why pre-state rejection matters

Failing before any state mutation keeps the dispatch state clean and predictable. A half-spawned sub-lead with a phantom budget reservation would be difficult to diagnose and could cause downstream spawn failures or incorrect budget accounting.

## Key assertions

See [`expected-observables.md`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/06-envelope-cap-rejection/expected-observables.md) for the full expected behavior.

## Related concepts

- [Depth-2 sub-leads](../operator-guide/depth-2-subleads.md) — `max_sublead_budget_usd` and other manifest caps
- [Manifest schema](../operator-guide/manifest-schema.md) — `[[lead]]` field reference
- [Session control](../mcp-reference/session-control.md) — `spawn_sublead` error conditions
