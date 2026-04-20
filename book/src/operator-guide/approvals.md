# Approvals

Pitboss provides two approval primitives that let a lead gate actions on operator review:

- `request_approval` — gate a **single in-flight action** on operator approval.
- `propose_plan` — gate the **entire run** on a pre-flight plan approval.

Both route to the TUI's approval pane. Without a TUI attached, the `[run].approval_policy` field controls automatic behavior.

## `request_approval`

The lead calls `request_approval` when it wants the operator to review a specific action before proceeding. The lead blocks until the operator approves, rejects, or edits.

**Args:**
```json
{
  "summary": "string",
  "timeout_secs": 60,
  "plan": {
    "summary": "string",
    "rationale": "string",
    "resources": ["path/to/file.rs"],
    "risks": ["May overwrite uncommitted changes"],
    "rollback": "git checkout -- path/to/file.rs"
  }
}
```

The `plan` field is optional but strongly recommended for non-trivial actions (deletions, multi-file edits, irreversible operations). The TUI renders the structured fields as labeled sections, with `risks` highlighted in warning color.

**Returns:**
```json
{
  "approved": true,
  "comment": "optional operator comment",
  "edited_summary": "optional edited version"
}
```

## `propose_plan`

The lead calls `propose_plan` before spawning any workers when a pre-flight review is desired. When `[run].require_plan_approval = true`, `spawn_worker` refuses until a `propose_plan` call has been approved.

The TUI modal shows `[PRE-FLIGHT PLAN]` in its title (vs `[IN-FLIGHT ACTION]` for `request_approval`). On rejection, the gate stays closed — the lead can revise and call `propose_plan` again.

When `require_plan_approval = false` (the default), calling `propose_plan` is still valid but informational only — `spawn_worker` never checks the result.

## `[run].approval_policy`

Controls handling of approval requests when no TUI is attached:

| Value | Behavior |
|-------|----------|
| `"block"` (default) | Queue until a TUI connects, or until `lead_timeout_secs` expires. |
| `"auto_approve"` | Immediately approve. Useful for CI or unattended runs. |
| `"auto_reject"` | Immediately reject with `comment: "no operator available"`. |

## `[[approval_policy]]` — declarative policy rules (v0.6+)

For finer control, declare deterministic policy rules in the manifest. Rules are evaluated in pure Rust — not LLM-evaluated — before approvals reach the operator queue.

```toml
# Auto-approve routine tool-use from sub-lead S1
[[approval_policy]]
match = { actor = "root→S1", category = "ToolUse" }
action = "auto_approve"

# Always block plan-category approvals for explicit review
[[approval_policy]]
match = { category = "Plan" }
action = "block"

# Block any cost event over $0.50
[[approval_policy]]
match = { category = "Cost", cost_over = 0.50 }
action = "block"
```

Rules are evaluated first-match-wins in declaration order. A request that doesn't match any rule falls through to the run-level `approval_policy`.

### Match fields

| Field | Type | Notes |
|-------|------|-------|
| `actor` | string | Actor path, e.g., `"root→S1"`. Unset matches all actors. |
| `category` | string | `"ToolUse"`, `"Plan"`, `"Cost"`. Unset matches all categories. |
| `tool_name` | string | Specific MCP tool name. Unset matches all. |
| `cost_over` | float | Fires when `cost_estimate > cost_over` (USD). |

### Actions

| Action | Effect |
|--------|--------|
| `"auto_approve"` | Immediately approve; never reaches the operator queue. |
| `"auto_reject"` | Immediately reject. |
| `"block"` | Force operator review regardless of run-level `approval_policy`. |

## Approval TTL and fallback (v0.6+)

Each approval request can carry an optional TTL:

- `timeout_secs` — the lead can pass a timeout on its `request_approval` call.
- `fallback` — if the approval ages past its TTL without an operator response, the fallback fires (`auto_reject`, `auto_approve`, or `block`). Prevents an unreachable operator from permanently stalling the tree.

## TUI approval pane

In the TUI, a non-modal right-rail (30% width) shows pending approvals as a queue. Press `'a'` to focus the pane; Up/Down navigate; Enter opens the detail modal.

In the modal:
- `y` — approve
- `n` — reject (can add an optional reason comment)
- `e` — edit (Ctrl+Enter to submit, Esc to cancel)

The reject branch accepts an optional reason string that flows back through MCP to the requesting actor's session, allowing Claude to adapt without a separate `reprompt_worker` round-trip.

## Reject-with-reason

When an approval is rejected with a `reason`, the reason is included in the MCP response returned to the lead. This allows the lead to adapt its behavior immediately (e.g., switch output format, try a different approach) without requiring a separate `reprompt_worker` call.

## Using approvals as a security control

`[[approval_policy]]` can be used to gate state-changing tool invocations before they execute, independent of operator availability. See [Security → Defense-in-depth → Approval-gated state-changing tools](../security/defense-in-depth.md#approval-gated-state-changing-tools) for a manifest pattern that auto-approves reads and blocks writes for operator review.
