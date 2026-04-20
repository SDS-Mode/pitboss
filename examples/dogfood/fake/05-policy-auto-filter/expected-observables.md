# Expected Observables — Spotlight #05: Approval policy auto-filter

This document describes what an operator or reader should expect to observe
when the scenario in `dogfood_policy_auto_filter` runs.

## Setup

An operator configures a depth-2 dispatch with two policy rules:

- **Rule 1**: Auto-approve all `ToolUse` approvals from actor `root→S1`
- **Rule 2**: Block (surface to operator) all `Plan` approvals, regardless of actor
- **Implicit Rule 3**: All other requests fall through to operator queue (default)

Then the operator spawns two sub-leads:
- **S1** — "sub-lead 1 for routine work" (trusted)
- **S2** — "sub-lead 2 for untrusted work" (no special rules)

---

## Rule 1 Match: S1's ToolUse Auto-Approval

### S1 requests tool-use approval
- S1 calls `request_approval` with `category="tool_use"` (implicit)
- The policy matcher evaluates Rule 1: actor matches `root→S1`, category matches `ToolUse` ✓
- **Result: `approved=true`** with comment "auto-approved by policy"
- **No queue entry created** — S1's request is resolved immediately without operator intervention

### Observable in queue
- Operator's approval queue: **0 entries** after S1's auto-approval
- Operator receives **no alert** for S1's routine work

---

## Rule 1 No-Match: S2's ToolUse Falls Through

### S2 requests tool-use approval (same request, different actor)
- S2 calls `request_approval` with identical content
- The policy matcher evaluates Rule 1: actor is `root→S2`, Rule 1 requires `root→S1` ✗
- Rule 2 evaluated: category is `ToolUse`, Rule 2 only matches `Plan` ✗
- Implicit Rule 3: no explicit rule matched, fallback to operator queue
- **Result: request blocks**, waiting for operator response

### Observable in queue
- Operator's approval queue: **1 entry** (S2's tool-use)
- Operator receives an **alert** for S2's approval request

---

## Rule 2 Match: S1's Plan Always Requires Operator

### S1 requests plan approval (any actor)
- S1 calls `propose_plan` with plan fields (summary, rationale, resources, risks, rollback)
- The policy matcher evaluates Rule 1: category is `Plan`, Rule 1 only matches `ToolUse` ✗
- Rule 2 evaluated: category matches `Plan`, action is `Block` ✓
- **Result: request blocks**, `Block` action forces operator queue regardless of auto-approval rules
- **No auto-approval occurs** — plan approvals are operator decisions

### Observable in queue
- Operator's approval queue: **2 entries total** (S2 tool-use + S1 plan)
- Operator sees both S2's routine tool-use request and S1's plan proposal

---

## Final Queue State

| Entry | Requester | Category | Rule Match | Approved | Status |
|---|---|---|---|---|---|
| 1 | S1 | ToolUse | Rule 1 (actor + category) | Yes | **Not queued** (auto-approved) |
| 2 | S2 | ToolUse | Rule 3 (no match) | Pending | **Queued** (awaits operator) |
| 3 | S1 | Plan | Rule 2 (category) | Blocked | **Queued** (operator required) |

---

## Observable Sequence

1. **Operator configures policy** with Rule 1 and Rule 2
2. **Root spawns S1 and S2** sub-leads
3. **S1 requests tool-use** → **auto-approved** (Rule 1 match)
   - Queue size: 0
4. **S2 requests tool-use** → **queued** (no Rule 1 match for S2)
   - Queue size: 1
5. **S1 requests plan approval** → **queued** (Rule 2 blocks all Plan)
   - Queue size: 2
6. **Operator's queue contains**: S2's tool-use + S1's plan (not S1's auto-approved request)

---

## Summary of invariants demonstrated

| Scenario | Expected result |
|---|---|
| S1 tool-use (Rule 1: actor+category match) | Auto-approved, no queue entry |
| S2 tool-use (no Rule 1 match for S2) | Queued (no matching auto-action rule) |
| S1 plan (Rule 2: Block all Plan) | Queued (Block forces operator review) |
| Operator queue final size | 2 entries (S2 tool-use + S1 plan) |
| Operator alert frequency | Only 2 alerts (vs 3 requests total) |
