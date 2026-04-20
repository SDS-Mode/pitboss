# Hierarchical dispatch with a lead

Hierarchical mode hands a **lead** — a Claude session with MCP orchestration tools — a prompt and a set of guardrails, then lets it decide how many workers to spawn and in what order.

Use hierarchical mode when you're describing a *policy* ("one worker per file in this directory", "one worker per unique author") rather than a fixed list of tasks.

## A minimal hierarchical manifest

```toml
[run]
max_workers = 4
budget_usd = 2.00
lead_timeout_secs = 900

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[lead]]
id = "digest"
directory = "/path/to/your/repo"
prompt = """
List the last 10 commits with:
    git log --format='%H %an %s' -10

Group commits by author. For each unique author, spawn one worker via
mcp__pitboss__spawn_worker with a prompt to summarize that author's commits
in a file at /tmp/digest/<author-slug>.md.

Wait for all workers via mcp__pitboss__wait_for_worker.

Read each output file and write a combined /tmp/digest/SUMMARY.md.

Then exit.
"""
```

## House rules

The three fields under `[run]` are the **house rules** — guardrails the lead must stay within:

| Field | Required | Meaning |
|-------|----------|---------|
| `max_workers` | yes | Hard cap on concurrent + queued workers (1–16). |
| `budget_usd` | yes | Total spend envelope. Each spawn reserves a model-aware estimate; `spawn_worker` returns `budget exceeded` once the estimate would push over the cap. |
| `lead_timeout_secs` | no | Wall-clock cap on the lead itself. Defaults to 3600s. Set generously for multi-phase plans. |

## Validate and dispatch

```bash
pitboss validate pitboss.toml   # prints a hierarchical summary when [[lead]] is set
pitboss dispatch pitboss.toml
```

## What the lead can do

The lead's `--allowedTools` is automatically populated with the full pitboss MCP toolset. The lead does not need to list them. Key tools:

| Tool | What it does |
|------|-------------|
| `mcp__pitboss__spawn_worker` | Spawn a worker with a prompt, optional directory/model/tools |
| `mcp__pitboss__wait_for_worker` | Block until a specific worker finishes |
| `mcp__pitboss__wait_for_any` | Block until the first of a list of workers finishes |
| `mcp__pitboss__list_workers` | Snapshot of all active and completed workers |
| `mcp__pitboss__cancel_worker` | Cancel a running worker |
| `mcp__pitboss__pause_worker` | Pause a worker (cancel-with-resume or SIGSTOP freeze) |
| `mcp__pitboss__continue_worker` | Resume a paused or frozen worker |
| `mcp__pitboss__reprompt_worker` | Mid-flight redirect with a new prompt |
| `mcp__pitboss__request_approval` | Gate an action on operator approval |
| `mcp__pitboss__propose_plan` | Submit a pre-flight plan for operator approval |

Workers also get the 7 shared-store tools (`kv_get`, `kv_set`, `kv_cas`, `kv_list`, `kv_wait`, `lease_acquire`, `lease_release`) for cross-worker coordination. See [Coordination & state](../mcp-reference/coordination.md).

## In the TUI

Lead tiles render with a `★` glyph and a cyan border. Workers spawned by the lead show a `▸` glyph and display `← <lead-id>` on their bottom border. The status bar counts `N workers spawned`.

## Depth-2 sub-leads (v0.6+)

If the job decomposes into orthogonal phases that each need their own clean context, a root lead can spawn **sub-leads** — each with its own budget envelope and coordination layer. Add `allow_subleads = true` to the `[[lead]]` block and use `spawn_sublead` instead of `spawn_worker` for the phase coordinators.

See [Depth-2 sub-leads](../operator-guide/depth-2-subleads.md) for the full model.

## Resuming a hierarchical run

```bash
pitboss resume <run-id>
```

Re-dispatches the lead with `--resume <session-id>`. Workers are **not** individually resumed — the lead decides whether to spawn fresh workers. If the original run used `worktree_cleanup = "on_success"` (the default), set `worktree_cleanup = "never"` on runs you know you'll want to resume.

## Next steps

- [Manifest schema](../operator-guide/manifest-schema.md) — full field reference
- [Flat vs. hierarchical](../operator-guide/flat-vs-hierarchical.md) — decision guide
- [MCP Tool Reference](../mcp-reference/overview.md) — full tool signatures
