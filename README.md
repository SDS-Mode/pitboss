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

**v0.9.2** turns the container subsystem into a real toolchain.
`pitboss container-dispatch` now reads two new manifest fields:
`[container].extra_apt` for distro packages installed at dispatch
start (the simple slow path) and `[[container.copy]]` for host
files baked into a derived image at build time. The new
`pitboss container-build` subcommand synthesizes a thin Dockerfile
from those fields and tags the result deterministically as
`pitboss-derived-<sha>:local`; subsequent dispatches pick up the
cached tag automatically and drop the apt-at-spin-up cost.
`pitboss container-prune` sweeps the stale tags that accumulate as
manifests evolve. Image cadence also gets fixed: the published
`pitboss-with-claude` rolls forward on every main merge with
`:main`, `:main-<sha>`, and `:latest` tags instead of stagnating
between release tags. Cost telemetry persists per-task
`cost_usd` on `TaskRecord`, and the run-detail page now renders
per-task and run-total cost estimates client-side. See
`CHANGELOG.md` for the full per-version history and `AGENTS.md`
for the MCP tool reference, keybindings, and manifest schema.

Rust toolkit for running and observing parallel Goose-powered agent sessions. A
dispatcher (`pitboss`) fans out `goose run` subprocesses under a concurrency
cap, captures structured artifacts per run, and — in hierarchical mode —
lets a **lead** dynamically spawn more workers via MCP. Goose owns the model
provider boundary, so one manifest can target API-key providers, local
providers, or subscription-auth CLI providers by changing `provider` and
`model`. The TUI
(`pitboss-tui`) gives the floor view: tile grid, live log tailing, budget +
token counters.

Language models are stochastic. A well-run pit is not. Give the house a
clear manifest, a budget, and a prompt — the pit turns variance into
consistently usable output.

## Vocabulary

| Term | Meaning |
|---|---|
| **Pitboss** | The `pitboss` dispatcher binary. Runs manifests, manages worktrees, persists run state. |
| **Lead** | The coordinating Goose subprocess in a hierarchical run — receives the operator's prompt + MCP tools, decides how many workers to spawn. |
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
`goose` binary — mount your host's Goose install or build a derived image
that layers it in.

A compatibility variant image `ghcr.io/sds-mode/pitboss-with-claude`
continues to exist for the Claude Code container workflow. See
[Using Claude in a container](./book/src/operator-guide/using-claude-in-container.md)
for auth setup and caveats.

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
pitboss init [output]                     emit a starter manifest TOML; --template simple|full
pitboss schema                            emit manifest-map.md or reference TOML; --format map|example
pitboss validate <manifest>               parse + resolve + validate, exit non-zero on error
pitboss tree <manifest>                   pre-flight visualization + cost gate; --check <USD>
pitboss dispatch <manifest>               deal a run; --background detaches & returns a run-id
pitboss resume <run-id>                   re-deal a prior run, reusing captured agent session ids
pitboss attach <run-id> <task-id>         follow-mode log viewer for a single worker
pitboss diff <run-a> <run-b>              compare two runs side-by-side
pitboss container-dispatch <manifest>     run dispatch inside a Docker/Podman container
pitboss status <run-id>                   snapshot task table for any run; supports --json
pitboss list                              inventory of recent runs; --active narrows to live
pitboss prune                             sweep orphaned run dirs; dry-run by default, --apply commits
pitboss agents-md                         print the bundled AGENTS.md reference
pitboss completions <shell>               print shell completion script
pitboss version                           print version
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
max_parallel_tasks = 2

[goose]
default_provider = "anthropic"

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
default_approval_policy = "auto_approve"

[lead]
id = "triage"
directory = "/path/to/repo"
provider = "anthropic"
model = "claude-haiku-4-5"
max_workers = 4
budget_usd = 5.00
lead_timeout_secs = 900
prompt = """
Inspect recent PRs, spawn one worker per unique author, ask each to summarize
that author's work in summary-<id>.md, then write a combined digest.
"""
branch = "feat/triage-lead"
```

```bash
pitboss validate pitboss.toml    # prints a hierarchical summary when [lead] is set
pitboss dispatch pitboss.toml
```

The lead has these MCP tools injected through Goose extensions. The display
name in raw streams can be provider-specific (`pitboss__list_workers` in
standard Goose streams, `mcp__pitboss__list_workers` through Claude ACP), but
the logical toolset is the same:

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
| `mcp__pitboss__continue_worker` | Resume a paused/frozen worker (`goose run --resume` or SIGCONT respectively) |
| `mcp__pitboss__reprompt_worker` | Mid-flight redirect: kill + `goose run --resume` with a new prompt |
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

### Goose Runtime

Pitboss runs every actor with `goose run --quiet --no-profile
--output-format stream-json`. Set `[goose].binary_path` to use a specific
Goose binary, `[goose].default_provider` to avoid repeating a provider on
every actor, and `[goose].default_max_turns` to apply a run-wide
`--max-turns` safety cap. Actors can override `provider` and `model`.

Provider strings are passed through to Goose. Built-in typed aliases include
`anthropic`, `openai`, `google`, `ollama`, `openrouter`, `azure`, and
`bedrock`; unknown provider strings are accepted and recorded as
`other:<name>` in rollups.

Subscription-auth providers verified against Goose 1.33.1:

| Use case | Provider | Model example | Auth path |
|---|---|---|---|
| Claude subscription | `claude-acp` | `sonnet` | Claude Code subscription login through the `claude-agent-acp` adapter |
| Codex / ChatGPT subscription | `chatgpt_codex` | `gpt-5.3-codex`, `gpt-5.4` | Codex/ChatGPT login managed by Goose |

Direct API providers still require their provider credentials. For example,
`provider = "anthropic"` uses the Anthropic API path and requires
`ANTHROPIC_API_KEY`; it does not use the Claude Code subscription login.
For Codex subscription auth, prefer Goose's built-in `chatgpt_codex` provider;
`codex-acp` requires a separate adapter binary.
Subscription-backed providers may report `cost_usd = null` because Pitboss
does not have public per-token pricing for that auth path.

### The bridge

Goose extensions speak stdio. The pitboss MCP server listens on a unix socket.
Between them is `pitboss mcp-bridge <socket>` — a stdio↔socket proxy that
pitboss auto-launches via the generated Goose `--with-extension` arguments.
You never invoke it directly.

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
real Goose subprocesses end-to-end with complete lifecycle tracking
(Cancel/Timeout/Error outcome classification, `TaskRecord` persistence,
budget reconciliation, and reprompt-loop kill+resume). See `AGENTS.md`
for the full model, manifest fields, and MCP tool schemas.

### Resume

`pitboss resume <run-id>` re-deals any prior run.

- **Flat**: each task respawns with its original `claude_session_id`.
- **Hierarchical**: only the lead resumes (`goose run --resume <session-id>`); the lead
  decides whether to deal fresh workers. `resolved.json` in the new run
  records the `resume_session_id` for audit.

The persisted field name remains `claude_session_id` for compatibility with
older summaries, but Goose-backed runs may store a Goose session name there.

## Concurrency

Default `max_parallel_tasks` is 4. Override priority:
`[run].max_parallel_tasks` beats the legacy `ANTHROPIC_MAX_CONCURRENT` env
name, which beats the default.

In hierarchical mode, `max_workers` is independent of `max_parallel_tasks` — it
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

`v0.8.0` — correctness hardening and new capabilities: `pitboss container-dispatch` (declarative bind-mount container dispatch), `pitboss status` (snapshot task table, supports `--json`), a live TUI policy editor (press `P` to edit `[[approval_policy]]` rules without restart), full `ApprovalTimedOut` TTL wiring via `BridgeEntry` (fires correctly even after a TUI has drained the approval queue), removal of `DispatchState` `Deref` (layer misrouting is now a compile error), per-sub-tree cancel cascade closing the second-Ctrl-C gap, and sub-lead resume. v0.8 also resolved all 34 medium- and high-severity bugs catalogued in the post-v0.7 audit cycle.

v0.7 added the Path A permission default, the bundled `pitboss-with-claude` container variant, `ApprovalRejected` terminal status, and `pitboss agents-md`; v0.6 added depth-2 sub-leads (`spawn_sublead`, `wait_actor`), run-global leases, `[[approval_policy]]` rules with TTL and fallback. See [`CHANGELOG.md`](CHANGELOG.md) for the full per-version history.

## Manual smoke testing

Offline scripts are in `scripts/` (see Development below). For live
verification against real providers: validate a hierarchical manifest,
dispatch a 2–4 worker triage, watch the TUI annotations, tighten `budget_usd`
to force mid-run rejections, Ctrl-C a running dispatch, resume a completed
run. Budget depends on the selected Goose provider and model.

For subscription-auth smoke tests, use `provider = "claude-acp"` with
`model = "sonnet"` for Claude Code subscription auth through
`claude-agent-acp`, or
`provider = "chatgpt_codex"` with `model = "gpt-5.3-codex"` for Codex /
ChatGPT subscription auth. For API-key providers, make sure the corresponding
provider key is present in the environment Goose will inherit.

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
