# Spotlight #03: Kill-cascade drain

**Source:** [`examples/dogfood/fake/03-kill-cascade-drain/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/fake/03-kill-cascade-drain)

## What it demonstrates

This spotlight exercises **depth-first cascade cancellation** with a grace window in a depth-2 dispatch.

**Scenario:** An operator kicks off a long-running dispatch — a root lead that has spawned two sub-leads (S1 for "phase 1" and S2 for "phase 2"), each with two active workers. Partway through, the operator realizes something is wrong (wrong prompt, runaway cost, unexpected output) and presses cancel.

Within the drain grace window, the root cancel cascades depth-first through the entire sub-tree:
- Root cancel token is triggered
- Each sub-lead's cancel token is triggered
- Each sub-lead's worker cancel tokens are triggered

No straggler processes are left running and burning budget.

## How to run

```bash
cargo test --test dogfood_fake_flows dogfood_kill_cascade_drain
```

The test constructs a `DispatchState` and `McpServer` in-process, spawns two sub-leads via the MCP `spawn_sublead` tool, injects two simulated worker cancel tokens into each sub-tree, triggers root cancel, and asserts that every token in the tree reaches the draining state within 200ms.

The [`run.sh`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/03-kill-cascade-drain/run.sh) script prints instructions pointing to this cargo invocation.

## Why cascade matters

Without explicit cascade, cancelling root would stop the root lead process but sub-lead sessions and their workers would keep running — consuming API budget and writing to the shared store — until they timed out or were killed by an external signal.

The cascade watcher installed by `install_cascade_cancel_watcher` ensures the drain signal propagates to every node in the tree synchronously within the tokio event loop. The full dispatch shuts down cleanly inside the grace window.

## Key assertions

See [`expected-observables.md`](https://github.com/SDS-Mode/pitboss/blob/main/examples/dogfood/fake/03-kill-cascade-drain/expected-observables.md) for the full scenario with timing expectations.

## Related concepts

- [Depth-2 sub-leads](../operator-guide/depth-2-subleads.md) — cancel cascade section
- [Architecture: The two-layer model](../architecture/two-layer-model.md) — tree structure
- [TUI](../operator-guide/tui.md) — `X` key cancels entire run
