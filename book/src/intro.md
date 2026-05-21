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

**v0.17.0** — stabilization release. **Budget enforcement correctness** (#602): closes a double-count in `compute_total_spend` for terminated sub-leads where the live watcher saw ~2× the actual run spend (a real $1.65 run tripped a $4.00 cap at $4.71 in validation); post-fix matches `summary.json::spend_breakdown.total_usd` byte-for-byte. **Parser support for extended-thinking content blocks** (#601): real Claude CLI emits `{"type":"thinking", ...}` blocks under haiku-4-5 / sonnet-4-5 / opus-4-x; these are now surfaced as `Event::AssistantThinking` (`~` prefix in `pitboss attach` / TUI), and `parse_assistant` reorders the `AssistantUsage` push so thinking-only turns no longer drop their usage data. **Parser drift detector** (#599): `tracing::warn!` on unknown stream-json `type` fields and unknown assistant content blocks — the very detector that caught the thinking-block drift on the first live run after merge. **Concurrency hardening**: six medium-severity audit findings closed (#595, #596, #597) — nested `RwLock` releases in `resolve_envelope` and hierarchical cancel-synthesis, `terminate()` now implies `drain()`, a `persistence_gap` sentinel marks `events.jsonl` holes from broadcast lag, `BudgetState.spent_usd` switches to `std::sync::Mutex` so the sync usage observer reads under contention without zero-fallback, and a rustdoc invariant pins `register_worker_cancel` as the only safe insertion path. **Resource sampling polish** (#580 FU-1-5): `[run].resource_sample_secs` bounded to ≤ 3600 s; synthetic `Clear` pressure on watcher shutdown mid-incident; `sampled_at_unix_ms` wire-time stamps on `ResourceSample` envelopes; `resource_high_water` hydration on resume; wire-compat enum guard preventing future protocol-struct back-compat regressions at compile time.

See [Changelog](./reference/changelog.md) for the full version history.
