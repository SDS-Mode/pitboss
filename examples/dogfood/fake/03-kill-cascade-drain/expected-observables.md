# Expected Observables — Spotlight #03: Depth-first cascade cancellation

This document describes what an operator or reader should expect to observe
when the scenario in `dogfood_kill_cascade_drain` runs.

## Setup

A root lead spawns two sub-leads in parallel:
- **S1** — "phase 1"
- **S2** — "phase 2"

Each sub-lead has two active workers simulated by injecting cancel tokens
directly into the sub-tree's `worker_cancels` map:
- **S1-w0**, **S1-w1** — workers under S1's sub-tree
- **S2-w0**, **S2-w1** — workers under S2's sub-tree

Total cancel tokens in the tree before cancel: 1 root + 2 sub-lead + 4 worker
= 7 tokens. None are draining at this point.

---

## Pre-cancel state

| Token | State |
|---|---|
| `root.cancel` | not draining |
| `S1.cancel` | not draining |
| `S2.cancel` | not draining |
| `S1-w0` | not draining |
| `S1-w1` | not draining |
| `S2-w0` | not draining |
| `S2-w1` | not draining |

---

## Operator triggers root cancel

The operator calls `state.root.cancel.drain()`. This is the same signal that
`pitboss dispatch` would send when it receives SIGINT/SIGTERM or the operator
presses Ctrl-C in an interactive terminal.

The cascade watcher task installed by `install_cascade_cancel_watcher` is
waiting on `root_cancel.await_drain()`. It wakes immediately and:

1. Iterates every sub-lead registered in `state.subleads`.
2. Calls `sub_layer.cancel.drain()` on each sub-lead's own cancel token.
3. Iterates every worker cancel token in `sub_layer.worker_cancels` and calls
   `tok.drain()` on each.

This happens entirely inside the tokio runtime, so in an in-process test a
`tokio::time::sleep(200ms)` is more than sufficient for the watcher task to
complete. In a real production dispatch with live Claude subprocesses, the
drain phase gives sessions a chance to finalize (write session state, flush
output) before escalating to forceful termination at the end of the
`TERMINATE_GRACE` window.

---

## Post-cancel state (within drain window)

| Token | State |
|---|---|
| `root.cancel` | **draining** (operator triggered this directly) |
| `S1.cancel` | **draining** (cascade from root) |
| `S2.cancel` | **draining** (cascade from root) |
| `S1-w0` | **draining** (cascade from root through S1) |
| `S1-w1` | **draining** (cascade from root through S1) |
| `S2-w0` | **draining** (cascade from root through S2) |
| `S2-w1` | **draining** (cascade from root through S2) |

All 7 tokens reach the draining state. No straggler processes remain.

---

## Summary of invariants demonstrated

| Assertion | Expected result |
|---|---|
| Pre-cancel: 2 sub-leads in `state.subleads` | Pass |
| Pre-cancel: each sub-lead has 2 worker tokens | Pass |
| Pre-cancel: no tokens are draining | Pass |
| Post-cancel: root token is draining | Pass |
| Post-cancel: S1 sub-tree token is draining | Pass |
| Post-cancel: S2 sub-tree token is draining | Pass |
| Post-cancel: all 4 worker tokens are draining | Pass |
| All changes complete within 200 ms grace window | Pass |
