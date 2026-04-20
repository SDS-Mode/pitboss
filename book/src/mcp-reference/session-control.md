# Session control tools

These tools are available to the **root lead** only. Sub-lead leads can call `spawn_worker` (for their own workers) but not `spawn_sublead` (depth-2 cap enforced at both the MCP handler and the sub-lead's `--allowedTools`).

---

## `spawn_worker`

Spawn a new worker subprocess with a given prompt.

**Args:**
```json
{
  "prompt": "string (required)",
  "directory": "string (optional, defaults to lead's directory)",
  "branch": "string (optional, auto-generated if omitted)",
  "tools": ["string"] ,
  "timeout_secs": 600,
  "model": "claude-haiku-4-5"
}
```

**Returns:** `{ "task_id": "string", "worktree_path": "string or null" }`

**Rules:**
- `prompt` is required.
- `directory` defaults to the lead's `directory`.
- `model` defaults to the lead's model. Override per-worker when you need a heavier worker (Sonnet or Opus) under a Haiku lead.
- `tools` defaults to the lead's `tools`.
- Fails with `budget exceeded` if `spent + reserved + next_estimate > budget_usd`.
- Fails with `worker cap reached` if the number of live workers equals `max_workers`.
- Fails with `plan approval required` if `require_plan_approval = true` and no plan has been approved yet.

---

## `spawn_sublead` *(v0.6+, root lead only)*

Spawn a sub-lead with its own Claude session, budget envelope, and isolated coordination layer.

**Args:**
```json
{
  "prompt": "string (required)",
  "model": "string (required)",
  "budget_usd": 2.00,
  "max_workers": 4,
  "lead_timeout_secs": 1800,
  "initial_ref": { "key": "value" },
  "read_down": false
}
```

**Returns:** `{ "sublead_id": "string" }`

- Available only when `allow_subleads = true` in the manifest.
- `budget_usd` and `max_workers` are required unless `read_down = true`.
- `initial_ref` seeds the sub-lead's `/ref/*` namespace at spawn time.
- Fails if `budget_usd > max_sublead_budget_usd` (manifest cap enforcement, pre-state).

---

## `wait_actor` *(v0.6+)*

Wait for any actor (worker or sub-lead) to settle. Generalizes `wait_for_worker`.

**Args:** `{ "actor_id": "string", "timeout_secs": 120 }`

**Returns:** `ActorTerminalRecord` — either `{ "Worker": TaskRecord }` or `{ "Sublead": SubleadTerminalRecord }` depending on actor type.

`wait_for_worker` is retained as a back-compat alias; it unwraps the `Worker` variant.

---

## `wait_for_worker`

Block until a specific worker settles (back-compat alias for `wait_actor` on worker ids).

**Args:** `{ "task_id": "string", "timeout_secs": 120 }`

**Returns:** Full `TaskRecord` when the worker settles.

---

## `wait_for_any`

Block until the first of a list of workers settles.

**Args:** `{ "task_ids": ["string"], "timeout_secs": 120 }`

**Returns:** `{ "task_id": "string", "record": TaskRecord }` — the first to finish.

---

## `worker_status`

Non-blocking peek at a worker's current state.

**Args:** `{ "task_id": "string" }`

**Returns:** `{ "state": "Running|Paused|Frozen|Done|...", "started_at": "...", "partial_usage": {...}, "last_text_preview": "...", "prompt_preview": "..." }`

---

## `list_workers`

Snapshot of all active and completed workers.

**Args:** `{}`

**Returns:** `{ "workers": [{ "task_id": "string", "state": "...", "prompt_preview": "...", "started_at": "..." }, ...] }`

---

## `cancel_worker`

Signal a worker's cancel token, terminating the subprocess.

**Args:** `{ "task_id": "string", "reason": "optional string" }`

**Returns:** `{ "ok": true }`

When `reason` is supplied, a synthetic `[SYSTEM]` reprompt is delivered to the killed actor's direct parent lead. This lets the parent adapt without a separate `reprompt_worker` call.

---

## `pause_worker`

Pause a running worker. Two modes:

| `mode` | Behavior |
|--------|----------|
| `"cancel"` (default) | Terminate the subprocess + snapshot `claude_session_id`. `continue_worker` spawns `claude --resume`. Zero context loss on Anthropic's side; some reload cost on resume. |
| `"freeze"` *(v0.5+)* | SIGSTOP the subprocess in place. `continue_worker` sends SIGCONT. No state loss at all, but long freezes risk Anthropic dropping the HTTP session — use for short pauses only. |

**Args:** `{ "task_id": "string", "mode": "cancel|freeze" }`

**Returns:** `{ "ok": true }`

Fails if the worker is not in `Running` state with an initialized session.

---

## `continue_worker`

Resume a paused or frozen worker.

**Args:** `{ "task_id": "string", "prompt": "optional string" }`

**Returns:** `{ "ok": true }`

- For paused (cancel-mode) workers: spawns `claude --resume <session_id>`. Optional `prompt` is added to the resume.
- For frozen workers: sends SIGCONT. `prompt` is ignored for frozen workers — use `reprompt_worker` after continue if you want to redirect.

---

## `reprompt_worker`

Mid-flight course correction: terminate and restart a worker with a new prompt, preserving the claude session via `--resume`.

**Args:** `{ "task_id": "string", "prompt": "string (required)" }`

**Returns:** `{ "ok": true }`

Differs from `pause_worker` + `continue_worker` in that the new prompt replaces the worker's current direction rather than resuming it. Use when a worker has gone off-track and you want to give it an explicit correction.
