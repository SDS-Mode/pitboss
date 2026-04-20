# MCP Tool Reference — Overview

In hierarchical mode, pitboss starts an MCP server on a unix socket and auto-generates an `--mcp-config` for the lead's `claude` subprocess. All tools in this reference are automatically added to the lead's `--allowedTools` list — the operator does not list them explicitly.

Workers get a narrower toolset (shared-store tools only; no `spawn_worker`, no `spawn_sublead`).

## Tool categories

| Category | Page | Who can call |
|----------|------|-------------|
| Session control | [Session control](./session-control.md) | Lead only (root lead) |
| Coordination & state | [Coordination & state](./coordination.md) | Lead + workers |
| Approvals | [Approvals](./approvals.md) | Lead only |

## MCP tool name prefix

All pitboss tools are prefixed `mcp__pitboss__`. In prompts and `--allowedTools` lists, use the full name:

```
mcp__pitboss__spawn_worker
mcp__pitboss__kv_get
mcp__pitboss__request_approval
```

## Structured content wrapper

All tool responses are wrapped in a record (`{ entry: ... }`, `{ entries: [...] }`, `{ workers: [...] }`, etc.). Claude Code's MCP client requires `structuredContent` to be a record, not a bare array or null. Unwrap one level when reading results in a lead prompt.

## The bridge

Claude Code's MCP client speaks stdio. The pitboss MCP server listens on a unix socket. Between them is `pitboss mcp-bridge <socket>` — a stdio-to-socket proxy auto-launched via the lead's generated `--mcp-config`. You never invoke it directly.

## Error patterns

| Error | Meaning | Recovery |
|-------|---------|----------|
| `budget exceeded: $X spent + $Y reserved + $Z estimated > $B budget` | Not enough budget headroom to spawn | Finish existing work; surface to operator if needed |
| `worker cap reached: N active (max M)` | Too many live workers | Wait for one to finish via `wait_for_worker` or `wait_for_any` |
| `run is draining: no new workers accepted` | Operator Ctrl-C'd or lead was cancelled | Finish gracefully; don't spawn new work |
| `unknown task_id` | Typo or referring to an unspawned worker | Call `list_workers` to see what's registered |
| `SpawnFailed` | Worker never started (worktree prep failure, branch conflict, non-git directory) | Check stderr log |
| `plan approval required: call propose_plan ...` | `require_plan_approval = true` and no approved plan yet | Call `propose_plan` and wait for approval |

## Full canonical reference

`AGENTS.md` in the source tree is the authoritative machine-readable reference for all tool schemas. The pages in this section derive from it and highlight the most important fields for human readers. If anything here conflicts with the binary's actual behavior, the binary wins — file a PR against this book.
