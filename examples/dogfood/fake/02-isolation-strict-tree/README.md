# Spotlight #02: Strict-tree isolation

## What it demonstrates

This spotlight exercises **per-layer KvStore isolation** and **strict peer
visibility** in a depth-2 dispatch.

**Scenario:** An operator runs a root lead that decomposes a multi-phase job
into two parallel sub-trees:
- **S1** — "phase 1: gather inputs"
- **S2** — "phase 2: process outputs"

Each sub-lead writes its progress to `/shared/progress`. The spotlight proves:

1. **KV isolation**: S1's `/shared/progress` and S2's `/shared/progress` live
   in separate layer stores. Each sub-lead reads back its own write; neither
   can observe the other's.

2. **Root isolation**: Root's `/shared/progress` is in a third, independent
   store. After S1 and S2 write their progress, root's layer still has no
   entry at that path.

3. **Strict peer visibility**: Workers within the same layer cannot read each
   other's `/peer/<id>/*` slots. The MCP server rejects such reads with a
   "strict peer visibility" error. Sub-leads are not the right actors for
   this demonstration (each sub-lead is the lead of its own layer and
   therefore has full visibility over that layer's peer namespace); this
   rule applies to actors sharing the same coordination layer.

4. **Layer-lead privilege**: The root lead (as layer lead of the root layer)
   CAN read any worker's `/peer/<id>/*` slot in the root layer.

## How to run

This spotlight is verified via an **in-process integration test**, not by
running the manifest directly:

```
cargo test --test dogfood_fake_flows dogfood_isolation_strict_tree
```

The test constructs a `DispatchState` and `McpServer` in-process and drives
them via `FakeMcpClient`, so no real Claude API call is made.

## Why in-process (vs subprocess like spotlight #01)?

Spotlight #01 is subprocess-driven: it spawns `pitboss dispatch` as a real
OS process against a fake-claude binary. That path validates the CLI dispatch
pipeline and manifest parsing, but it cannot exercise MCP tools
(`spawn_sublead`, `kv_set`, `kv_get`) because `PITBOSS_FAKE_MCP_SOCKET` is
not injected into the lead's subprocess environment in v0.6.

Spotlights #02–#06 use the in-process pattern established by
`crates/pitboss-cli/tests/sublead_flows.rs` (Task 5.2): spin up a
`DispatchState` + `McpServer` inside the test, then drive them directly via
`FakeMcpClient` over a Unix socket. This gives full MCP-layer coverage with
no real subprocess or API dependency.

## Files

| File | Purpose |
|---|---|
| `manifest.toml` | Reference manifest (documentation only — tests use in-code config) |
| `README.md` | This file |
| `expected-observables.md` | Plain-English description of expected behavior |
| `run.sh` | Prints instructions pointing to the cargo test invocation |
