# Agent Shire

Rust toolkit for running and observing parallel Claude Code agent sessions.
Ships two binaries:

- **`shire`** — headless dispatcher. Reads a `shire.toml` manifest, fans out N
  `claude` subprocesses under a concurrency cap, writes structured per-run
  artifacts. [See `crates/shire-cli/`.](crates/shire-cli/)
- **`mosaic`** — terminal observer for in-progress or completed runs (v0.2-alpha).
  Tile grid of task state, live log tailing, read-only. [See `crates/mosaic-tui/`.](crates/mosaic-tui/)

## Install

```
cargo install --path crates/shire-cli
cargo install --path crates/mosaic-tui
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
shire validate shire.toml
shire dispatch shire.toml
```

Artifacts land in `~/.local/share/shire/runs/<run-id>/`.

## Quick start — observe

```
mosaic         # opens the most recent run
mosaic list    # table of runs to stdout
mosaic 019d99  # opens a run by UUID prefix
```

See [`crates/mosaic-tui/README.md`](crates/mosaic-tui/README.md) for keybindings.

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
shire validate shire.toml   # prints a hierarchical summary when [[lead]] is used
shire dispatch shire.toml
```

The lead has access to these MCP tools: `shire__spawn_worker`,
`shire__worker_status`, `shire__wait_for_worker`, `shire__wait_for_any`,
`shire__list_workers`, `shire__cancel_worker`.

In Mosaic, leads show a `[LEAD]` prefix and worker tiles display `← lead-id`
so you can see who spawned what. `shire resume <run-id>` works for
hierarchical runs too — it re-invokes the lead with `--resume` and the session
picks up where it left off.

Full design: [`docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md`](docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md).

## Concurrency

Default `max_parallel` is 4. Override hierarchy: manifest `[run].max_parallel`
beats `ANTHROPIC_MAX_CONCURRENT` env beats the default.

## Manual smoke test for releases

With a real `claude` binary on PATH and ANTHROPIC_API_KEY set:

1. Create two throwaway git repos.
2. Point one manifest at each with a trivial prompt ("write `hi` to a file").
3. `shire dispatch ./manifest.toml` — confirm the progress table updates, both
   Hobbits succeed, and the summary.json contains expected fields.
4. Run again with `halt_on_failure = true` and an intentionally-failing prompt
   in the first task. Confirm the second task is skipped.
5. Run with a long-running prompt and Ctrl-C once → drain completes; Ctrl-C
   twice → tasks report `Cancelled`.

## Development

```
cargo test --workspace
cargo test -p mosaic-core --features test-support
cargo lint
cargo tidy
```

See `docs/superpowers/specs/2026-04-16-agent-shire-design.md` for design.
