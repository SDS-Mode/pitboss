# Expected Observables — Spotlight #06: Envelope cap rejection

This document describes what an operator or reader should expect to observe
when the scenario in `dogfood_envelope_cap_rejection` runs.

## Setup

An operator configures a depth-2 dispatch with:
- **Manifest `max_sublead_budget_usd = 3.0`**: Safety rail preventing any single sub-lead from exceeding $3

Then the operator spawns a root lead that will attempt to spawn sub-leads with
varying budgets.

---

## Act 1: Root attempts to spawn sub-lead with budget exceeding the cap

### Root requests spawn with budget_usd = 5.0
- Root lead calls `spawn_sublead` with `budget_usd = 5.0`
- Dispatch checks the envelope against `max_sublead_budget_usd = 3.0`
- **5.0 > 3.0** ✗ — cap violation detected

### Result: Clean rejection
- **MCP error returned** with message containing "exceeds per-sublead cap"
- **No LayerState registered** — `state.subleads` remains empty
- **No budget reservation made** — `state.root.reserved_usd` stays at 0.0
- **No partial state left behind** — dispatch remains clean for retry

### Observable in state
```
state.subleads.read().await.is_empty() == true   // no partial registration
*state.root.reserved_usd.lock().await == 0.0      // no phantom reservation
```

---

## Act 2: Root retries with compliant budget

### Root requests spawn with budget_usd = 2.0
- Root lead calls `spawn_sublead` with `budget_usd = 2.0`
- Dispatch checks the envelope against `max_sublead_budget_usd = 3.0`
- **2.0 < 3.0** ✓ — cap check passes

### Result: Clean success
- **MCP returns sublead_id** (e.g., `sublead-xxx`)
- **LayerState IS registered** — sub-lead appears in `state.subleads`
- **Budget IS reserved** — `state.root.reserved_usd` increases to 2.0
- **Sub-lead is ready** — root can interact with it via MCP

### Observable in state
```
state.subleads.read().await.contains_key(sublead_id) == true  // registered
*state.root.reserved_usd.lock().await == 2.0                  // envelope reserved
```

---

## Observable Sequence

1. **Operator configures manifest** with `max_sublead_budget_usd = 3.0`
2. **Root attempts spawn with $5.0 budget** → **rejected with error**
   - Queue size: 0 (no approval needed, pre-state check)
   - State: clean (no partial registration, no phantom reservation)
3. **Root retries spawn with $2.0 budget** → **succeeds**
   - Queue size: still 0 (no approvals in this scenario)
   - State: sub-lead registered, $2.0 reserved
4. **Operator confirms dispatch continues normally**

---

## Summary of invariants demonstrated

| Scenario | Expected result |
|---|---|
| Spawn with budget > cap (5.0 > 3.0) | Rejected, MCP error, no state mutation |
| State after rejected spawn | `subleads.is_empty()` && `reserved_usd == 0.0` |
| Retry with budget < cap (2.0 < 3.0) | Succeeds, sub-lead registered, $2.0 reserved |
| State after successful spawn | `subleads.contains(sublead_id)` && `reserved_usd == 2.0` |
| Error message clarity | Mentions "exceeds per-sublead cap" or "cap" |

---

## Why this test matters

**Cap enforcement is a critical safety boundary:**

1. **Pre-state rejection prevents cascading failures**: By failing before state mutation, cap enforcement ensures that a cap violation doesn't leave the dispatch in an inconsistent state (half-spawned sub-lead, phantom reservation).

2. **Deterministic and operator-friendly**: Unlike approval policies (which depend on operator response), cap enforcement is deterministic, immediate, and always prevents overspend.

3. **Protects against runaway generation**: At scale, a misbehaving LLM or prompt could spawn many sub-leads with excessive budgets. Caps act as a hard limit, protecting the operator's total budget and wallet.

4. **Final fake-claude spotlight**: This is the last spotlight before the e2e flow tests. It demonstrates a complete depth-2 feature (envelope cap enforcement) with clean semantics, readying the codebase for Task 6 (e2e test refreshes and integration polish).
