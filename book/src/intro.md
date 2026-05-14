# Pitboss

Pitboss is a Rust toolkit for running and observing **parallel Claude Code sessions**. A dispatcher (`pitboss`) fans out `claude` subprocesses under a concurrency cap, captures structured artifacts per run, and — in hierarchical mode — lets a **lead** dynamically spawn more workers via MCP. The TUI (`pitboss-tui`) gives the floor view: tile grid, live log tailing, budget and token counters.

Language models are stochastic. A well-run pit is not.

## What pitboss does

| Primitive | Description |
|-----------|-------------|
| **Flat dispatch** | Declare N tasks up front; pitboss runs them in parallel under a concurrency cap. Each task runs in its own git worktree on its own branch. |
| **Hierarchical dispatch** | Declare one lead; the lead observes the situation and dynamically spawns workers via MCP tools, under budget and worker-cap guardrails you set. |
| **Depth-2 sub-leads** | *(v0.6+)* A root lead may spawn sub-leads, each with its own envelope and isolated coordination layer. Useful for multi-phase projects that each need their own context. |
| **Container dispatch** | *(v0.8+)* `pitboss container-dispatch` runs dispatch inside a Docker/Podman container with declarative bind mounts — project directory, reference material, `~/.claude` auth. |
| **Operator control** | Cancel, pause, freeze, or reprompt workers live. Gate actions on operator approval. The TUI shows everything in real time. |
| **Structured artifacts** | Every run produces per-task logs, token usage, session ids, and a `summary.json`. Nothing disappears when the terminal closes. |

## Quick orientation

- **New to pitboss?** Start with [Install](./getting-started/install.md), then work through [Your first dispatch](./getting-started/first-dispatch.md).
- **Want to understand when to use flat vs. hierarchical mode?** See [Flat vs. hierarchical](./operator-guide/flat-vs-hierarchical.md).
- **Looking for the full manifest field reference?** See [Manifest schema](./operator-guide/manifest-schema.md).
- **Want to see it work?** The [Cookbook spotlights](./cookbook/) are runnable end-to-end examples.
- **Writing a lead that needs MCP tools?** See the [MCP Tool Reference](./mcp-reference/overview.md).
- **Processing untrusted content or running in a security-sensitive context?** See the [Security section](./security/threat-model.md), starting with the [Threat model](./security/threat-model.md) and [The Rule of Two](./security/rule-of-two.md).

## Current version

**v0.14.0** — closes out the unified envelope API (#438), persists the communication plane for resume (#282), and ships a run-wide aggregated audit log (#414). `pitboss-core::stream::open_run_stream` is now the single read path for live + historical run state across CLI, TUI, and web; subscriber-mode client handshakes let multiple read-only viewers share one run without displacing each other. Op replies persist in `events.jsonl`. Mailbox state journaled to `<run-dir>/communication/messages.jsonl` survives a crashed dispatcher. The new `pitboss audit <run-id>` CLI and `GET /api/runs/:id/audit` endpoint surface every per-actor `TaskEvent` row, chronologically merged with `actor_id` attribution and filterable by actor, kind, timestamp, or denial reason.

See [Changelog](./reference/changelog.md) for the full version history.
