# Pitboss for agents

Instructions for an AI agent (Claude, GPT, whatever) operating `pitboss` on
behalf of a human. If you're a human: read `README.md`. If you're an agent
that needs to orchestrate pitboss from natural language, stay here.

---

## Mission

Pitboss is a Rust dispatcher that runs multiple `claude` subprocesses in
parallel under a concurrency cap and captures structured artifacts per run.
It has two modes:

- **Flat**: the operator predeclares N tasks; pitboss runs them.
- **Hierarchical**: the operator declares one **lead**; the lead dynamically
  spawns **worker** subprocesses via MCP tool calls, under house rules the
  operator set.

You invoke pitboss from a shell. You do not need to touch rust source to
use it. You write a TOML manifest, validate it, dispatch it, read the run
directory.

---

## When to reach for pitboss (decision tree)

Pitboss is the right tool when **all** of the following hold:

1. The task decomposes into **≥ 2 units** that could run in parallel.
2. Each unit is substantial enough to justify a fresh claude subprocess
   (order of ≥30 seconds of work), *not* a one-liner the caller could do
   inline.
3. You want **isolated git worktrees** per unit (or you've set
   `use_worktree = false` and accepted the shared-directory tradeoff).
4. You want **structured artifacts** — per-task logs, token usage, session
   ids, summary.json — not just "the output scrolled by in a terminal."
5. The wall-clock win from parallelism beats the setup cost (~1-2 seconds
   per worker for process spawn + worktree prep).

Anti-patterns — **don't use pitboss** for:
- Single-shot work. One claude call is simpler than writing a manifest.
- Tightly coupled work where units must communicate mid-execution. Pitboss
  workers cannot message each other (by design).
- Work where the operator needs to inspect intermediate state
  interactively. Pitboss runs are batch; the TUI is read-only.
- Deep recursion. Depth is 1 (hub-and-spoke). Workers cannot spawn
  sub-workers.

---

## Flat vs hierarchical — which one

| | Flat | Hierarchical |
|---|---|---|
| **When you know the decomposition up front** | ✓ | |
| **When the decomposition depends on the input** | | ✓ |
| **Manifest declares every task statically** | ✓ | |
| **Lead observes + reacts to intermediate results** | | ✓ |
| **Number of workers** | fixed | dynamic, bounded by `max_workers` |
| **Budget enforcement** | no | yes, `budget_usd` |
| **MCP server runs** | no | yes, on a unix socket |

Rule of thumb: **if the operator can write out every `[[task]]` before
running, use flat. If the operator is describing a *policy* (e.g., "one
worker per file in this directory", "one worker per unique author"), use
hierarchical.**

---

## Vocabulary

| Term | Meaning |
|---|---|
| **Pitboss** | The `pitboss` binary you invoke. |
| **Run** | One `pitboss dispatch` invocation. Produces `~/.local/share/pitboss/runs/<run-id>/`. |
| **Lead** | In hierarchical mode, the first claude subprocess. Receives the operator's prompt + six MCP tools. Decides how many workers to spawn. |
| **Worker** | A claude subprocess executing a single task, either declared in `[[task]]` (flat) or dynamically spawned by the lead (hierarchical). |
| **House rules** | Hierarchical guardrails: `max_workers` (≤16), `budget_usd`, `lead_timeout_secs`. |
| **Worktree** | A per-task git worktree under a fresh branch, isolating concurrent work. `use_worktree = true` by default. |

---

## Manifest schema

TOML, typically named `pitboss.toml`. Every field annotated below.

### Top-level `[run]`

| Key | Type | Required? | Default | Notes |
|---|---|---|---|---|
| `max_parallel` | int | no | 4 | Concurrency cap for flat-mode tasks. Overridden by `ANTHROPIC_MAX_CONCURRENT` env. |
| `halt_on_failure` | bool | no | false | Flat mode. If a task fails, skip remaining tasks. |
| `run_dir` | string path | no | `~/.local/share/pitboss/runs` | Where per-run artifacts land. |
| `worktree_cleanup` | `"always"` \| `"on_success"` \| `"never"` | no | `"on_success"` | What to do with each worker's worktree after completion. `"never"` for inspection-heavy runs. |
| `emit_event_stream` | bool | no | false | Emit a JSONL event stream alongside summary.jsonl. |
| `max_workers` | int | only if `[[lead]]` present | unset | Hierarchical: hard cap on concurrent + queued workers (1–16). |
| `budget_usd` | float | only if `[[lead]]` present | unset | Hierarchical: soft cap with reservation accounting. Worker spawns fail with `budget exceeded` once `spent + reserved + next_estimate > budget`. |
| `lead_timeout_secs` | int | only if `[[lead]]` present | 3600 (fallback) | Hierarchical: wall-clock cap on the lead. No upper bound — set generously for multi-hour orchestration plans (e.g. `21600` for a 6-hour plan executor). The 3600s fallback is tuned for single-task leads, not plan drivers. |

### `[defaults]`

Inherited by every `[[task]]` and `[[lead]]` unless overridden.

| Key | Type | Notes |
|---|---|---|
| `model` | string | e.g. `claude-haiku-4-5`, `claude-sonnet-4-6`, `claude-opus-4-7`. Dated suffixes allowed. |
| `effort` | `"low"` \| `"medium"` \| `"high"` | Maps to claude's `--effort` flag. |
| `tools` | array of string | `--allowedTools` value. Defaults to `["Read", "Write", "Edit", "Bash", "Glob", "Grep"]` if unset. |
| `timeout_secs` | int | Per-task wall-clock cap. |
| `use_worktree` | bool | Default `true`. Set `false` for read-only analysis runs. |
| `env` | table (string → string) | Env vars passed to the claude subprocess. |

### `[[task]]` (flat mode, repeat)

| Key | Required? | Notes |
|---|---|---|
| `id` | yes | Short slug used in logs, worktree names. Alphanumeric + `_` + `-`. Unique within manifest. |
| `directory` | yes | Must be inside a git repo if `use_worktree = true`. |
| `prompt` | yes | What the claude subprocess receives via `-p`. |
| `branch` | no | Branch name for the worktree. Defaults to a generated name. |
| `model`, `effort`, `tools`, `timeout_secs`, `use_worktree`, `env` | no | Per-task overrides of `[defaults]`. |

### `[[lead]]` (hierarchical mode, exactly one, mutually exclusive with `[[task]]`)

Same fields as `[[task]]`. `id` is used as the tile label in the TUI.
Mutually exclusive with `[[task]]` — a manifest is either flat or
hierarchical.

---

## Invocation patterns

### Validate before dispatch

```bash
pitboss validate pitboss.toml
```

Exit 0 = valid. Non-zero = parse error or semantic error. **Always validate
first.** This catches all the class-of-error issues (mixed `[[task]]` + `[[lead]]`,
`max_workers = 17`, `budget_usd = 0`, missing `id`, directory doesn't exist)
before any claude subprocess is spawned.

### Dispatch

```bash
pitboss dispatch pitboss.toml
```

Blocks until all tasks finish. Exit codes:
- `0` — all tasks succeeded
- `1` — one or more tasks failed (but pitboss itself ran cleanly)
- `2` — manifest error, claude binary missing, etc.
- `130` — interrupted (Ctrl-C drained gracefully)

### Resume

```bash
pitboss resume <run-id>
```

Re-runs a prior dispatch. For flat-mode runs, each task respawns with its
original `claude_session_id`. For hierarchical runs, only the lead resumes
(`--resume <session-id>`); the lead decides whether to spawn fresh workers.

**Gotcha:** if the original run used `worktree_cleanup = "on_success"` (the
default), the worktrees are gone — claude can't find its sessions by cwd.
Use `worktree_cleanup = "never"` on runs you know you want to resume.

### Diff

```bash
pitboss diff <run-a> <run-b>
```

Compares two runs side-by-side. Useful for A/B testing prompts or models.

---

## Interpreting a run directory

After `pitboss dispatch` finishes, find the run via:

```bash
RUN_DIR=$(ls -td ~/.local/share/pitboss/runs/*/ | head -1)
```

Files in the run dir:

| File | Purpose |
|---|---|
| `manifest.snapshot.toml` | Exact manifest bytes used for this run. |
| `resolved.json` | Fully resolved manifest (defaults applied). |
| `meta.json` | `run_id`, `started_at`, `claude_version`, `pitboss_version`. |
| `summary.json` | Written on clean finalize. Full structured summary of the run. |
| `summary.jsonl` | Appended incrementally as tasks finish. Useful for live observation. |
| `tasks/<id>/stdout.log` | Raw stream-json from the task's claude subprocess. |
| `tasks/<id>/stderr.log` | Stderr. |
| `lead-mcp-config.json` | Hierarchical only. The `--mcp-config` file pointed at `pitboss mcp-bridge <socket>`. |

### `summary.json` structure

```json
{
  "run_id": "019d9b...",
  "started_at": "2026-04-17T12:14:22Z",
  "ended_at":   "2026-04-17T12:14:55Z",
  "total_duration_ms": 32654,
  "tasks_total": 4,
  "tasks_failed": 0,
  "was_interrupted": false,
  "pitboss_version": "0.3.3",
  "claude_version":  "2.1.112 (Claude Code)",
  "tasks": [
    {
      "task_id": "triage",
      "status": "Success" | "Failed" | "TimedOut" | "Cancelled" | "SpawnFailed",
      "exit_code": 0,
      "started_at": "...",
      "ended_at":   "...",
      "duration_ms": 22649,
      "worktree_path": "/path/to/worktree-or-null",
      "log_path":      "/path/to/tasks/triage/stdout.log",
      "token_usage": {
        "input": 26,
        "output": 1373,
        "cache_read":     72647,
        "cache_creation": 37413
      },
      "claude_session_id": "...",
      "final_message_preview": "Done. Composed summary...",
      "parent_task_id": null | "lead-id"
    }
  ]
}
```

Lead records have `parent_task_id: null`. Worker records have
`parent_task_id: "<lead-id>"`. Query with `jq`.

---

## The 6 MCP tools the lead has

When running hierarchical, the lead's `--allowedTools` is automatically
populated with these. You (the operator) don't list them explicitly.

| Tool | Args | Returns |
|---|---|---|
| `mcp__pitboss__spawn_worker` | `{prompt, directory?, branch?, tools?, timeout_secs?, model?}` | `{task_id, worktree_path}` |
| `mcp__pitboss__worker_status` | `{task_id}` | `{state, started_at, partial_usage, last_text_preview, prompt_preview}` |
| `mcp__pitboss__wait_for_worker` | `{task_id, timeout_secs?}` | full `TaskRecord` when worker settles |
| `mcp__pitboss__wait_for_any` | `{task_ids: [...], timeout_secs?}` | `(winner_id, TaskRecord)` on first settle |
| `mcp__pitboss__list_workers` | `{}` | `[{task_id, state, prompt_preview, started_at}, ...]` |
| `mcp__pitboss__cancel_worker` | `{task_id}` | `{ok: bool}` |

**Worker spawn arg rules:**
- `prompt` is the new worker's system prompt / `-p` payload. Required.
- `directory` defaults to the lead's `directory`.
- `model` defaults to the lead's model. Override per-worker when you want
  heavier workers (Sonnet) under a Haiku lead.
- `tools` defaults to the lead's tools.

### `mcp__pitboss__pause_worker`

Pause a running worker. Snapshots its `claude_session_id` so
`continue_worker` can resume. Args: `{task_id: string}`.
Fails if worker is not in `Running` state with an initialized session.

### `mcp__pitboss__continue_worker`

Continue a previously-paused worker. Spawns `claude --resume <id>`
under the hood. Args: `{task_id: string, prompt?: string}` (default
prompt "continue").

### `mcp__pitboss__request_approval`

Block the lead until the operator approves, rejects, or edits.
Args: `{summary: string, timeout_secs?: number}`. Returns
`{approved: bool, comment?: string, edited_summary?: string}`.
Policy-gated: see `approval_policy` below.

---

## Error patterns

### `budget exceeded: $<spent> spent + $<reserved> reserved + $<estimated> estimated > $<budget> budget`

The lead tried `spawn_worker` with insufficient budget headroom. Two
reactions:
1. **Fall back gracefully.** Finish the work the lead already has, compose
   a partial report, note the budget hit in the final output.
2. **Request a larger budget from the operator.** If you're orchestrating
   pitboss from natural language, surface this error back to the human and
   ask whether to re-run with a higher cap.

Don't loop calling `spawn_worker` after budget exhaustion — each call costs
nothing structurally but adds noise to the logs.

### `worker cap reached: N active (max M)`

More than `max_workers` workers are Pending/Running. Wait for one to finish
(via `wait_for_worker` or `wait_for_any`), then retry the spawn.

### `run is draining: no new workers accepted`

The operator Ctrl-C'd or the lead's `cancel` was tripped. Finish gracefully;
don't spawn new work.

### `unknown task_id`

You're referring to a worker that was never spawned (or was typo'd).
`mcp__pitboss__list_workers` shows what's actually registered.

### `SpawnFailed`

A worker never started — usually a git worktree prep failure (dirty tree,
branch conflict, non-git directory). Check the stderr log.

---

## Operator keybindings (pitboss-tui, v0.4.0+)

Navigation / views:
- `h j k l` / arrows — navigate tiles
- `Enter` — snap-in to focused tile (full-screen log)
- `L` — log overlay for focused tile
- `o` — run picker (switch to another run)
- `?` — help overlay (full keybinding reference)
- `q` / `Ctrl-C` — quit
- `Esc` — close any overlay / modal

Control plane:
- `x` — confirm+cancel focused worker
- `X` — confirm+cancel entire run
- `p` — pause focused worker (requires initialized session)
- `c` — continue paused worker
- `r` — open reprompt textarea (Ctrl+Enter to submit, Esc to cancel)
- During approval modal: `y` approve, `n` reject (with comment), `e`
  edit (Ctrl+Enter to submit, Esc to cancel)

## `[run].approval_policy`

Controls handling of `request_approval` calls when no TUI is attached.

- `block` (default) — queue until a TUI connects, or fail after
  `lead_timeout_secs`.
- `auto_approve` — immediate `{approved: true}`.
- `auto_reject` — immediate `{approved: false, comment: "no operator
  available"}`.

---

## Canonical examples

### 1. Fan out a summarization across N files

Flat mode, predeclared tasks.

```toml
[run]
max_parallel = 3

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[task]]
id = "summarize-a"
directory = "/path/to/repo"
prompt = "Read file-a.txt and summarize in one sentence to /tmp/summaries/a.md"

[[task]]
id = "summarize-b"
directory = "/path/to/repo"
prompt = "Read file-b.txt and summarize in one sentence to /tmp/summaries/b.md"

# ... etc
```

### 2. Lead decides fanout based on the input

Hierarchical mode, dynamic decomposition.

```toml
[run]
max_workers = 6
budget_usd = 1.50
lead_timeout_secs = 1200

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[lead]]
id = "author-digest"
directory = "/path/to/repo"
prompt = """
List the last 20 commits with `git log --format='%H %an %s' -20`. Group
them by author. Spawn one worker per unique author via
mcp__pitboss__spawn_worker to summarize that author's work in
/tmp/digest/<author-slug>.md. Wait for all via mcp__pitboss__wait_for_worker.
Compose a combined /tmp/digest/SUMMARY.md. Then exit.
"""
```

### 3. Refactor analysis of a neighboring repo

This pattern was exercised end-to-end against the
[Ketchup](https://github.com/SDS-Mode/ketchup) plugin on
2026-04-17 (branch
[`feature/pitboss-refactor-analysis`](https://github.com/SDS-Mode/ketchup/tree/feature/pitboss-refactor-analysis)).
4 Haiku workers ran in parallel auditing one angle each — SKILL.md
structure, 16 CLI parsing rules, cross-file overlap, README-vs-SKILL.md
separation — and a Haiku lead synthesized into a single `REFACTOR-PLAN.md`
with 11 prioritized changes (5 P0, 4 P1, 2 P2) and a ~12% token-footprint
reduction estimate. **Total: 5 tasks, 0 failed, 201 s wall-clock, under
$0.40 on Haiku.**

Sketch of the manifest:

```toml
[run]
max_workers = 4
budget_usd = 1.50
lead_timeout_secs = 1500
worktree_cleanup = "never"

[defaults]
model = "claude-haiku-4-5"
use_worktree = false        # read-only audit — no worktree isolation needed

[[lead]]
id = "refactor-analyst"
directory = "/path/to/target-repo"
prompt = """
[...concrete directions for spawning 4 workers, one per audit angle,
each writing to /tmp/refactor/<angle>.md; then reading them back and
synthesizing into /tmp/refactor/REFACTOR-PLAN.md...]
"""
```

The lead spawned workers in an explicit loop, used
`mcp__pitboss__wait_for_worker` on each before reading their outputs,
then composed the synthesis. Key patterns the lead applied:

- Each worker received a **specific file path** to write to, so the lead
  could read the results back deterministically.
- Workers received **per-angle instructions**, not the whole repo — keeps
  their context tight and their output focused.
- The lead's synthesis prompt explicitly asked for *executive summary,
  before/after file structure, prioritized list, risk section* — giving
  the output a predictable shape.

Run this pattern against any repo of similar shape by:
1. Listing the 3–5 files that matter most.
2. Choosing 4 audit angles (structural / rules / overlap / user-vs-internal
   / test-coverage / dependencies — pick what applies).
3. Writing worker prompts that each produce one focused analysis file.
4. Writing a lead-synthesis prompt that reads those files back and
   composes an actionable plan.

### 4. Tight-budget stress / graceful degradation

```toml
[run]
max_workers = 8
budget_usd = 0.20
lead_timeout_secs = 900

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[lead]]
id = "partial"
directory = "/path/to/repo"
prompt = """
Attempt to spawn 6 workers with mcp__pitboss__spawn_worker, one per file in
src/. When a spawn fails with 'budget exceeded', DO NOT retry — record the
file and move on. Wait for successfully-spawned workers, then compose a
partial summary noting which files were skipped and why.
"""
```

Use this pattern when you want to explore *what you can get* within a fixed
spend envelope.

---

## Writing manifests from natural-language requests

When a human asks you (the agent) to "run claude on each X in Y and
combine the results", the canonical translation is:

1. Can I enumerate the Xs up front? → flat mode, one `[[task]]` per X.
2. Do I need to compute the list of Xs first? → hierarchical mode, lead
   does the enumeration then spawns workers.
3. Is the user ok with a worst-case budget? → put it in `budget_usd`.
4. Does the work need git worktree isolation, or is it read-only? → set
   `use_worktree` accordingly.
5. What model? Default to Haiku unless the work is substantial (deep code
   analysis, multi-file refactor proposals) — then Sonnet.

Write the manifest, run `pitboss validate`, show the human the manifest
and the validation result, ask for confirmation, then dispatch. If you
dispatch first and have to ask follow-ups, you've probably wasted budget.

---

## Version

Written for pitboss `v0.3.3`. Schema may evolve; `pitboss validate` is the
source of truth. This document should stay self-contained — if something
here conflicts with the actual binary, the binary wins. File a PR.
