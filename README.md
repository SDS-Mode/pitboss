# Pitboss

> **Operating pitboss from an AI agent?** See [`AGENTS.md`](AGENTS.md) —
> schema reference, decision tree, canonical examples, and the rules for
> translating natural-language requests into valid manifests.

## Documentation

Full operator guide, MCP tool reference, Security section (threat model,
defense-in-depth, Rule of Two), approval policy reference, and a cookbook
of working scenarios: **https://sds-mode.github.io/pitboss/**

To browse offline:

    cd book/
    cargo install mdbook  # one time
    mdbook serve --open

**v0.8.0** is the correctness hardening and new-capabilities release.
All 34 medium- and high-severity bugs catalogued since v0.7 are resolved.
New subcommands: `pitboss container-dispatch` (run dispatch inside a
Docker/Podman container with declarative bind mounts) and `pitboss status`
(snapshot task table for any run, in-flight or finalized). The TUI gains a
live policy editor (`P`) for editing `[[approval_policy]]` rules without
restarting. Approval TTL is now fully wired end-to-end — `ApprovalTimedOut`
correctly fires even when a TUI was connected during the approval window.
`DispatchState` no longer implements `Deref` — layer misrouting is now a
compile error. Per-sub-tree cancel cascade closes the second-Ctrl-C gap for
sub-lead workers. See `CHANGELOG.md` for the full per-version history and
`AGENTS.md` for the MCP tool reference, keybindings, and manifest schema.

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

### Via shell installer (recommended)

Pitboss releases ship through [`cargo-dist`][cargo-dist], which
produces two `curl | sh` installers per release (one per binary):

```bash
curl -LsSf https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-cli-installer.sh | sh
curl -LsSf https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-tui-installer.sh | sh

pitboss version
pitboss-tui --version
```

Each installer detects your platform, downloads the matching `tar.xz`
tarball, verifies its SHA-256, and drops the binary into `~/.cargo/bin`.
Current target matrix: `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`.

### Via Homebrew

```bash
brew install SDS-Mode/pitboss/pitboss-cli
brew install SDS-Mode/pitboss/pitboss-tui
```

Formulae are auto-published to the [`SDS-Mode/homebrew-pitboss`][tap] tap
on every release.

### Via container image

Published to GitHub Container Registry on every push to `main` and
every release tag (`linux/amd64` + `linux/arm64`):

```bash
podman pull ghcr.io/sds-mode/pitboss:latest
podman run --rm -v $(pwd)/pitboss.toml:/run/pitboss.toml \
    ghcr.io/sds-mode/pitboss:latest \
    pitboss validate /run/pitboss.toml
```

The image carries `git` (needed for worktree isolation) but NOT the
`claude` binary — mount your host's Claude Code install or build a
derived image that layers it in.

A variant image `ghcr.io/sds-mode/pitboss-with-claude` bundles a pinned Claude Code CLI (`2.1.114`) so you can run pitboss without installing claude on the host. See [Using Claude in a container](./book/src/operator-guide/using-claude-in-container.md) for auth setup and caveats.

### Direct tarball download

Prefer the tarball? Grab `pitboss-cli-<target>.tar.xz` or
`pitboss-tui-<target>.tar.xz` from the [latest release][releases]:

```bash
curl -L https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-cli-x86_64-unknown-linux-gnu.tar.xz \
  | tar xJ -C ~/.local/bin
```

[cargo-dist]: https://github.com/astral-sh/cargo-dist
[tap]: https://github.com/SDS-Mode/homebrew-pitboss
[releases]: https://github.com/SDS-Mode/pitboss/releases/latest

### From source

```bash
git clone https://github.com/SDS-Mode/pitboss.git
cd pitboss
cargo install --path crates/pitboss-cli
cargo install --path crates/pitboss-tui
```

## Subcommands

```
pitboss validate <manifest>          parse + resolve + validate, exit non-zero on error
pitboss dispatch <manifest>          deal a run
pitboss resume <run-id>              re-deal a prior run, reusing claude_session_id
pitboss attach <run-id> <task-id>    follow-mode log viewer for a single worker
pitboss diff <run-a> <run-b>         compare two runs side-by-side
pitboss completions <shell>          print shell completion script
pitboss version                      print version
```

`pitboss attach` accepts a run-id prefix (first 8 chars are plenty when
it's unique). `--raw` streams the raw stream-json jsonl; without it,
lines render like the TUI focus pane. Exits on Ctrl-C or when the
worker emits its terminal `Event::Result`.

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
| `mcp__pitboss__wait_actor` | Generalized lifecycle wait — accepts any actor id (worker or sub-lead); returns `ActorTerminalRecord` |
| `mcp__pitboss__wait_for_any` | Block until any of a list of workers settles |
| `mcp__pitboss__list_workers` | Snapshot of active + completed workers |
| `mcp__pitboss__cancel_worker` | Signal a per-worker `CancelToken`; optional `reason` delivers a synthetic reprompt to the parent |
| `mcp__pitboss__pause_worker` | Pause a worker — `mode="cancel"` (default, terminates + snapshots session) or `mode="freeze"` (SIGSTOPs the subprocess in place) |
| `mcp__pitboss__continue_worker` | Resume a paused/frozen worker (`claude --resume` or SIGCONT respectively) |
| `mcp__pitboss__reprompt_worker` | Mid-flight redirect: kill + `claude --resume <sid>` with a new prompt |
| `mcp__pitboss__request_approval` | Gate a single in-flight action on operator approval; accepts an optional typed `ApprovalPlan` |
| `mcp__pitboss__propose_plan` | Pre-flight gate: submit an execution plan for approval; required before `spawn_worker` when `[run].require_plan_approval = true` |
| `mcp__pitboss__spawn_sublead` | (v0.6+, root lead only) Spawn a sub-lead with its own envelope; requires `[lead] allow_subleads = true` |
| `mcp__pitboss__run_lease_acquire` | (v0.6+) Acquire a run-global lease for cross-sub-tree resource coordination |
| `mcp__pitboss__run_lease_release` | (v0.6+) Release a run-global lease |

Workers additionally get the 7 shared-store tools (`kv_get`, `kv_set`,
`kv_cas`, `kv_list`, `kv_wait`, `lease_acquire`, `lease_release`) for
hub-mediated coordination. See `AGENTS.md` for full schemas.

### House rules

- **`max_workers`** — hard cap on concurrent + queued workers (1–16, default unset).
- **`budget_usd`** — the chip stack. Each spawn reserves a model-aware estimate
  up front; the reservation releases and the actual cost books in when the
  worker settles. Once `spent + reserved + next_estimate` would exceed the
  stack, `spawn_worker` returns `budget exceeded` and the lead decides what to
  do with partial results.
- **`lead_timeout_secs`** — wall-clock cap on the lead. The pit always clears.
- Depth is capped at 2. Workers don't spawn sub-workers. Root leads may spawn sub-leads (v0.6+, opt-in via `allow_subleads = true`); sub-leads spawn only workers.

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

### Depth-2 sub-leads (v0.6+)

A root lead can spawn sub-leads at runtime, each with its own envelope
(budget, worker cap, timeout) and isolated coordination layer. Useful
when a project decomposes into orthogonal phases that each need their own
clean context. `spawn_sublead_session` is fully wired — sub-leads run as
real Claude subprocesses end-to-end with complete lifecycle tracking
(Cancel/Timeout/Error outcome classification, `TaskRecord` persistence,
budget reconciliation, and reprompt-loop kill+resume). See `AGENTS.md`
for the full model, manifest fields, and MCP tool schemas.

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

`v0.6.0` — depth-2 sub-leads shipped: `spawn_sublead` MCP tool, real
end-to-end subprocess lifecycle for sub-leads, `wait_actor` (generalized
lifecycle wait), `run_lease_acquire` / `run_lease_release` (run-global
leases), `cancel_worker` reason delivery, `[[approval_policy]]`
deterministic manifest rules, reject-with-reason, approval TTL +
fallback, TUI grouped grid + non-modal approval list pane, and a
dogfood test suite of six fake-claude spotlights. Builds on v0.5.0's
flagship operator-control bucket (`pitboss attach`, SIGSTOP
freeze-pause, `ApprovalPlan`, `propose_plan` gate, fake-claude e2e
harness) and the v0.4.x control plane (shared store, cancel/pause/
continue/reprompt, notifications). 536 tests pass under `cargo test
--workspace --features pitboss-core/test-support`, 0 failures. See
[`CHANGELOG.md`](CHANGELOG.md) for the full per-version history.

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
cargo test --workspace --features pitboss-core/test-support    # 536 tests
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
2. Bump `version` in the root `Cargo.toml` `[workspace.package]` block.
3. Commit to `main`.
4. Tag and push:
   ```bash
   git tag -a vX.Y.Z -m "vX.Y.Z — <short summary>"
   git push origin vX.Y.Z
   ```

The tag push triggers two workflows in parallel:

- **`.github/workflows/release.yml`** (cargo-dist generated). Builds
  `pitboss-cli-<target>.tar.xz` and `pitboss-tui-<target>.tar.xz` for
  every target in the `dist-workspace.toml` matrix
  (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `aarch64-apple-darwin`), produces shell installers + Homebrew
  formulae, and publishes to the
  [`SDS-Mode/homebrew-pitboss`][tap] tap via the
  `HOMEBREW_TAP_TOKEN` repo secret. Attaches everything to the
  auto-created GitHub release. Adding or removing a target triple is
  a one-line change to `dist-workspace.toml` followed by `dist
  generate`.
- **`.github/workflows/container.yml`**. Builds the multi-arch image
  (`linux/amd64` + `linux/arm64`) and pushes to
  `ghcr.io/sds-mode/pitboss:{version, major.minor, major, latest}`.

### Regenerating the release workflow

`dist-workspace.toml` is the source of truth for the cargo-dist matrix.
After editing it, regenerate the workflow:

```bash
cargo install cargo-dist --version 0.28.7    # one-time
dist generate
git diff .github/workflows/release.yml       # review
```
