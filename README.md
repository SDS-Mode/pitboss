# Agent Shire

Rust toolkit for running and observing parallel Claude Code agent sessions.
Ships two binaries:

- **`pitboss`** — headless dispatcher. Reads a `shire.toml` manifest, fans out N
  `claude` subprocesses under a concurrency cap, writes structured per-run
  artifacts. [See `crates/pitboss-cli/`.](crates/pitboss-cli/)
- **`pitboss-tui`** — terminal observer for in-progress or completed runs (v0.2-alpha).
  Tile grid of task state, live log tailing, read-only. [See `crates/pitboss-tui/`.](crates/pitboss-tui/)

## Install

```
cargo install --path crates/pitboss-cli
cargo install --path crates/pitboss-tui
```

## Quick start — dispatch

Create `shire.toml` in a directory that is inside a git repo:

```toml
[run]
max_parallel = 2

[[task]]
id = "hello"
directory = "/path/to/repo"
prompt = "Say hello in a file called hello.txt"
branch = "feat/hello"
```

Then:

```
pitboss validate shire.toml
pitboss dispatch shire.toml
```

Artifacts land in `~/.local/share/shire/runs/<run-id>/`.

## Quick start — observe

```
pitboss-tui         # opens the most recent run
pitboss-tui list    # table of runs to stdout
pitboss-tui 019d99  # opens a run by UUID prefix
```

See [`crates/pitboss-tui/README.md`](crates/pitboss-tui/README.md) for keybindings.

## Quick start — hierarchical (v0.3)

Instead of a flat list of tasks, you can declare a **lead** Hobbit that spawns
worker Hobbits on the fly via MCP. The lead decides how many workers to run,
what each one does, and waits on results — all from inside the Claude session.

```toml
[run]
max_workers = 4
budget_usd = 5.00
lead_timeout_secs = 900

[[lead]]
id = "triage"
directory = "/path/to/repo"
prompt = "Inspect recent PRs, spawn one worker per unique author, ask each worker to summarize that author's work in summary-<id>.md, then write a combined digest."
branch = "feat/triage-lead"
```

Run it the same way:

```
pitboss validate shire.toml   # prints a hierarchical summary when [[lead]] is used
pitboss dispatch shire.toml
```

The lead has access to these MCP tools: `shire__spawn_worker`,
`shire__worker_status`, `shire__wait_for_worker`, `shire__wait_for_any`,
`shire__list_workers`, `shire__cancel_worker`.

Under the hood, pitboss generates a `--mcp-config` file that points the lead's
claude subprocess at `pitboss mcp-bridge <socket>` — a small helper subcommand
that proxies stdio to the shire MCP server's unix socket. The lead never
invokes `mcp-bridge` directly; it's wired up automatically for every
hierarchical run.

In the Pitboss TUI, leads show a `[LEAD]` prefix and worker tiles display `← lead-id`
so you can see who spawned what. `pitboss resume <run-id>` works for
hierarchical runs too — it re-invokes the lead with `--resume` and the session
picks up where it left off. Only the lead is resumed; workers are not
individually resumed — the lead's next decisions determine whether to spawn
fresh workers.

Full design: [`docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md`](docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md).

## Concurrency

Default `max_parallel` is 4. Override hierarchy: manifest `[run].max_parallel`
beats `ANTHROPIC_MAX_CONCURRENT` env beats the default.

## Manual smoke test for releases

With a real `claude` binary on PATH and ANTHROPIC_API_KEY set:

1. Create two throwaway git repos.
2. Point one manifest at each with a trivial prompt ("write `hi` to a file").
3. `pitboss dispatch ./manifest.toml` — confirm the progress table updates, both
   Hobbits succeed, and the summary.json contains expected fields.
4. Run again with `halt_on_failure = true` and an intentionally-failing prompt
   in the first task. Confirm the second task is skipped.
5. Run with a long-running prompt and Ctrl-C once → drain completes; Ctrl-C
   twice → tasks report `Cancelled`.

## Development

```
cargo test --workspace
cargo test -p pitboss-core --features test-support
cargo lint
cargo tidy
```

See `docs/superpowers/specs/2026-04-16-agent-shire-design.md` for design.
