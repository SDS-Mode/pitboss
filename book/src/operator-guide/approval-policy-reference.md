# Approval policy reference

## Overview

The `[[approval_policy]]` TOML block defines deterministic approval rules that auto-resolve requests before they reach the operator. Rules are pure Rust evaluation, NOT LLM-evaluated.

Each rule matches against fixed approval fields (actor path, category, tool name, cost estimate) and applies an action (`auto_approve`, `auto_reject`, or `block`). Rules are evaluated in declaration order; the first match wins. If no rule matches, the approval falls through to the run-level `[run].approval_policy` (or the operator queue if no default is set).

Why it exists: At depth-2 scale where N concurrent sub-leads can spawn M approval requests each, the operator queue drowns in noise. Policy rules handle routine approvals deterministically, surfacing only exceptional cases.

---

## TOML syntax

```toml
[[approval_policy]]
match = { actor = "root→S1", category = "tool_use", tool_name = "Bash", cost_over = 0.50 }
action = "auto_approve"

[[approval_policy]]
match = { category = "plan" }
action = "block"
```

- `[[approval_policy]]` — each rule is a separate array-of-tables block.
- `match` — TOML inline table of conditions. All fields are optional; an empty `match = {}` matches every approval (catch-all). Multiple match fields use AND semantics.
- `action` — one of `"auto_approve"`, `"auto_reject"`, or `"block"`.

---

## Match fields reference

| Field | Type | Matches when | Notes |
|-------|------|--------------|-------|
| `actor` | string (optional) | The approval's `actor_path` rendered as a string (e.g., `"root"` or `"root→S1"`) equals the value | Use `"root"` for root-level requests. Use `"root→<sublead_id>"` for a specific sub-lead; sub-lead IDs are UUIDv7 and runtime-generated, so this field is most useful when you know the sub-lead's identity in advance — e.g., from a prior `spawn_sublead` response. Exact string match only; no wildcard patterns. |
| `category` | enum (optional) | The approval's category field equals the value exactly | Allowed values: `"tool_use"`, `"plan"`, `"cost"`, `"other"`. Most `request_approval` calls land in `tool_use`; `propose_plan` lands in `plan`. Cost-category approvals are not emitted by default (see deferment notes). |
| `tool_name` | string (optional) | The lead's optional `tool_name` hint on `request_approval` equals the value | Only fires if the lead populates the optional `tool_name` arg. Without it, this field never matches. Exact string match only. |
| `cost_over` | float (optional) | The lead's optional `cost_estimate` hint exceeds this threshold (strict `>` comparison) | Only fires if the lead passes a `cost_estimate` arg to `request_approval`. Without it, this field never matches. Numeric greater-than comparison. |

---

## Action values

| Action | Effect |
|--------|--------|
| `"auto_approve"` | Approval is resolved as approved without operator interaction. The requesting actor's MCP call returns `approved=true` immediately. Logged at `info` level for audit trail. |
| `"auto_reject"` | Approval is resolved as rejected without operator interaction. The requesting actor receives `approved=false`. The response includes reason text `"auto-rejected by policy"`. |
| `"block"` | Forces the approval to enqueue for operator action regardless of its match. Overrides any run-level `[run].approval_policy` default that would be permissive. Useful for "always require explicit approval for X" rules. |

---

## Evaluation order and semantics

- Rules are evaluated in declaration order from top to bottom in the manifest.
- The first rule whose `match` clause fully matches (all specified fields match) wins. Its `action` is applied immediately.
- If no rule matches any approval field, the approval falls through to the run-level `[run].approval_policy` value.
- If no run-level policy is set and no rule matches, the approval is queued for the operator (the default v0.5 behavior).
- `block` action does NOT short-circuit further rule evaluation for other approvals. It only guarantees the current approval reaches the operator queue.

---

## Worked examples

### Pattern A: Auto-approve routine reads, surface mutations

```toml
[[approval_policy]]
match = { tool_name = "Read" }
action = "auto_approve"

[[approval_policy]]
match = { tool_name = "Glob" }
action = "auto_approve"

[[approval_policy]]
match = { tool_name = "Grep" }
action = "auto_approve"

# Everything else (Bash, Write, Edit, etc.) falls through to operator
```

**How it works:** Leads must populate `tool_name` on their `request_approval` calls for this to work. File reads are auto-approved, reducing noise. Any other tool (including `Write`, `Edit`, `Bash`, or custom MCP tools) reaches the operator.

**Note:** Requires the lead's prompt to pass `tool_name` on every `request_approval` call. Without it, these rules won't fire.

---

### Pattern B: Block all plan approvals, auto-approve trusted sub-lead's tool use

```toml
[[approval_policy]]
match = { category = "plan" }
action = "block"

[[approval_policy]]
match = { actor = "root→sublead-trusted-id", category = "tool_use" }
action = "auto_approve"
```

**How it works:** Order matters. The `category = "plan"` rule fires first, blocking all `propose_plan` calls for explicit operator review. The second rule auto-approves tool-use from a specific trusted sub-lead (once you know its UUIDv7). Tool-use from other actors falls through to the operator.

**When to use:** Multi-phase runs where you want to gate each phase's plan proposal (to catch logic errors early) but trust a specific sub-lead's tool invocations.

---

### Pattern C: Cost-bounded auto-approval

```toml
[[approval_policy]]
match = { category = "cost", cost_over = 1.00 }
action = "block"

[[approval_policy]]
match = { category = "cost" }
action = "auto_approve"
```

**How it works:** Cost-category approvals over $1.00 always reach the operator. Smaller ones auto-approve. This is a firewall against unexpectedly expensive operations.

**Note:** This pattern is forward-looking. As of v0.6, leads do not emit cost-category approvals by default. The lead must explicitly call `request_approval` with `category = "cost"` for these rules to fire. See deferment notes below.

---

## Deferment notes

The following features are not in v0.6 but are requested or planned:

### Runtime policy mutation

TUI commands to add/remove rules mid-run are deferred to v0.7+. v0.6 reads the policy once at manifest load. You cannot change rules while a run is in flight.

### No regex or glob patterns in match values

Match fields support exact string comparison only. Wildcard patterns like `tool_name = "Read*"` or `actor = "root→sublead-*"` are not supported. Matching is literal. If you have a use case that needs wildcard matching (e.g., "auto-approve all Read variants"), please file an issue.

### Cost-category approvals are not emitted by default

The `request_approval` MCP tool defaults to `category = "tool_use"`. For cost-category rules to fire in practice, the lead must explicitly pass `category = "cost"` when calling `request_approval`. Most leads don't do this yet, so cost-category rules have limited utility in v0.6.

### Rule-index attribution in logs

Auto-action logs say "auto-approved by policy" or "auto-rejected by policy" but do not (currently) name which rule fired. Adding rule indices to audit logs is a small followup that would help track which policy pattern is in effect.

---

## See also

- [Approvals](./approvals.md) — operator-side workflow (TUI pane, reject-with-reason, TTL fallback)
- [Defense-in-depth → Approval-gated state-changing tools](../security/defense-in-depth.md#approval-gated-state-changing-tools) — how to use policy as a security control pattern
- [MCP Tool Reference → Approvals](../mcp-reference/approvals.md) — the underlying `request_approval` and `propose_plan` MCP tools
