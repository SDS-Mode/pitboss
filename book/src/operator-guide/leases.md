# Leases & coordination

Pitboss provides a hierarchical coordination surface for workers and leads:

- **Per-layer KV store** — an in-memory key-value store per dispatch layer (root, each sub-lead). Four namespaces with different access rules.
- **Per-layer leases** — `/leases/*` namespace within each layer's KV store.
- **Run-global leases** — `run_lease_acquire` / `run_lease_release` for cross-tree coordination (v0.6+).

## The KV namespaces

All KV tools operate on paths within the current layer's store.

| Namespace | Who can write | Who can read | Use for |
|-----------|---------------|--------------|---------|
| `/ref/*` | Lead only | All actors in this layer | Shared configuration, task lists, conventions the lead wants all workers to see |
| `/peer/<actor-id>/*` | That actor only (and lead as override) | That actor + the layer's lead | Per-worker outputs (findings, status flags, partial results) |
| `/peer/self/*` | Any actor | — | Alias: the dispatcher resolves `/peer/self/` to `/peer/<caller.actor_id>/` at the tool layer |
| `/shared/*` | All actors | All actors | Loose cross-worker coordination (shared findings, counters) |
| `/leases/*` | Managed via `lease_acquire` / `lease_release` | — | Mutual exclusion within this layer |

## KV tools

| Tool | Purpose |
|------|---------|
| `kv_get` | Read a single entry by path |
| `kv_set` | Write a value; bumps version on each write |
| `kv_cas` | Compare-and-swap: write only if the current version matches `expected_version` |
| `kv_list` | List entries matching a glob pattern |
| `kv_wait` | Block until a path reaches a minimum version (long-poll) |
| `lease_acquire` | Acquire a named lease with a TTL; blocks or fails if held |
| `lease_release` | Release a held lease |

See [Coordination & state](../mcp-reference/coordination.md) for full signatures and return shapes.

## Lease acquisition

```
lease_acquire(name: string, ttl_secs: u32, wait_secs?: u32)
→ { lease_id, version, acquired_at, expires_at }
```

- `name` — the lease key (a path under `/leases/*`).
- `ttl_secs` — how long the lease lives after acquisition. The lease auto-expires if the holder crashes.
- `wait_secs` — optional: block up to this many seconds for the lease to become available. Default: fail immediately if already held.

Leases are auto-released when the holding actor's MCP session terminates (connection drop, worker crash, etc.).

## Per-layer vs run-global leases

**Use `/leases/*` (per-layer)** for resources internal to the current sub-tree:
- A worker-coordinated counter for "next chunk to process within this sub-tree"
- A mutex for a shared file within one phase's working directory
- Any resource only one sub-tree ever writes to

**Use `run_lease_acquire` (run-global)** for resources that span sub-trees:
- A path on the operator's filesystem that any sub-tree might write to
- A shared service or port that multiple sub-leads compete for
- Any resource that must be serialized across the entire dispatch tree

```
run_lease_acquire(key: string, ttl_secs: u32) → { lease_id, version }
run_lease_release(lease_id: string) → { ok: true }
```

The run-global registry is on `DispatchState` — outside any layer — so it spans all sub-trees.

**When in doubt:** prefer `run_lease_acquire`. Over-serializing is safer than silent cross-tree collision.

## Shared store dump

Set `dump_shared_store = true` in `[run]` to write `<run-dir>/shared-store.json` at finalize time. Useful for post-mortem inspection of cross-worker coordination state.

```toml
[run]
dump_shared_store = true
```

## Architecture note

The KV store is **in-memory and per-run**. It is not persisted between runs (except via the optional `shared-store.json` dump). Workers in separate runs cannot see each other's state.

See [Lease scope selection](../architecture/lease-scope-selection.md) for a deeper discussion of the architectural rationale.
