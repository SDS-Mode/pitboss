# Spotlight #04: Run-global lease contention

**Source:** [`examples/dogfood/fake/04-run-lease-contention/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/fake/04-run-lease-contention)

## What it demonstrates

This spotlight exercises **run-global lease coordination** across sub-trees in a depth-2 dispatch.

**Scenario:** An operator has a shared filesystem resource (e.g., `output.json`) that multiple sub-leads need exclusive write access to. The run-global lease API (`run_lease_acquire` / `run_lease_release`) provides cross-tree coordination.

Two sub-leads (S1 and S2) compete for the same lease key:

1. **S1** acquires the lease first and holds it successfully
2. **S2** attempts to acquire the same lease — blocked, with S1 named as current holder
3. **S1** releases the lease
4. **S2** retries and acquires successfully

## How to run

```bash
cargo test --test dogfood_fake_flows dogfood_run_lease_contention
```

The test constructs a `DispatchState` and `McpServer` in-process, spawns two sub-leads, drives them through the acquire/release dance, and asserts that blocking, release, and reacquisition work correctly.

The [`run.sh`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/04-run-lease-contention/run.sh) script prints instructions pointing to this cargo invocation.

## The key distinction this spotlight demonstrates

| Lease type | Scope | Use for |
|-----------|-------|---------|
| `/leases/*` via `lease_acquire` | Per-layer (sub-tree internal) | Resources only one sub-tree writes to |
| `run_lease_acquire` | Run-global (spans all sub-trees) | Operator filesystem paths, shared services |

If S1 and S2 used per-layer `/leases/*` instead of `run_lease_acquire`, they would each get their own independent `/leases/output-file` with no contention — which is wrong when they're both trying to write the same real file.

## Key assertions

See [`expected-observables.md`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/04-run-lease-contention/expected-observables.md) for the full scenario with expected lease state transitions.

## Related concepts

- [Leases & coordination](../operator-guide/leases.md) — per-layer vs run-global
- [Architecture: Lease scope selection](../architecture/lease-scope-selection.md) — decision guide
- [Coordination & state](../mcp-reference/coordination.md) — `run_lease_acquire` / `run_lease_release` signatures
