# The two-layer model

Pitboss v0.6 introduced a second coordination tier. Understanding the layer structure is essential for writing correct depth-2 manifests.

## Layers

A **layer** is the scope within which workers and leads share coordination state (KV store, leases, approval queue). In the v0.6 model:

- **Root layer** — always present. Contains the root lead, any direct workers the root lead spawns, and the run-global lease registry.
- **Sub-lead layers** — one per spawned sub-lead. Each sub-lead layer contains that sub-lead's lead session and the workers it spawns.

Workers remain terminal: they cannot spawn anything. A sub-lead is a lead within its own layer; it can spawn workers but not other sub-leads.

```
Root layer
  ├─ root lead (reads/writes root KvStore)
  ├─ worker W0 (if root lead calls spawn_worker directly)
  └─ run-global LeaseRegistry (spans all layers)

Sub-lead S1 layer
  ├─ sub-lead S1 (reads/writes S1 KvStore)
  ├─ worker W1
  └─ worker W2

Sub-lead S2 layer
  ├─ sub-lead S2 (reads/writes S2 KvStore)
  ├─ worker W3
  └─ worker W4
```

## Isolation by default

Sub-tree layers are **opaque to root** unless `read_down = true` is passed at `spawn_sublead` time. This means:

- The root lead cannot `kv_get` any path from S1's or S2's layer.
- S1 cannot `kv_get` any path from S2's layer or the root layer.
- Workers within S1 cannot see each other's `/peer/<X>/*` slots; only S1's lead and the operator can.

This isolation is not just a convention — it's enforced at the MCP tool handler layer.

## The `read_down` escape hatch

When the root lead calls `spawn_sublead(..., read_down=true)`, the root gains read access into that sub-tree's KV namespace. Write access is never granted to the root for sub-tree namespaces (the sub-lead's workers must remain the writers).

Use `read_down` when:
- The root lead's synthesis step needs to observe sub-tree progress without going through explicit handoff patterns (like `/peer/S1/done`).
- You're building a monitoring-style root that reports on all sub-tree states.

Avoid `read_down` when you want strict phase isolation — if Phase 1 shouldn't influence Phase 2's context, don't give root visibility that it might inadvertently surface in Phase 2's prompt.

## The operator is always super-user

The TUI can read and write across all layers regardless of `read_down`. This is intentional: the operator needs unrestricted visibility for debugging and approval decisions.

## Sub-leads as peers in the root layer

Sub-leads are not workers in the root layer. They appear as sub-tree containers in the TUI, not as worker tiles. The root lead tracks sub-leads via `sublead_id` (returned by `spawn_sublead`) and waits on them via `wait_actor`.

From the root lead's perspective:
- `spawn_worker` → creates a worker tile in the root layer
- `spawn_sublead` → creates a sub-tree with its own layer

Both return identifiers the root lead can use with `wait_actor`.

## Budget flow

Budget flows hierarchically:

1. Operator sets `budget_usd = 20.00` on the run.
2. Root lead calls `spawn_sublead(budget_usd=5.0)` → $5 is reserved from the root budget.
3. Sub-lead S1 spawns workers; each worker spawn reserves an estimate from S1's $5 envelope.
4. When S1 terminates, any unspent envelope returns to the root's reservable pool.

The `max_sublead_budget_usd` manifest cap enforces an upper bound on any single sub-lead envelope, regardless of what the root lead requests.

## Cancel cascade

Cancellation propagates depth-first. A root cancel:
1. Trips the root layer's drain token.
2. The cascade watcher finds all registered sub-leads and trips their cancel tokens.
3. Each sub-lead's cancel triggers its worker cancel tokens.

The two-phase drain at each layer ensures no straggler processes. Sub-leads spawned mid-drain are caught by a spawn-time `is_draining()` check.

## Kill-with-reason routing

`cancel_worker(task_id, reason)` routes one hop upward:
- Cancel a worker → the worker's layer lead (sub-lead or root) receives the synthetic `[SYSTEM]` reprompt.
- Cancel a sub-lead → the root lead receives the reprompt.

The root lead is never notified for cancels that stay within a sub-tree it doesn't own (unless it has `read_down = true` and is observing).
