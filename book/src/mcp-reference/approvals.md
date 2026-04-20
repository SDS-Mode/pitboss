# Approval tools

These tools are available to the **lead** (and sub-leads). Workers cannot call approval tools.

For the operator-side view (TUI, `[[approval_policy]]` rules, `[run].approval_policy`), see [Approvals](../operator-guide/approvals.md).

---

## `request_approval`

Gate a single in-flight action on operator approval. The lead blocks until the operator approves, rejects, or edits.

**Args:**
```json
{
  "summary": "string (required)",
  "timeout_secs": 60,
  "plan": {
    "summary": "string (required)",
    "rationale": "string",
    "resources": ["list of files/APIs/PRs that will be touched"],
    "risks": ["known failure modes"],
    "rollback": "how to undo if something goes wrong"
  },
  "tool_name": "string (optional hint for policy matching)",
  "cost_estimate": 0.05
}
```

**Returns:**
```json
{
  "approved": true,
  "comment": "optional operator comment",
  "edited_summary": "optional operator-edited version of summary"
}
```

**Notes:**
- The `plan` field is optional for simple approvals but strongly recommended for non-trivial actions (deletions, multi-file edits, irreversible operations).
- `tool_name` and `cost_estimate` are hints that allow `[[approval_policy]]` rules to match on `tool_name` / `cost_over` criteria.
- Policy rules (if configured) are evaluated before the request reaches the operator queue. A matching `auto_approve` or `auto_reject` rule skips the operator entirely.

---

## `propose_plan`

Gate the entire run on operator pre-flight approval. Submit an execution plan before spawning any workers.

**Args:**
```json
{
  "plan": {
    "summary": "string (required)",
    "rationale": "string",
    "resources": ["files, services, PRs that will be touched"],
    "risks": ["known failure modes"],
    "rollback": "how to undo"
  },
  "timeout_secs": 120
}
```

**Returns:** Same shape as `request_approval`.

**Notes:**
- When `[run].require_plan_approval = true`, `spawn_worker` refuses until a `propose_plan` call has received `approved: true`.
- The TUI modal shows `[PRE-FLIGHT PLAN]` in the title (vs `[IN-FLIGHT ACTION]` for `request_approval`) so operators can tell them apart.
- On rejection, the gate stays closed — the lead can revise and call `propose_plan` again.
- When `require_plan_approval = false` (the default), calling `propose_plan` is informational only — `spawn_worker` never checks the result.

---

## `ApprovalPlan` schema

```json
{
  "summary": "required; appears in the modal title",
  "rationale": "optional; why this action should be taken",
  "resources": ["optional; files, APIs, PRs that will be touched"],
  "risks": ["optional; known failure modes — TUI highlights these in warning color"],
  "rollback": "optional; how to undo if something goes wrong"
}
```

Use the structured `plan` form for any non-trivial approval. The bare `summary` string form (no `plan` field) still works for simple approvals.

---

## Approval TTL and fallback (v0.6+)

Prevent an unreachable operator from permanently stalling the tree:

- `timeout_secs` in the call sets a per-approval TTL.
- A background watcher applies the run-level `approval_policy` as the fallback when the TTL expires.

If `approval_policy = "auto_reject"` and a lead calls `request_approval` with `timeout_secs = 30`, the approval auto-rejects after 30 seconds if no operator responds.

---

## Reject-with-reason

When an operator rejects with a reason comment, the reason flows back in `comment`. The lead can use it to adapt immediately — for example, switching output format — without a separate `reprompt_worker` call.

Example prompt pattern:

```
result = request_approval(summary="Write results as JSON?", ...)
if not result.approved:
    # result.comment might say "use CSV, not JSON"
    write_as_csv(findings)
```
