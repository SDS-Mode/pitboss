# Coordination & state tools

These tools are available to both **leads** and **workers** (though with different access levels depending on the namespace). They operate on the current layer's in-memory KV store.

For guidance on when to use `/leases/*` (per-layer) vs `run_lease_acquire` (run-global), see [Lease scope selection](../architecture/lease-scope-selection.md).

---

## KV namespaces recap

| Namespace | Lead can write | Worker can write | Notes |
|-----------|---------------|-----------------|-------|
| `/ref/*` | Yes | No | Lead-authored shared context for all workers |
| `/peer/<id>/*` | Yes (any actor) | Only own path | Per-worker output slots |
| `/peer/self/*` | Yes | Yes | Alias resolving to caller's actor id |
| `/shared/*` | Yes | Yes | Loose cross-worker coordination |
| `/leases/*` | Managed | Managed | Via `lease_acquire` / `lease_release` only |

---

## `kv_get`

Read a single entry.

**Args:** `{ "path": "/ref/config" }`

**Returns:** `{ "entry": { "path": "...", "value": "bytes", "version": 1, "updated_at": "..." } | null }`

Returns `null` in the `entry` field if the path does not exist (wrapped in a record per MCP spec).

---

## `kv_set`

Write a value to a path. Increments the version on each write.

**Args:**
```json
{
  "path": "/shared/findings/my-result",
  "value": "bytes (UTF-8 string or base64)",
  "override_flag": false
}
```

**Returns:** `{ "version": 2 }`

- Workers can only write to their own `/peer/<self>/*` or `/shared/*`. Writing to another worker's `/peer/<X>/*` returns `Forbidden`.
- `override_flag` — reserved; currently unused.

---

## `kv_cas`

Compare-and-swap: write only if the current version matches.

**Args:**
```json
{
  "path": "/shared/counter",
  "expected_version": 3,
  "new_value": "bytes",
  "override_flag": false
}
```

**Returns:** `{ "version": 4, "swapped": true }`

- `swapped: false` means the version didn't match; the write was not applied.
- Use `kv_cas` when multiple workers might write the same path to avoid lost updates.

---

## `kv_list`

List entries matching a glob pattern.

**Args:** `{ "glob": "/shared/findings/*" }`

**Returns:** `{ "entries": [{ "path": "...", "version": 1, "updated_at": "..." }, ...] }`

Returns metadata only (no values). Follow up with `kv_get` for values.

---

## `kv_wait`

Block until a path reaches a minimum version. Useful for workers to wait until the lead writes a shared configuration, or for the lead to wait until a worker writes its result.

**Args:**
```json
{
  "path": "/peer/self/completed",
  "timeout_secs": 60,
  "min_version": 1
}
```

**Returns:** `Entry` when the condition is met. Times out with an error if `timeout_secs` elapses.

---

## `lease_acquire`

Acquire a named mutex within the current layer. Auto-released on actor termination.

**Args:**
```json
{
  "name": "/leases/output-file",
  "ttl_secs": 30,
  "wait_secs": 10
}
```

**Returns:** `{ "lease_id": "uuid", "version": 1, "acquired_at": "...", "expires_at": "..." }`

- `name` is a path under `/leases/*`.
- `ttl_secs` — how long the lease lives after acquisition.
- `wait_secs` — block up to this many seconds for the lease to become available. If omitted, fail immediately if already held.
- The error message on contention names the current holder, so the requesting actor knows who to wait on.

---

## `lease_release`

Release a held lease.

**Args:** `{ "lease_id": "uuid" }`

**Returns:** `{ "ok": true }`

---

## `run_lease_acquire` *(v0.6+)*

Acquire a run-global mutex. Scoped to the entire dispatch (not per-layer). Use for resources that span sub-trees.

**Args:** `{ "key": "string", "ttl_secs": 30 }`

**Returns:** `{ "lease_id": "uuid", "version": 1 }`

Auto-released on actor termination, same as per-layer leases.

---

## `run_lease_release` *(v0.6+)*

Release a run-global lease.

**Args:** `{ "lease_id": "uuid" }`

**Returns:** `{ "ok": true }`

---

## Coordination patterns

### Lead writes a shared config; workers read it

```
# Lead:
kv_set(path="/ref/config", value="target: main branch")

# Workers (in prompt):
Read /ref/config via mcp__pitboss__kv_get. Then proceed with the task.
```

### Worker signals completion; lead polls

```
# Worker:
kv_set(path="/peer/self/done", value="true")

# Lead:
kv_wait(path="/peer/<worker-id>/done", timeout_secs=120, min_version=1)
```

### Workers coordinate via CAS counter

```
# Worker (pseudo):
while true:
  entry = kv_get("/shared/next-chunk")
  n = entry.version
  result = kv_cas("/shared/next-chunk", expected_version=n, new_value=str(n+1))
  if result.swapped:
    process_chunk(n)
    break
  # else: another worker got there first, retry
```

For a canonical reference, see [`AGENTS.md`](https://github.com/SDS-Mode/pitboss/blob/main/AGENTS.md) in the source tree.
