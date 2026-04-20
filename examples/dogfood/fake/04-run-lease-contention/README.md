# Spotlight #04: Run-global lease contention

## What it demonstrates

This spotlight exercises **run-global lease coordination** across sub-trees in a
depth-2 dispatch.

**Scenario:** An operator has a shared filesystem resource (e.g., `output.json`)
that multiple sub-leads need exclusive write access to. The pitboss run-global
lease API (`run_lease_acquire` / `run_lease_release`) provides cross-tree
coordination for such resources.

Two sub-leads (S1 and S2) compete for the same lease:
- **S1** acquires the lease first and holds it successfully
- **S2** attempts to acquire the same lease → blocked with S1 named as current
  holder
- **S1** releases the lease
- **S2** retries and now acquires successfully

This spotlight demonstrates the key distinction in the pitboss spec:
- `/leases/*` is for **intra-layer** coordination (within a single sub-tree)
- `run_lease_*` is for **cross-tree** coordination (operator filesystem, shared
  services, etc.)

## How to run

This spotlight is verified via an **in-process integration test**:

```
cargo test --test dogfood_fake_flows dogfood_run_lease_contention
```

The test constructs a `DispatchState` and `McpServer` in-process, spawns two
sub-leads via the MCP `spawn_sublead` tool, drives them through the lease
acquire/release dance, and asserts that blocking, release, and reacquisition
work correctly.

## Why in-process?

Same reason as spotlights #02 and #03: MCP tools (`spawn_sublead`,
`run_lease_acquire`) cannot be exercised via subprocess fake-claude in v0.6
because `PITBOSS_FAKE_MCP_SOCKET` is not injected into the lead's subprocess
environment. The in-process pattern gives full MCP-layer coverage with no real
subprocess or API dependency.

## Files

| File | Purpose |
|---|---|
| `manifest.toml` | Reference manifest (documentation only — tests use in-code config) |
| `README.md` | This file |
| `expected-observables.md` | Plain-English scenario with expected lease state transitions |
| `run.sh` | Prints instructions pointing to the cargo test invocation |
