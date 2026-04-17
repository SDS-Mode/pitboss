# Pitboss

Rust toolkit for running and observing parallel Claude Code agent sessions.
Ships two binaries:

- **`pitboss`** — headless dispatcher. Reads a `pitboss.toml` manifest, fans
  out N `claude` subprocesses under a concurrency cap, writes structured
  per-run artifacts. Supports flat task lists and hierarchical runs where a
  *lead* claude dynamically spawns *worker* claudes via MCP.
  [See `crates/pitboss-cli/`.](crates/pitboss-cli/)
- **`pitboss-tui`** — terminal observer for in-progress or completed runs.
  Tile grid of task state, live log tailing, read-only. Renders `[LEAD]`
  prefixes and worker-to-lead arrows for hierarchical runs.
  [See `crates/pitboss-tui/`.](crates/pitboss-tui/)

Status: `v0.3.0-pre`. Exercised end-to-end against real `claude` including a
real-world practical test that documented the workspace, reviewed commits, and
surveyed neighboring projects with hierarchical fanout.

## Install

```bash
cargo install --path crates/pitboss-cli
cargo install --path crates/pitboss-tui
```

## Subcommands

```
pitboss validate <manifest>       # parse + resolve + validate, exit non-zero on error
pitboss dispatch <manifest>       # execute a manifest
pitboss resume <run-id>           # re-run a prior dispatch, reusing session ids
pitboss diff <run-a> <run-b>      # compare two runs side-by-side
pitboss version                   # print version
```

## Quick start — flat dispatch

Create `pitboss.toml` in a directory that is inside a git repo:

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

```bash
pitboss validate pitboss.toml
pitboss dispatch pitboss.toml
```

Artifacts land in `~/.local/share/pitboss/runs/<run-id>/`:
`manifest.snapshot.toml`, `resolved.json`, `meta.json`, `summary.json`,
`summary.jsonl`, and per-task `tasks/<id>/{stdout.log,stderr.log}`.

## Quick start — observe

```bash
pitboss-tui                # opens the most recent run (polls every 500ms)
pitboss-tui list           # table of runs to stdout
pitboss-tui 019d99         # opens a run by UUID prefix
```

See [`crates/pitboss-tui/README.md`](crates/pitboss-tui/README.md) for
keybindings.

## Quick start — hierarchical (v0.3)

Instead of a flat task list, declare a **lead** Hobbit that spawns workers on
the fly via MCP. The lead decides how many workers to run, what each one does,
and waits on results — all from inside the Claude session.

```toml
[run]
max_workers = 4
budget_usd = 5.00
lead_timeout_secs = 900

[[lead]]
id = "triage"
directory = "/path/to/repo"
prompt = """
Inspect recent PRs, spawn one worker per unique author, ask each worker to
summarize that author's work in summary-<id>.md, then write a combined digest.
"""
branch = "feat/triage-lead"
```

Run it the same way:

```bash
pitboss validate pitboss.toml    # prints a hierarchical summary when [[lead]] is used
pitboss dispatch pitboss.toml
```

The lead has access to these MCP tools, auto-allowed in `--allowedTools`:

| Tool | Purpose |
|---|---|
| `mcp__pitboss__spawn_worker` | Spawn a new worker with a prompt + optional directory/model/tools |
| `mcp__pitboss__worker_status` | Non-blocking peek at a worker's state |
| `mcp__pitboss__wait_for_worker` | Block until a specific worker completes |
| `mcp__pitboss__wait_for_any` | Block until any of a list of workers completes |
| `mcp__pitboss__list_workers` | Snapshot of all workers (excludes the lead) |
| `mcp__pitboss__cancel_worker` | Signal a per-worker `CancelToken` |

### Guardrails

- **`max_workers`** caps concurrent + queued workers (1–16, default unset).
- **`budget_usd`** is a soft cap with reservation accounting. Each `spawn_worker`
  call reserves a model-aware cost estimate up front; the reservation is
  released + replaced by actual cost when the worker completes. Once
  `spent + reserved + next_estimate` would exceed the budget, the next
  `spawn_worker` returns `budget exceeded`. The lead can handle the error and
  decide to continue with partial results or exit.
- **`lead_timeout_secs`** bounds the lead's wall-clock runtime.
- Depth is 1 (hub-and-spoke); workers have no MCP access and can't spawn
  sub-workers.

### Plumbing

Pitboss generates a `--mcp-config` file that points the lead's claude
subprocess at `pitboss mcp-bridge <socket>` — a small helper subcommand that
proxies stdio to the pitboss MCP server's unix socket. The lead never invokes
`mcp-bridge` directly; it's wired up automatically.

In the Pitboss TUI, leads show a `[LEAD]` prefix and worker tiles display
`← lead-id` so you can see who spawned what. The status bar shows
`— N workers spawned`.

### Resume

`pitboss resume <run-id>` works for both flat and hierarchical runs. For flat
mode, each task respawns with its original `claude_session_id`. For
hierarchical mode, only the **lead** is resumed — the lead's next decisions
determine whether to spawn fresh workers. `resolved.json` in the new run
records the `resume_session_id` so you can audit what was picked up.

Full design:
[`docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md`](docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md).

## Concurrency

Default `max_parallel` is 4. Override hierarchy: manifest `[run].max_parallel`
beats `ANTHROPIC_MAX_CONCURRENT` env beats the default.

For hierarchical mode, `max_workers` is independent of `max_parallel` — it
caps the lead's worker fanout, not the overall process count.

## Manual smoke testing

See [`docs/v0.1-smoke-test.md`](docs/v0.1-smoke-test.md) for flat-mode offline
+ live tests (10 offline + signal/drain coverage).

See [`docs/v0.3-smoke-test.md`](docs/v0.3-smoke-test.md) for hierarchical-mode
tests: schema validation, real 3-worker triage, Mosaic observation, budget
enforcement, Ctrl-C drain, and `pitboss resume` verification (~$0.50–$1.50 in
Haiku API usage).

Requires a working `claude` CLI authenticated via its normal subscription
config (no `ANTHROPIC_API_KEY` needed on systems using Claude Code login).

## Development

```bash
cargo build --workspace
cargo test --workspace --features pitboss-core/test-support    # 217 tests
cargo lint                                                     # clippy -D warnings
cargo fmt --all -- --check
```

Automated smoke scripts are in `scripts/`:

```bash
scripts/smoke-part1.sh          # 10 offline flat-mode tests (no API calls)
scripts/smoke-part3-tui.sh      # 7 non-interactive TUI tests (no API calls)
```

See `docs/superpowers/specs/` for design docs:

- `2026-04-16-pitboss-design.md` — v0.1 flat dispatcher
- `2026-04-17-hierarchical-orchestration-design.md` — v0.3 hierarchical mode
