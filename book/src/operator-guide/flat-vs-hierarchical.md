# Flat vs. hierarchical mode

Pitboss has two dispatch modes. Choosing the right one before writing a manifest saves significant rework.

## Decision table

| Question | Answer → Mode |
|----------|--------------|
| Can you enumerate every task before running? | Flat |
| Does the decomposition depend on what you find at runtime? | Hierarchical |
| Do you need budget enforcement? | Hierarchical |
| Is the work purely parallel with no coordination? | Flat |
| Does the lead need to observe partial results and decide next steps? | Hierarchical |
| Do sub-tasks need a shared coordination surface (KV store, leases)? | Hierarchical |

## Side-by-side comparison

| | Flat | Hierarchical |
|---|------|------|
| **Tasks declared** | Statically, in the manifest | Dynamically, by the lead at runtime |
| **Number of workers** | Fixed (N `[[task]]` entries) | Dynamic, bounded by `max_workers` |
| **Budget enforcement** | None | Yes, via `budget_usd` + reservation accounting |
| **MCP server** | Not started | Yes, unix socket; auto-bridged to lead |
| **Cross-worker state** | None | Shared KV store + leases |
| **Operator approvals** | Not available | Available via `request_approval` / `propose_plan` |
| **Lead can be paused/redirected** | N/A | Yes, via TUI or MCP tools |
| **Resume semantics** | Each task resumes individually | Only the lead resumes; lead re-decides worker strategy |

## When to use flat mode

- You can write out every `[[task]]` before running.
- The tasks are independent — each one doesn't need the output of another.
- You want the simplest possible setup with no MCP overhead.
- You're running read-only analysis where every target is known up front.

```toml
[run]
max_parallel = 3

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[task]]
id = "summarize-a"
directory = "/path/to/repo"
prompt = "Summarize file-a.txt into one sentence. Write to /tmp/summaries/a.md."

[[task]]
id = "summarize-b"
directory = "/path/to/repo"
prompt = "Summarize file-b.txt into one sentence. Write to /tmp/summaries/b.md."
```

## When to use hierarchical mode

- You're describing a policy: "one worker per file in this directory", "one worker per unique author".
- The decomposition depends on what the lead finds when it starts running.
- You want a budget cap to protect against runaway spending.
- You want the lead to observe partial results and make decisions (e.g., spawn more workers if initial results look incomplete, or skip remaining work after a budget hit).

```toml
[run]
max_workers = 6
budget_usd = 1.50
lead_timeout_secs = 1200

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[lead]]
id = "author-digest"
directory = "/path/to/repo"
prompt = """
List the last 20 commits with `git log --format='%H %an %s' -20`. Group by author.
Spawn one worker per unique author via mcp__pitboss__spawn_worker to summarize
that author's work in /tmp/digest/<author-slug>.md. Wait for all, then compose
/tmp/digest/SUMMARY.md.
"""
```

## Rule of thumb

> If the operator can write out every `[[task]]` before running, use flat. If the operator is describing a *policy*, use hierarchical.

## When to use depth-2 sub-leads

Depth-2 sub-leads (v0.6+) add a third tier: a root lead may spawn sub-leads, each running their own workers with their own envelope and isolated coordination layer.

Use sub-leads when:
- The project decomposes into orthogonal phases that each need their own clean Claude context.
- Different phases have meaningfully different budget requirements.
- You want to prevent one phase from observing another's intermediate state.

Do not use sub-leads for every multi-worker job. Plain workers are cheaper to coordinate; sub-leads add MCP round-trips and context switching overhead. Add sub-leads only when the context isolation benefit is worth it.

See [Depth-2 sub-leads](./depth-2-subleads.md) for the full model.
