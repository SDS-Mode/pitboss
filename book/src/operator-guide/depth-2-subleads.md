# Depth-2 sub-leads

*Added in v0.6.0.*

Pitboss normally allows a single level of nesting: a root lead spawning workers. Depth-2 sub-leads add one more tier — a root lead may spawn sub-leads, each of which spawns workers. Workers remain terminal: they cannot spawn anything.

## When to use sub-leads

Use sub-leads when the root lead's plan decomposes into **orthogonal phases** that each need their own clean Claude context. For example:
- Phase 1 gathers inputs; Phase 2 processes them — they don't share implementation state, so keeping them in separate contexts avoids prompt pollution.
- Different phases have meaningfully different budget requirements.
- You want to prevent one phase from reading another's intermediate work (strict-tree isolation is the default).

Do not use sub-leads for every multi-worker job. Plain workers are cheaper and simpler. Add sub-leads only when context isolation is worth the overhead.

## Manifest

Enable sub-leads by setting `allow_subleads = true` on the `[[lead]]` block:

```toml
[run]
max_workers = 20
budget_usd = 20.00
lead_timeout_secs = 7200

[[lead]]
id = "root"
allow_subleads = true
max_subleads = 4
max_sublead_budget_usd = 5.00
max_workers_across_tree = 16
directory = "/path/to/repo"
prompt = """
Decompose this project into phases. For each phase, spawn a sub-lead with
its own budget and a focused prompt via spawn_sublead. Wait for all sub-leads
via wait_actor. Synthesize the results.
"""

[lead.sublead_defaults]
budget_usd = 2.00
max_workers = 4
lead_timeout_secs = 1800
read_down = false
```

### `[[lead]]` fields for sub-leads

| Field | Default | Notes |
|-------|---------|-------|
| `allow_subleads` | false | Required to expose `spawn_sublead` to the root lead. |
| `max_subleads` | none | Optional cap on total sub-leads spawned across the run. |
| `max_sublead_budget_usd` | none | Cap on the per-sub-lead `budget_usd` envelope. Spawn attempts exceeding this fail fast before any state is mutated. |
| `max_workers_across_tree` | none | Cap on total live workers (root + all sub-trees). |

### `[lead.sublead_defaults]`

Optional defaults inherited by `spawn_sublead` calls that omit those parameters.

## `spawn_sublead` MCP tool

Available only to the root lead when `allow_subleads = true`.

```
spawn_sublead(
  prompt: string,
  model: string,
  budget_usd: float,
  max_workers: u32,
  lead_timeout_secs?: u64,
  initial_ref?: { [key: string]: any },
  read_down?: bool,
)
→ { sublead_id: string }
```

- `initial_ref` — optional key-value snapshot seeded into the sub-lead's `/ref/*` namespace at spawn time. Use it to pass shared configuration (e.g., target file paths, conventions, task list) without requiring the sub-lead to make a separate `kv_get` call.
- `read_down` — when `true`, the root lead can observe the sub-tree's KV store. Default `false` (strict-tree isolation).

## `wait_actor` MCP tool

`wait_actor` generalizes `wait_for_worker` to accept any actor id — worker or sub-lead:

```
wait_actor(actor_id: string, timeout_secs?: u64)
→ ActorTerminalRecord
```

`wait_for_worker` is retained as a back-compat alias and continues to work for worker ids.

## Authorization model

| Access | Default behavior |
|--------|-----------------|
| Root reads into sub-tree | Blocked. Pass `read_down = true` at `spawn_sublead` to allow. |
| Sub-lead reads into sibling sub-tree | Always blocked. |
| Peer visibility within a layer | Each `/peer/<X>/*` is readable only by X itself, that layer's lead, or the operator via TUI. Workers within a sub-tree do not see each other's peer slots. |
| Operator (TUI) | Super-user across all layers, regardless of `read_down`. |

## Approval routing

All approval requests — from any layer — route to the operator via the TUI approval list pane. The root lead is not an approval authority. Use `[[approval_policy]]` rules to auto-approve routine requests before they reach the operator queue.

See [Approvals](./approvals.md) for the full policy model.

## Kill-with-reason

`cancel_worker(target, reason)` — when invoked with a `reason` string, a synthetic `[SYSTEM]` reprompt is delivered to the killed actor's direct parent lead. This lets the parent adapt without a separate `reprompt_worker` round-trip:
- Kill a worker → its sub-lead (or root lead) receives the reason.
- Kill a sub-lead → the root lead receives the reason.

## Cancel cascade

When the operator cancels a run (via TUI `X` or Ctrl-C), cancellation propagates depth-first through the entire tree: root → each sub-lead → each sub-lead's workers. No straggler processes are left running.

## TUI presentation

In the TUI, sub-trees render as collapsible containers. The container header shows the `sublead_id`, a budget bar, worker count, approval badge, and `read_down` indicator. Tab cycles focus across containers; Enter on a header toggles expand/collapse.

## Run-global leases for cross-tree coordination

Per-layer `/leases/*` KV namespaces are isolated per sub-tree. For resources that span sub-trees (e.g., a path on the operator's filesystem), use the run-global lease API:

```
run_lease_acquire(key: string, ttl_secs: u32) → { lease_id, version }
run_lease_release(lease_id: string) → { ok: true }
```

See [Leases & coordination](./leases.md) for guidance on when to use `/leases/*` vs `run_lease_*`.
