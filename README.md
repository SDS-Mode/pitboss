# Pitboss

> You deal the cards; the pit watches the chips.

Rust toolkit for running and observing parallel Claude Code sessions. A
dispatcher (`pitboss`) fans out `claude` subprocesses under a concurrency
cap, captures structured artifacts per run, and — in hierarchical mode —
lets a **lead** dynamically open new hands via MCP. The TUI (`pitboss-tui`)
gives the floor view: tile grid, live log tailing, budget + token counters.

Language models are stochastic. A well-run pit is not. Give the house a
clear manifest, a budget, and a prompt — the pit turns variance into
consistently usable output.

## Vocabulary

| Term | Meaning |
|---|---|
| **Pitboss** | The `pitboss` dispatcher binary. Runs manifests, manages worktrees, persists run state. |
| **Dealer** | A `claude` subprocess executing a single task. Each `[[task]]` in flat mode spawns one dealer; a lead spawns them dynamically in hierarchical mode. |
| **Lead** | The first dealer in a hierarchical run — receives the operator's prompt + MCP tools, decides how many additional dealers to deal in. |
| **Run** | One invocation of `pitboss dispatch`. Produces `~/.local/share/pitboss/runs/<run-id>/` with manifest snapshot, resolved config, per-task logs, and a summary. |
| **House rules** | The hierarchical-run guardrails: `max_workers`, `budget_usd`, `lead_timeout_secs`. |

(MCP tool names like `spawn_worker` / `list_workers` keep the generic vocabulary
since they're the on-the-wire protocol. In prose and on the floor, they're dealers.)

## Install

```bash
cargo install --path crates/pitboss-cli
cargo install --path crates/pitboss-tui
```

## Subcommands

```
pitboss validate <manifest>       parse + resolve + validate, exit non-zero on error
pitboss dispatch <manifest>       deal a run
pitboss resume <run-id>           re-deal a prior run, reusing claude_session_id
pitboss diff <run-a> <run-b>      compare two runs side-by-side
pitboss version                   print version
```

## Quick start — flat dispatch

Deal N independent hands at once. Each `[[task]]` becomes a dealer; results
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
| `mcp__pitboss__spawn_worker` | Deal a new dealer with a prompt + optional directory/model/tools |
| `mcp__pitboss__worker_status` | Non-blocking peek at a dealer's state |
| `mcp__pitboss__wait_for_worker` | Block until a specific dealer settles |
| `mcp__pitboss__wait_for_any` | Block until any of a list of dealers settles |
| `mcp__pitboss__list_workers` | Snapshot of active + completed dealers |
| `mcp__pitboss__cancel_worker` | Signal a per-dealer `CancelToken` |

### House rules

- **`max_workers`** — hard cap on concurrent + queued dealers (1–16, default unset).
- **`budget_usd`** — the chip stack. Each spawn reserves a model-aware estimate
  up front; the reservation releases and the actual cost books in when the
  dealer settles. Once `spent + reserved + next_estimate` would exceed the
  stack, `spawn_worker` returns `budget exceeded` and the lead decides what to
  do with partial results.
- **`lead_timeout_secs`** — wall-clock cap on the lead. The pit always clears.
- Depth is 1. Dealers don't spawn sub-dealers; no re-raise, no chaining.

### The bridge

Claude Code's MCP client only speaks stdio. The pitboss MCP server listens on
a unix socket. Between them is `pitboss mcp-bridge <socket>` — a stdio↔socket
proxy that pitboss auto-launches via the lead's generated `--mcp-config`. You
never invoke it directly.

### On the floor

In the TUI, leads render with `[LEAD] <id>` in a cyan border; dealers show
`← <lead-id>` on their bottom border. The status bar reads
`— N workers spawned`. As dealers complete, their tiles mark done without
clearing — full history stays visible for the run.

### Resume

`pitboss resume <run-id>` re-deals any prior run.

- **Flat**: each task respawns with its original `claude_session_id`.
- **Hierarchical**: only the lead resumes (`--resume <session-id>`); the lead
  decides whether to deal fresh dealers. `resolved.json` in the new run
  records the `resume_session_id` for audit.

## Concurrency

Default `max_parallel` is 4. Override priority: `[run].max_parallel` beats
`ANTHROPIC_MAX_CONCURRENT` env beats the default.

In hierarchical mode, `max_workers` is independent of `max_parallel` — it
caps the lead's fanout, not the overall process count.

## Philosophy

The model is stochastic. The pit is not.

You cannot guarantee any single hand. You can guarantee:

- **Isolation.** Every dealer runs in its own git worktree on its own branch.
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

`v0.3.0-pre`. Crate + binary rebrand landed; hierarchical dispatch exercised
end-to-end against real `claude` including a multi-worker self-documentation
run and a budget-stress test that caught (and fixed) a spawn-burst TOCTOU in
the budget guard. 217 tests pass under
`cargo test --workspace --features pitboss-core/test-support`. See the
`feat/v0.3` branch for the full history.

## Manual smoke testing

Offline scripts are in `scripts/` (see Development below). For live
verification against real `claude`: validate a hierarchical manifest,
dispatch a 2–4 dealer triage, watch the TUI annotations, tighten `budget_usd`
to force mid-run rejections, Ctrl-C a running dispatch, resume a completed
run. Budget: ~$0.50–$1.50 on Haiku for a full sweep.

Requires `claude` authenticated via its normal subscription config (no
`ANTHROPIC_API_KEY` needed on Claude Code login systems).

## Development

```bash
cargo build --workspace
cargo test --workspace --features pitboss-core/test-support    # 217 tests
cargo lint                                                     # clippy -D warnings
cargo fmt --all -- --check
```

Automated smoke scripts (no API calls):

```bash
scripts/smoke-part1.sh          # 10 offline flat-mode tests
scripts/smoke-part3-tui.sh      # 7 non-interactive TUI tests
```
