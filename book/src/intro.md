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

**v0.9.2** — container subsystem grows into a real toolchain. New manifest fields `[container].extra_apt` and `[[container.copy]]`, plus the new `pitboss container-build` and `pitboss container-prune` subcommands, let operators bake apt packages and host files into a deterministically-tagged derived image and sweep the stragglers. `pitboss container-dispatch` picks up the cached tag automatically. Image cadence also fixed: the published `pitboss-with-claude` rolls forward on every main merge with `:main`, `:main-<sha>`, and `:latest` tags. Plus per-task `cost_usd` persistence on `TaskRecord`, an SSE event-stream filter on the run-detail page, and graceful shutdown for `pitboss-web stop`.

See [Changelog](./reference/changelog.md) for the full version history.
