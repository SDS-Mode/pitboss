# Spotlight #02: Strict-tree isolation

**Source:** [`examples/dogfood/fake/02-isolation-strict-tree/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/fake/02-isolation-strict-tree)

## What it demonstrates

This spotlight exercises **per-layer KV store isolation** and **strict peer visibility** in a depth-2 dispatch.

A root lead decomposes a multi-phase job into two parallel sub-trees:
- **S1** — "phase 1: gather inputs"
- **S2** — "phase 2: process outputs"

Each sub-lead writes its progress to `/shared/progress`. The spotlight proves:

1. **KV isolation**: S1's `/shared/progress` and S2's `/shared/progress` live in separate layer stores. Each sub-lead reads back its own write; neither can observe the other's.

2. **Root isolation**: Root's `/shared/progress` is in a third, independent store. After S1 and S2 write their progress, root's layer still has no entry at that path.

3. **Strict peer visibility**: Workers within the same layer cannot read each other's `/peer/<id>/*` slots. The MCP server rejects such reads with a "strict peer visibility" error.

4. **Layer-lead privilege**: The root lead (as layer lead of the root layer) CAN read any worker's `/peer/<id>/*` slot in the root layer.

## How to run

This spotlight is verified via an in-process integration test (no real subprocess or API dependency):

```bash
cargo test --test dogfood_fake_flows dogfood_isolation_strict_tree
```

The test constructs a `DispatchState` and `McpServer` in-process and drives them via `FakeMcpClient` over a Unix socket.

The [`run.sh`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/02-isolation-strict-tree/run.sh) script prints instructions pointing to this cargo invocation.

## Key assertions

See [`expected-observables.md`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/02-isolation-strict-tree/expected-observables.md) for the full plain-English description of expected behavior.

## Why this matters

Without strict-tree isolation, a noisy sub-tree could observe another sub-tree's partial state and corrupt its coordination logic. The KV isolation guarantee means each sub-tree can be reasoned about independently — operators can audit one phase without worrying about contamination from another.

## Related concepts

- [Depth-2 sub-leads](../operator-guide/depth-2-subleads.md) — authorization model
- [Leases & coordination](../operator-guide/leases.md) — KV namespaces
- [Architecture: The two-layer model](../architecture/two-layer-model.md) — how layers are structured
