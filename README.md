# Pitboss

> You deal the cards; the pit watches the chips.

> **Operating pitboss from an AI agent?** See [`AGENTS.md`](AGENTS.md) —
> schema reference, decision tree, canonical examples, and the rules for
> translating natural-language requests into valid manifests.

**v0.4.0** ships a live control plane: cancel/pause/continue/reprompt
and operator-approval interrupts. See CHANGELOG + AGENTS.md for
keybindings and the three new MCP tools.

Rust toolkit for running and observing parallel Claude Code sessions. A
dispatcher (`pitboss`) fans out `claude` subprocesses under a concurrency
cap, captures structured artifacts per run, and — in hierarchical mode —
lets a **lead** dynamically spawn more workers via MCP. The TUI
(`pitboss-tui`) gives the floor view: tile grid, live log tailing, budget +
token counters.

Language models are stochastic. A well-run pit is not. Give the house a
clear manifest, a budget, and a prompt — the pit turns variance into
consistently usable output.

## Vocabulary

| Term | Meaning |
|---|---|
| **Pitboss** | The `pitboss` dispatcher binary. Runs manifests, manages worktrees, persists run state. |
| **Lead** | The coordinating `claude` subprocess in a hierarchical run — receives the operator's prompt + MCP tools, decides how many workers to spawn. |
| **Run** | One invocation of `pitboss dispatch`. Produces `~/.local/share/pitboss/runs/<run-id>/` with manifest snapshot, resolved config, per-task logs, and a summary. |
| **House rules** | The hierarchical-run guardrails: `max_workers`, `budget_usd`, `lead_timeout_secs`. |

## Install

### From release (recommended)

Pre-built binaries are attached to every GitHub release.

```bash
# x86_64 Linux
mkdir -p ~/.local/bin
curl -L https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-v0.3.1-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz -C ~/.local/bin

pitboss version
pitboss-tui --version
```

The tarball contains both `pitboss` and `pitboss-tui`. Put them anywhere on
`$PATH`. Swap `x86_64-unknown-linux-gnu` for your target when more builds
land in the matrix.

### From source

```bash
git clone https://github.com/SDS-Mode/pitboss.git
cd pitboss
cargo install --path crates/pitboss-cli
cargo install --path crates/pitboss-tui
```

## Subcommands

```
pitboss validate <manifest>       parse + resolve + validate, exit non-zero on error
pitboss dispatch <manifest>       deal a run
pitboss resume <run-id>           re-deal a prior run, reusing claude_session_id
pitboss diff <run-a> <run-b>      compare two runs side-by-side
pitboss completions <shell>       print shell completion script
pitboss version                   print version
```

### Shell completions

Both binaries emit completion scripts for bash, zsh, fish, elvish, and
powershell:

```bash
# bash
pitboss completions bash       > ~/.local/share/bash-completion/completions/pitboss
pitboss-tui completions bash   > ~/.local/share/bash-completion/completions/pitboss-tui

# zsh (adjust for your $fpath)
pitboss completions zsh        > ~/.zsh/completions/_pitboss
pitboss-tui completions zsh    > ~/.zsh/completions/_pitboss-tui
```

## Quick start — flat dispatch

Deal N independent hands at once. Each `[[task]]` becomes a worker; results
land in `~/.local/share/pitboss/runs/<run-id>/`.

```toml
[run]
max_parallel = 2

[[task]]
id = "hello"
directory = "/path/to/repo"
prompt = "Say hello in a file called hello.txt"
branch = "feat/hello"
```

```bash
pitboss validate pitboss.toml
pitboss dispatch pitboss.toml
```

Each run produces: `manifest.snapshot.toml`, `resolved.json`, `meta.json`,
`summary.json`, `summary.jsonl`, and per-task `tasks/<id>/{stdout.log,stderr.log}`.

## Quick start — watch the floor

```bash
pitboss-tui                # open the most recent run (500ms polling)
pitboss-tui list           # table of runs to stdout
pitboss-tui 019d99         # open a run by UUID prefix
```

See [`crates/pitboss-tui/README.md`](crates/pitboss-tui/README.md) for
keybindings.

## Quick start — hierarchical

Flat dispatch is fixed-seat blackjack: N tables, N hands, all at once.
Hierarchical dispatch hands the lead a deck and a stake and says *deal as
many hands as you need to finish the job, within house rules*.

```toml
[run]
max_workers = 4
budget_usd = 5.00
lead_timeout_secs = 900

[[lead]]
id = "triage"
directory = "/path/to/repo"
prompt = """
Inspect recent PRs, spawn one worker per unique author, ask each to summarize
that author's work in summary-<id>.md, then write a combined digest.
"""
branch = "feat/triage-lead"
```

```bash
pitboss validate pitboss.toml    # prints a hierarchical summary when [[lead]] is set
pitboss dispatch pitboss.toml
```

The lead has these MCP tools, auto-allowed in its `--allowedTools`:

| Tool | Purpose |
|---|---|
| `mcp__pitboss__spawn_worker` | Deal a new worker with a prompt + optional directory/model/tools |
| `mcp__pitboss__worker_status` | Non-blocking peek at a worker's state |
| `mcp__pitboss__wait_for_worker` | Block until a specific worker settles |
| `mcp__pitboss__wait_for_any` | Block until any of a list of workers settles |
| `mcp__pitboss__list_workers` | Snapshot of active + completed workers |
| `mcp__pitboss__cancel_worker` | Signal a per-worker `CancelToken` |

### House rules

- **`max_workers`** — hard cap on concurrent + queued workers (1–16, default unset).
- **`budget_usd`** — the chip stack. Each spawn reserves a model-aware estimate
  up front; the reservation releases and the actual cost books in when the
  worker settles. Once `spent + reserved + next_estimate` would exceed the
  stack, `spawn_worker` returns `budget exceeded` and the lead decides what to
  do with partial results.
- **`lead_timeout_secs`** — wall-clock cap on the lead. The pit always clears.
- Depth is 1. Workers don't spawn sub-workers; no re-raise, no chaining.

### The bridge

Claude Code's MCP client only speaks stdio. The pitboss MCP server listens on
a unix socket. Between them is `pitboss mcp-bridge <socket>` — a stdio↔socket
proxy that pitboss auto-launches via the lead's generated `--mcp-config`. You
never invoke it directly.

### On the floor

In the TUI, leads render with `[LEAD] <id>` in a cyan border; workers show
`← <lead-id>` on their bottom border. The status bar reads
`— N workers spawned`. As workers complete, their tiles mark done without
clearing — full history stays visible for the run.

### Resume

`pitboss resume <run-id>` re-deals any prior run.

- **Flat**: each task respawns with its original `claude_session_id`.
- **Hierarchical**: only the lead resumes (`--resume <session-id>`); the lead
  decides whether to deal fresh workers. `resolved.json` in the new run
  records the `resume_session_id` for audit.

## Concurrency

Default `max_parallel` is 4. Override priority: `[run].max_parallel` beats
`ANTHROPIC_MAX_CONCURRENT` env beats the default.

In hierarchical mode, `max_workers` is independent of `max_parallel` — it
caps the lead's fanout, not the overall process count.

## Philosophy

The model is stochastic. The pit is not.

You cannot guarantee any single hand. You can guarantee:

- **Isolation.** Every worker runs in its own git worktree on its own branch.
  One bad hand doesn't contaminate the next.
- **Observability.** Every token, every cache hit, every session id is
  persisted. When you want to know what happened, the artifacts are on the
  table.
- **Bounded risk.** Workers, budget, and timeouts are explicit. The house
  knows its exposure before the first card is dealt.
- **Determinism where it's free.** Stream-JSON parsing, cancellation protocol,
  SQL schema migrations, UTF-8 boundary safety. If we can make it reliable
  without sacrificing capability, we do.
- **A visible floor.** The TUI never editorializes. It shows you what's
  happening; you decide what it means.

Play enough hands under these rules and the edge shows up. The pit does not
guarantee any single hand — it guarantees you can inspect it.

## Status

Last release `v0.4.0` (live control plane). Unreleased on `main`: v0.4.1
prerequisites (MCP-client fake-claude + e2e flows, `reprompt_worker` MCP
tool), the notifications plugin system (LogSink / WebhookSink / SlackSink
/ DiscordSink routed via `[[notification]]` config), and a v0.4.1.x TUI
polish pass (grid-clear, 250ms poll, word-wrap, theme module, semantic
log-line coloring, aborted-run status, refreshed legends). 335 tests pass
under `cargo test --workspace --features pitboss-core/test-support`. See
[`CHANGELOG.md`](CHANGELOG.md) for the per-version history.

## Manual smoke testing

Offline scripts are in `scripts/` (see Development below). For live
verification against real `claude`: validate a hierarchical manifest,
dispatch a 2–4 worker triage, watch the TUI annotations, tighten `budget_usd`
to force mid-run rejections, Ctrl-C a running dispatch, resume a completed
run. Budget: ~$0.50–$1.50 on Haiku for a full sweep.

Requires `claude` authenticated via its normal subscription config (no
`ANTHROPIC_API_KEY` needed on Claude Code login systems).

## Development

```bash
cargo build --workspace
cargo test --workspace --features pitboss-core/test-support    # 335 tests
cargo lint                                                     # clippy -D warnings
cargo fmt --all -- --check
```

Automated smoke scripts (no API calls):

```bash
scripts/smoke-part1.sh          # 10 offline flat-mode tests
scripts/smoke-part3-tui.sh      # 7 non-interactive TUI tests
```

### Continuous integration

`.github/workflows/ci.yml` runs on every push or PR to `main`:
`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, and
`scripts/smoke-part1.sh`. Commits that change only `CHANGELOG.md` or
`README.md` skip CI.

### Cutting a release

1. Move `[Unreleased]` items in `CHANGELOG.md` into a new
   `[X.Y.Z] — YYYY-MM-DD` section. Add the compare-link at the bottom.
2. Commit to `main`.
3. Tag and push:
   ```bash
   git tag -a vX.Y.Z -m "vX.Y.Z — <short summary>"
   git push origin vX.Y.Z
   ```

The tag push triggers `.github/workflows/release.yml`, which builds release
binaries for every target in the matrix, packages them as
`pitboss-vX.Y.Z-<target>.tar.gz`, and attaches them to the auto-created
GitHub release. Current matrix: `x86_64-unknown-linux-gnu`. Adding more
targets (macOS, Windows, aarch64) is a matrix-entry addition in the
workflow.
