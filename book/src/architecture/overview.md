# Architecture overview

## One-screen mental model

Pitboss is a **dispatcher** that manages a tree of `claude` subprocesses under operator-defined guardrails. In the simplest case (flat mode), it's a process pool. In the full case (depth-2 hierarchical), it's a two-tier tree with a control plane and shared coordination state.

```
Operator
  │
  ├─ pitboss dispatch <manifest>
  │     │
  │     ├─ [root lead] ──────── MCP bridge (stdio↔unix socket)
  │     │     │                              │
  │     │     │              MCP server ─────┘
  │     │     │                  │
  │     │     │                  ├─ DispatchState (root layer)
  │     │     │                  │    KvStore, LeaseRegistry, ApprovalQueue
  │     │     │                  │
  │     │     ├─ spawn_sublead ──┤
  │     │     │     │            ├─ LayerState (sub-lead S1)
  │     │     │     │            │    KvStore, workers, budget
  │     │     │     │            │
  │     │     │     └─ [S1 lead] ──── spawn_worker → [W1, W2, W3]
  │     │     │
  │     │     └─ spawn_sublead ──┤
  │     │                        ├─ LayerState (sub-lead S2)
  │     │                        │
  │     │                        └─ [S2 lead] ──── spawn_worker → [W4, W5]
  │     │
  │     └─ control.sock ─────── pitboss-tui (operator floor view)
  │
  └─ run artifacts: ~/.local/share/pitboss/runs/<run-id>/
```

## Key components

### Dispatcher (`pitboss`)

The CLI binary. Reads the manifest, validates it, sets up the run directory, and kicks off the dispatch. In flat mode, it starts a process pool directly. In hierarchical mode, it starts the MCP server and spawns the lead subprocess with a generated `--mcp-config`.

### MCP server

Listens on a unix socket per run. Receives tool calls from leads (and workers) via the bridge proxy. All tool handlers route through `DispatchState` for authorization and state mutation.

### The bridge

`pitboss mcp-bridge <socket>` — a stdio-to-socket proxy auto-launched for each `claude` subprocess that needs MCP access. Claude Code speaks stdio JSON-RPC; the pitboss MCP server speaks unix socket. The bridge translates between them and stamps `_meta` (actor identity) into each forwarded call.

### `DispatchState`

The root state object. In v0.6+, it wraps an `Arc<LayerState>` for the root layer plus a registry of sub-lead `LayerState` objects. All MCP tool handlers receive a `DispatchState` reference and use it to locate the right layer for authorization and coordination.

### `LayerState`

Per-layer state: the layer's `KvStore`, worker registry, budget tracking, `ApprovalQueue`, and cancel tokens. Workers within a layer share one `LayerState`. Sub-leads each get their own `LayerState` — this is what provides isolation.

### Control socket

A unix socket (`control.sock`) in the run directory that the TUI connects to. The TUI sends control operations (cancel, pause, reprompt, approve) and receives push events (worker state changes, approval requests, budget updates). The dispatcher applies operations to `DispatchState` and broadcasts events back.

### TUI (`pitboss-tui`)

A ratatui terminal application that connects to the control socket of a running dispatch. Reads-only for finished runs (no control socket). See [TUI](../operator-guide/tui.md) for the operator interface.

## Data flow: a worker spawn

1. Lead calls `mcp__pitboss__spawn_worker` via its MCP bridge subprocess.
2. Bridge reads the stdio request and forwards it to the MCP server on the unix socket, adding `_meta.actor_id` from its `--actor-id` arg.
3. MCP server handler receives the call, looks up the caller's layer in `DispatchState`, and validates the request (budget, worker cap, plan gate).
4. Dispatcher spawns a `claude` subprocess with generated `--mcp-config` (for workers: shared-store tools only) and a new worktree.
5. Worker's task id and worktree path are returned to the lead via MCP response.
6. TUI receives a `WorkerSpawned` push event from the control socket and renders a new tile.

## Philosophy

> The model is stochastic. The pit is not.

Pitboss bets on four guarantees:

1. **Isolation.** Each worker runs in its own git worktree. One bad hand doesn't contaminate the next.
2. **Observability.** Every token, every cache hit, every session id is persisted. The artifacts are on the table.
3. **Bounded risk.** Workers, budget, and timeouts are explicit. The house knows its exposure before the first card is dealt.
4. **Determinism where it's free.** Stream-JSON parsing, cancellation protocol, KV authorization, approval policy matching — all Rust, all deterministic, none LLM-evaluated.

For deeper dives, see [The two-layer model](./two-layer-model.md) and [Lease scope selection](./lease-scope-selection.md).
