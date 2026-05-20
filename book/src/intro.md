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

**v0.16.0** — adds **per-actor resource sampling** (#553): the dispatcher polls RSS / VSZ / CPU% per `claude` subprocess on a configurable cadence (`[run].resource_sample_secs`, default 5 s) and tracks container-cgroup memory headroom, surfaced through a new **Resources** tab in the web console with per-actor sparklines, a **"Mem peak"** column on the run list, hysteretic memory-pressure events (warn 70% / error 90% / clear 60%), a `pitboss status --resources` flag, and a finalized `summary.json::resource_high_water` aggregate. **Companion change** (#581): `pitboss container-dispatch --background` mode plus host-side `manifest.source.toml` preservation, so the Fork-manifest button on container-dispatched runs recovers the full `[container]` block and `/api/runs` auto-routes container manifests back to `container-dispatch`. Also lands a defensive serde back-compat sweep on `ControlEvent` payload structs (#566, #567, #571, #575) backed by a content-guard test (#578), MCP token revocation on actor exit (#564), and a manifest UX polish pass — validation errors gain a field-path prefix (#574), four common errors got remediation hints, and five reference-table gaps are filled (#573).

See [Changelog](./reference/changelog.md) for the full version history.
