# Spotlight #03: Depth-first cascade cancellation

## What it demonstrates

This spotlight exercises **depth-first cascade cancellation** with a grace
window in a depth-2 dispatch.

**Scenario:** An operator kicks off a long-running dispatch — a root lead that
has spawned two sub-leads (S1 for "phase 1" and S2 for "phase 2"), each with
two active workers. Partway through execution the operator realizes something
is wrong: wrong prompt, runaway API cost, unexpected output. They press cancel.

Within the drain grace window, the root cancel cascades depth-first through
the entire sub-tree: each sub-lead's cancel token is drained, and each
sub-lead's worker cancel tokens are drained. No straggler processes are left
running and burning budget.

Why cascade matters: without explicit cascade, cancelling root would stop the
root lead process but sub-lead sessions and their workers would keep running,
consuming API budget and writing to the shared store, until they timed out or
were killed by an external signal. The cascade watcher installed by
`install_cascade_cancel_watcher` ensures the drain signal propagates to every
node in the tree synchronously within the tokio event loop, so the full
dispatch shuts down cleanly inside the grace window.

## How to run

This spotlight is verified via an **in-process integration test**:

```
cargo test --test dogfood_fake_flows dogfood_kill_cascade_drain
```

The test constructs a `DispatchState` and `McpServer` in-process, spawns two
sub-leads via the MCP `spawn_sublead` tool, injects two simulated worker cancel
tokens into each sub-tree, triggers root cancel, and asserts that every token
in the tree reaches the draining state within 200 ms.

## Why in-process?

Same reason as spotlight #02: MCP tools (`spawn_sublead`) cannot be exercised
via subprocess fake-claude in v0.6 because `PITBOSS_FAKE_MCP_SOCKET` is not
injected into the lead's subprocess environment. The in-process pattern gives
full MCP-layer coverage with no real subprocess or API dependency.

## Files

| File | Purpose |
|---|---|
| `manifest.toml` | Reference manifest (documentation only — tests use in-code config) |
| `README.md` | This file |
| `expected-observables.md` | Plain-English scenario with timing expectations |
| `run.sh` | Prints instructions pointing to the cargo test invocation |
