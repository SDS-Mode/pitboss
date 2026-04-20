# Lease scope selection

Pitboss provides two lease primitives. Choosing the right one prevents silent cross-tree collisions.

## Quick rule

| Resource | Primitive to use |
|----------|-----------------|
| Internal to one sub-tree | `/leases/*` via `lease_acquire` |
| Shared across sub-trees | `run_lease_acquire` |
| When in doubt | `run_lease_acquire` |

Over-serializing is always safer than silent collision.

## The two primitives

### Per-layer leases: `lease_acquire` / `lease_release`

Each layer (root, S1, S2, ...) has its own `KvStore` with its own `/leases/*` namespace. A lease acquired by an actor in S1 at path `/leases/output-file` is **entirely separate** from a lease acquired by an actor in S2 at the same path.

This isolation is by design. It means sub-trees can coordinate internally without knowing about each other. It also means per-layer leases provide **no cross-tree serialization**.

Use per-layer leases for:
- A chunk-processing counter within one phase's workers
- A mutex for a temporary file that only one sub-tree touches
- Any resource fully scoped to a single sub-tree's lifetime

### Run-global leases: `run_lease_acquire` / `run_lease_release`

Run-global leases live on `DispatchState` (outside any layer) in a dedicated `LeaseRegistry`. A lease acquired at key `"output.json"` by S1 blocks S2 from acquiring the same key.

Use run-global leases for:
- A path on the operator's filesystem that multiple sub-trees write to
- A shared service or network port that only one phase should use at a time
- Any resource that must be serialized across the entire dispatch tree

## Why the split exists

Sub-tree isolation is a core guarantee. If per-layer leases were globally shared, one sub-tree's lock could block an unrelated sub-tree — violating isolation and making sub-tree behavior dependent on sibling activity.

The run-global lease registry exists as a deliberate, explicit cross-tree escape hatch. Because it's separate and explicitly invoked, operators and leads can reason about which resources are cross-tree-serialized without inspecting sibling sub-trees.

## Auto-release on termination

Both primitive types auto-release their leases when the holding actor's MCP session terminates (connection drop, worker crash, Ctrl-C). This prevents deadlocks from crashed workers holding leases indefinitely.

The TTL (`ttl_secs`) is a belt-and-suspenders backup: if the auto-release misses (e.g., an ungraceful socket close), the lease expires after the TTL.

## Debugging lease contention

When `run_lease_acquire` or `lease_acquire` returns a contention error, the error message names the current holder:

```
lease "output.json" held by actor <uuid> (S1→W2), expires in 28s
```

This lets the waiting actor know whether to retry immediately (holder is about to expire) or wait for an explicit release.

## Spotlight

Spotlight #04 ([Run-global lease contention](../cookbook/run-lease-contention.md)) demonstrates the full acquire/block/release/retry sequence in a runnable test.
