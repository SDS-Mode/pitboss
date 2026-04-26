---
document: pitboss-agent-instructions
schema_version: 1
pitboss_version: 0.8.0
last_updated: 2026-04-24
audience: ai-agent
canonical_url: https://github.com/SDS-Mode/pitboss/blob/main/AGENTS.md
---

# Pitboss for agents

Instructions for an AI agent (Claude, GPT, whatever) operating `pitboss` on
behalf of a human. If you're a human: read `README.md`. If you're an agent
that needs to orchestrate pitboss from natural language, stay here.

> **Agents:** the YAML frontmatter above is a stable machine-readable identity
> block. Filter on `document: pitboss-agent-instructions` + `pitboss_version`
> to decide whether this applies to the binary you're orchestrating. If
> `pitboss --version` disagrees with `pitboss_version` above, trust the
> binary — regenerate the manifest against its actual schema via
> `pitboss validate`.

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
- Deep recursion. Max depth is 2 (root lead → sub-leads → workers).
  Workers cannot spawn anything. Sub-leads are available via `spawn_sublead`
  when `allow_subleads = true`; use them for orthogonal phases, not for
  general recursion.

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
| **Lead** | In hierarchical mode, the first claude subprocess. Receives the operator's prompt + the full MCP orchestration toolset. Decides how many workers to spawn. |
| **Worker** | A claude subprocess executing a single task, either declared in `[[task]]` (flat) or dynamically spawned by the lead (hierarchical). |
| **House rules** | Hierarchical guardrails: `max_workers` (≤16), `budget_usd`, `lead_timeout_secs`. For depth-2 runs: also `max_subleads`, `max_sublead_budget_usd`, `max_total_workers`. |
| **Worktree** | A per-task git worktree under a fresh branch, isolating concurrent work. `use_worktree = true` by default. |

---

## Manifest schema

TOML, typically named `pitboss.toml`. Every field annotated below.

> **v0.9 schema** — collapses the v0.8 `[[lead]]`/`[lead]` split into one
> canonical `[lead]` (single-table) form, moves lead-level caps off `[run]`
> and onto `[lead]`, promotes `[lead.sublead_defaults]` to top-level
> `[sublead_defaults]`, and renames a few fields for consistency. See the
> `Migration from v0.8 → v0.9` table at the bottom of this section. Pre-v0.9
> manifests are rejected; `pitboss validate` provides per-field migration
> guidance.

### Top-level `[run]` (run-wide infrastructure config)

`[run]` carries settings that apply to the whole dispatch run. Lead-level
caps (which used to live here in v0.8) moved to `[lead]` in v0.9.

| Key | Type | Required? | Default | Notes |
|---|---|---|---|---|
| `max_parallel_tasks` | int | no | 4 | Concurrency cap for flat-mode `[[task]]` runs. Overridden by `ANTHROPIC_MAX_CONCURRENT` env. Renamed from `max_parallel` in v0.9. |
| `halt_on_failure` | bool | no | false | Flat mode. If a task fails, skip remaining tasks. |
| `run_dir` | string path | no | `~/.local/share/pitboss/runs` | Where per-run artifacts land. |
| `worktree_cleanup` | `"always"` \| `"on_success"` \| `"never"` | no | `"on_success"` | What to do with each worker's worktree after completion. `"never"` for inspection-heavy runs. |
| `emit_event_stream` | bool | no | false | Emit a JSONL event stream alongside summary.jsonl. |
| `default_approval_policy` | `"block"` \| `"auto_approve"` \| `"auto_reject"` | no | `"block"` | Hierarchical: default action for `request_approval` / `propose_plan` when no TUI is attached and no `[[approval_policy]]` rule matches. Renamed from `approval_policy` in v0.9 to disambiguate from the rules array. |
| `require_plan_approval` | bool | no | false | Hierarchical: when true, `spawn_worker` refuses until a plan submitted via `propose_plan` has been operator-approved. |
| `dump_shared_store` | bool | no | false | Hierarchical: at run finalize, write `shared-store.json` into the run dir for post-mortem inspection. |

### `[[notification]]` sinks (v0.4.1+)

Optional notification sinks. Multiple blocks allowed, one per sink.

| Key | Required? | Notes |
|---|---|---|
| `kind` | yes | `"log"`, `"webhook"`, `"slack"`, `"discord"` |
| `url` | for webhook/slack/discord | Endpoint; `${PITBOSS_VAR}` substitution supported (v0.7.1+: only `PITBOSS_`-prefixed env vars may be substituted) |
| `events` | no | Filter by event category. Defaults to all. |
| `severity_min` | no | Minimum severity to fire (`info` / `warning` / `error` / `critical`). |

Event categories:

| Category | Fires when |
|---|---|
| `"approval_request"` | A `request_approval` or `propose_plan` call is queued for operator action |
| `"approval_pending"` | An approval enqueues and awaits operator action (v0.6+); use for alerting when a run is blocked |
| `"run_finished"` | A run reaches a terminal state |
| `"budget_exceeded"` | A `spawn_worker` call is rejected due to budget exhaustion |

Example — Slack alert on blocked approvals:

```toml
[[notification]]
kind = "slack"
url = "${PITBOSS_SLACK_WEBHOOK_URL}"
events = ["approval_pending", "run_finished"]
```

### `[defaults]`

Inherited by every `[[task]]` and `[lead]` unless overridden.

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

### `[lead]` (hierarchical mode, exactly one, mutually exclusive with `[[task]]`)

`[lead]` is a single-table block (no array form — the v0.8 `[[lead]]` array
form was removed in v0.9). `id` is used as the tile label in the TUI.

> **Important:** `prompt =` must appear **before** any subtable declaration
> (e.g. `[lead.env]`) in the TOML source. A `prompt =` key that appears
> after a subtable header is parsed into that subtable's scope and silently
> dropped; `pitboss validate` catches this and reports `"prompt is required
> but is empty"`.

Required and per-actor fields:

| Key | Type | Required? | Notes |
|---|---|---|---|
| `id` | string | yes | Short slug used in logs, worktree names, TUI tiles. Alphanumeric + `_` + `-`. |
| `directory` | string path | yes | Working dir for the lead's claude subprocess. Must be a git work-tree if `use_worktree = true`. |
| `prompt` | string | yes | Operator instructions passed via `-p`. Must come before any `[lead.X]` subtable. |
| `branch` | string | no | Branch name for the lead's worktree. Auto-generated if omitted. |
| `model`, `effort`, `tools`, `timeout_secs`, `use_worktree`, `env` | various | no | Per-lead overrides of `[defaults]`. |

Lead-level caps (moved from `[run]` in v0.9 — they're properties of the
lead, not the run):

| Key | Type | Default | Notes |
|---|---|---|---|
| `max_workers` | int | unset | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. |
| `budget_usd` | float | unset | Soft cap with reservation accounting. `spawn_worker` fails with `budget exceeded` once `spent + reserved + next_estimate > budget`. |
| `lead_timeout_secs` | int | 3600 fallback | Wall-clock cap on the lead session. No upper bound — set generously for multi-hour orchestration plans. |

Depth-2 controls (sub-leads):

| Key | Type | Default | Notes |
|---|---|---|---|
| `allow_subleads` | bool | `false` | Expose `spawn_sublead` in the root lead's `--allowedTools`. Required to enable depth-2. |
| `max_subleads` | int | unset | Cap on total sub-leads the root lead may spawn. |
| `max_sublead_budget_usd` | float | unset | Per-sub-lead envelope cap; `spawn_sublead` rejects envelopes exceeding this. |
| `max_total_workers` | int | unset | Cap on total live workers including all sub-tree workers. Renamed from `max_workers_across_tree` in v0.9. |
| `permission_routing` | `"path_a"` \| `"path_b"` | `"path_a"` | `"path_a"` sets `CLAUDE_CODE_ENTRYPOINT=sdk-ts` so pitboss is the sole permission authority. `"path_b"` routes claude's built-in gate through pitboss's approval queue — rejected at validate time until stabilization (issues #92–#94). |

### Top-level `[sublead_defaults]` (v0.9+, promoted from `[lead.sublead_defaults]`)

Optional defaults applied to `spawn_sublead` calls that omit the
corresponding parameters. Top-level in v0.9 — the v0.8 nested
`[lead.sublead_defaults]` form is gone.

```toml
[sublead_defaults]
budget_usd = 2.00
max_workers = 4
lead_timeout_secs = 1800
read_down = false
```

| Key | Type | Notes |
|---|---|---|
| `budget_usd` | float | Per-sub-lead envelope when `read_down = false`. |
| `max_workers` | int | Per-sub-lead worker pool when `read_down = false`. |
| `lead_timeout_secs` | int | Wall-clock cap for the sub-lead session. |
| `read_down` | bool | When true, the sub-lead shares the root's budget and worker pool instead of carving its own envelope. |

### `[container]` (v0.8+)

The `[container]` section enables `pitboss container-dispatch`, which assembles and execs a Docker/Podman run command from the manifest. Task and lead `directory` fields are interpreted as container-side paths when `[container]` is present.

| Key | Type | Default | Notes |
|---|---|---|---|
| `image` | string | `ghcr.io/sds-mode/pitboss-with-claude:latest` | Container image to run. |
| `runtime` | `"docker"` \| `"podman"` \| `"auto"` | `"auto"` | Runtime selector. `"auto"` prefers podman when available. |
| `extra_args` | array of string | `[]` | Inserted verbatim before the image name in the assembled `run` invocation. |
| `workdir` | string | first mount's container path, else `/home/pitboss` | Working directory inside the container. |

#### `[[container.mount]]`

| Key | Required? | Notes |
|---|---|---|
| `host` | yes | Absolute host path. Tilde (`~`) is expanded. |
| `container` | yes | Absolute path inside the container. |
| `readonly` | no | Default `false`. |

Two mounts are always auto-injected: `~/.claude → /home/pitboss/.claude` (OAuth) and the run artifact directory; the manifest itself is injected at `/run/pitboss.toml` read-only.

### `[[mcp_server]]` (v0.9+)

Declare external MCP servers to inject into **every actor's** `--mcp-config` (lead, sub-leads, and workers). This is the native alternative to the KV-bridge workaround for giving all actors access to tools like context7.

```toml
[[mcp_server]]
id      = "context7"
command = "npx"
args    = ["-y", "@upstash/context7-mcp"]

[[mcp_server]]
id      = "my-tool"
command = "/usr/local/bin/my-mcp-server"
args    = ["--port", "3000"]
env     = { MY_TOKEN = "abc" }
```

| Key | Required? | Notes |
|---|---|---|
| `id` | yes | Key name in the generated `mcpServers` JSON. Must be unique within the manifest. |
| `command` | yes | Executable to launch (e.g. `"npx"`, `"uvx"`, absolute path). |
| `args` | no | Arguments passed to the command. Default `[]`. |
| `env` | no | Environment variables injected into the server process. Default `{}`. |

All declared servers are injected into all actors (scope = all). Per-actor scoping is deferred — see roadmap.

**Tools from injected servers are available immediately** — no additional `--allowedTools` configuration is needed; claude's MCP client discovers the tools from the server at startup.

### Migration from v0.8 → v0.9

Pre-v0.9 manifests are rejected. `pitboss validate` scans for the migration
patterns below and emits guidance:

| v0.8 form | v0.9 form |
|---|---|
| `[[lead]]` (array) | `[lead]` (single-table) |
| `[run].max_workers` | `[lead].max_workers` |
| `[run].budget_usd` | `[lead].budget_usd` |
| `[run].lead_timeout_secs` | `[lead].lead_timeout_secs` |
| `[run].max_parallel` | `[run].max_parallel_tasks` |
| `[run].approval_policy` | `[run].default_approval_policy` |
| `[lead].max_workers_across_tree` | `[lead].max_total_workers` |
| `[lead.sublead_defaults]` | top-level `[sublead_defaults]` |
| `[lead].id` and `[lead].directory` optional | both required |

In-flight runs (`resolved.json` snapshots in run-dirs) remain readable —
`#[serde(alias)]` on the renamed `ResolvedManifest` fields preserves resume.

---

## Invocation patterns

### Validate before dispatch

```bash
pitboss validate pitboss.toml
```

Exit 0 = valid. Non-zero = parse error or semantic error. **Always validate
first.** This catches all the class-of-error issues (mixed `[[task]]` + `[lead]`,
`max_workers = 17`, `budget_usd = 0`, missing `id`, directory doesn't exist,
pre-v0.9 schema usage) before any claude subprocess is spawned.

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

### Attach (v0.5.0+)

```bash
pitboss attach <run-id> <task-id>
pitboss attach <run-id> <task-id> --raw            # stream raw stream-json
pitboss attach <run-id> <task-id> --lines 200      # larger backfill
```

Follow-mode log viewer for a single worker. Run-id is resolved by
prefix; first 8 chars are plenty when unique. Formatted output matches
the TUI focus pane; `--raw` dumps the underlying jsonl. Exits on
Ctrl-C or when the worker emits its terminal `Event::Result`. Use
this when you want to watch one worker interactively without pulling
up the whole TUI.

### Diff

```bash
pitboss diff <run-a> <run-b>
```

Compares two runs side-by-side. Useful for A/B testing prompts or models.

---

## Headless mode (agents dispatching pitboss without a terminal)

If you (the agent) are dispatching pitboss without a terminal attached —
running in a container, under systemd, from another orchestrator — the
behavior diverges from interactive TUI use in several ways. Read this
section before writing manifests for headless dispatch.

### Permission model

**Pitboss is the sole permission authority for every claude subprocess
it spawns.** As of v0.7.1 every spawned claude (lead, sub-lead, worker)
receives:

1. `CLAUDE_CODE_ENTRYPOINT=sdk-ts` in env — closes claude's MCP-tool
   permission gate. Operator-overridable via `[defaults.env]`.
2. `--dangerously-skip-permissions` on the CLI — closes claude's
   filesystem (read/write outside cwd), bash-with-`$VAR`-expansion, and
   bash-with-`&&` gates. **Set unconditionally; not env-overridable.**

Without (1) sub-leads in headless dispatch exited in ~7 seconds
reporting `Success` with no output (apologizing that they couldn't get
MCP permission). Without (2), even after the MCP gate was closed, every
sublead's `echo x >> "$WORK_DIR/file"` returned `"Contains
simple_expansion"` — `-p` mode has no UI to answer the prompt — and the
orchestration plan collapsed silently with empty registries and null
kv reads.

Pitboss replaces the closed claude gates with its own approval surface:
`[run].approval_policy` (block / auto_approve / auto_reject),
`[[approval_policy]]` rules with TTL+fallback, the `request_approval`
and `propose_plan` MCP tools, and the TUI's approve/reject modal. If
you need claude's own gate fully back, do not use pitboss's headless
dispatch — drive the claude CLI interactively instead.

The trust boundary: anything you wouldn't run in your own claude
session under `--dangerously-skip-permissions` should not be in a
pitboss manifest. Operator-supplied prompts have full filesystem and
shell access at the `[lead].directory` cwd. Treat manifests as
production code.

### Approval policy — set `auto_approve` (or use rules)

Without a TUI to approve things, pitboss's own approval mechanisms hang
forever (or until timeout) if not configured. Set:

```toml
[run]
default_approval_policy = "auto_approve"  # or "auto_reject" for strict dry-run dispatch
```

For finer control, use `[[approval_policy]]` rules with `ttl_secs`
fallbacks. Pitboss now emits a startup warning to stderr when it detects
`default_approval_policy = "block"` (or unset) AND no TTY on stdout — read it
before assuming a hang is something else.

### `require_plan_approval = true` is usually wrong headless

Setting `[run].require_plan_approval = true` blocks the run until an
operator approves the lead's first `propose_plan` call. In headless mode
this hangs unless the lead's `propose_plan` includes a `ttl_secs` +
`fallback`. If you see a task land with `status: "ApprovalRejected"` (v0.7+)
or `"ApprovalTimedOut"` (v0.8+, TTL elapsed and fallback fired), this gate is
the most likely cause.

### Hierarchical mode with `use_worktree = true` requires a git repo

When `use_worktree = true` (the default), both the lead and each worker
directory must be inside a git repository — pitboss calls `git worktree add`
to create an isolated checkout per actor. With `use_worktree = false` no git
repository is required; actors share the directory directly.

If you dispatch a hierarchical manifest in a fresh workspace with worktrees
enabled, initialise the repo first:

```bash
git init /workspace
git -C /workspace commit --allow-empty -m "init"
```

Flat mode (`[[task]]` only, no `[lead]`) follows the same rule — it only
needs a git repo when `use_worktree = true`.

### Reading run status without the TUI

```bash
# Snapshot table of all tasks (in-flight or completed):
pitboss status <run-id-prefix>

# Machine-readable JSON array:
pitboss status <run-id-prefix> --json
```

Other low-level options:

- `pitboss attach <run-prefix> <task-id>` — follow a specific worker's stream-json
- `cat <run-dir>/summary.jsonl` — completed tasks (streamed append-only)
- `cat <run-dir>/summary.json` — full summary on clean finalize
- `ls <run-dir>/tasks/` — all spawned task directories

The root lead's logs live at `<run-dir>/tasks/<lead-id>/stdout.log`.
Workers and sub-leads live at `<run-dir>/tasks/<task-id>/` and
`<run-dir>/tasks/<sublead-id>/` respectively. Sub-lead ids are
`sublead-<uuid>` — they don't match the manifest's `[lead].id`. Use
the UUID form from `summary.jsonl` when calling `pitboss attach`.

### Terminal-state classification (v0.7+)

A task that exited because an approval returned negative is classified
distinctly from a task that succeeded:

| Status | Meaning |
|--------|---------|
| `Success` | Task completed work and exited cleanly |
| `Failed` | Task exited with non-zero status |
| `TimedOut` | Task exceeded `timeout_secs` |
| `Cancelled` | Task was explicitly cancelled by operator or cascade |
| `SpawnFailed` | Task never started (worktree prep, claude not found, etc.) |
| `ApprovalRejected` | Task's last approval returned `{approved: false}` from operator action or `[[approval_policy]]` auto_reject rule, then exited shortly after |
| `ApprovalTimedOut` | Task's last approval aged past its declared `ttl_secs` and the configured `fallback` fired (v0.8: TTL is wired end-to-end via `BridgeEntry` — covers both queued approvals and ones already drained into a connected TUI). The task's `ApprovalResponse` carries `from_ttl: true` so downstream consumers can distinguish TTL-driven from operator-driven responses. |

Before v0.7, both new statuses were reported as `Success` because the
claude subprocess exited 0. If you see `ApprovalRejected` in
`summary.json`, your manifest's `approval_policy` (or operator action)
denied the actor's request — revisit the approval configuration.

### Customizing sub-lead env and tools per spawn (v0.7+)

`spawn_sublead` accepts optional `env` and `tools` parameters:

```
spawn_sublead(
  prompt: "...",
  model: "claude-sonnet-4-6",
  budget_usd: 2.0,
  max_workers: 4,
  env: { "MY_VAR": "value" },        // merged over pitboss defaults
  tools: ["Read", "Bash"]            // adds to standard sublead toolset
)
```

Both fields are optional. Operator-supplied `env` keys override pitboss
defaults (including `CLAUDE_CODE_ENTRYPOINT` if you really want claude's
own gate back for that sub-lead). Operator-supplied `tools` are added
to the standard sublead MCP toolset; pitboss orchestration tools are
always present regardless of override.

### Offline access to this doc

```bash
pitboss agents-md                                    # from any binary
cat /usr/share/doc/pitboss/AGENTS.md                  # from a container
```

Both routes serve the same bytes — `AGENTS.md` is `include_str!`'d into
the binary at compile time and `COPY`'d into the container image.
`pitboss_version` in the frontmatter matches the binary version.

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

## The MCP tools the lead has

When running hierarchical, the lead's `--allowedTools` is automatically
populated with these. You (the operator) don't list them explicitly.

### Orchestration tools

| Tool | Args | Returns |
|---|---|---|
| `mcp__pitboss__spawn_worker` | `{prompt, directory?, branch?, tools?, timeout_secs?, model?}` | `{task_id, worktree_path}` |
| `mcp__pitboss__worker_status` | `{task_id}` | `{state, started_at, partial_usage, last_text_preview, prompt_preview}` |
| `mcp__pitboss__wait_for_worker` | `{task_id, timeout_secs?}` | full `TaskRecord` when worker settles |
| `mcp__pitboss__wait_actor` | `{actor_id, timeout_secs?}` | `ActorTerminalRecord` (`Worker(TaskRecord)` or `Sublead(SubleadTerminalRecord)`) when actor settles. Accepts worker or sub-lead ids. `wait_for_worker` is a back-compat alias. |
| `mcp__pitboss__wait_for_any` | `{task_ids: [...], timeout_secs?}` | `{task_id, record}` on first settle |
| `mcp__pitboss__list_workers` | `{}` | `{workers: [{task_id, state, prompt_preview, started_at}, ...]}` |
| `mcp__pitboss__cancel_worker` | `{task_id, reason?: string}` | `{ok: bool}` — optional `reason` delivers a synthetic `[SYSTEM]` reprompt to the killed actor's direct parent lead via kill+resume |
| `mcp__pitboss__pause_worker` | `{task_id, mode?}` — `mode` is `"cancel"` (default) or `"freeze"` | `{ok: bool}` |
| `mcp__pitboss__continue_worker` | `{task_id, prompt?}` | `{ok: bool}` |
| `mcp__pitboss__reprompt_worker` | `{task_id, prompt}` | `{ok: bool}` — mid-flight course-correct via `claude --resume` |
| `mcp__pitboss__request_approval` | `{summary, timeout_secs?, plan?: ApprovalPlan}` | `{approved, comment?, edited_summary?, reason?}` |
| `mcp__pitboss__propose_plan` | `{plan: ApprovalPlan, timeout_secs?}` | `{approved, comment?, edited_summary?, reason?}` |
| `mcp__pitboss__spawn_sublead` | `{prompt, model, budget_usd?, max_workers?, lead_timeout_secs?, initial_ref?, read_down?, env?, tools?, resume_session_id?}` | `{sublead_id}` — root lead only; requires `[lead] allow_subleads = true`. `resume_session_id` is used by `pitboss resume` to re-attach a prior sub-lead session; omit for fresh spawns. See Depth-2 section. |
| `mcp__pitboss__run_lease_acquire` | `{key, ttl_secs, wait_secs?}` | `{lease_id, version, ...}` — run-global; auto-released on actor termination |
| `mcp__pitboss__run_lease_release` | `{lease_id}` | `{ok: true}` |

All tool responses returning a collection are wrapped in a record
(`{workers: [...]}`, `{entries: [...]}`, `{entry: ...}`) — MCP spec
requires `structuredContent` to be `{ [key: string]: unknown }`, so
tools don't return bare arrays or null. Unwrap one level from callers.

**Worker spawn arg rules:**
- `prompt` is the new worker's system prompt / `-p` payload. Required.
- `directory` defaults to the lead's `directory`.
- `model` defaults to the lead's model. Override per-worker when you want
  heavier workers (Sonnet) under a Haiku lead.
- `tools` defaults to the lead's tools.

### Worker shared store (v0.4.2+)

A per-run, in-memory, hub-mediated coordination surface. Workers get
a narrower `mcp-config.json` that lists only the seven tools below
(not `spawn_worker` or `spawn_sublead` — workers are terminal). Namespaces:

- `/ref/*` — lead-write, all-read. Use for shared context (plans,
  conventions, targets).
- `/peer/<actor-id>/*` — actor-write (own path only), lead-override.
  Use for per-worker outputs like `/peer/self/completed`.
- `/shared/*` — all-write. Use for loose cross-worker coordination
  like `/shared/findings/`.
- `/leases/*` — managed via `lease_acquire` / `lease_release` only.

Workers don't know their UUID actor-id; use the `/peer/self/` alias —
the dispatcher resolves it to `/peer/<caller.actor_id>/` at the tool
layer.

| Tool | Args | Returns |
|---|---|---|
| `mcp__pitboss__kv_get` | `{path}` | `{entry: Option<Entry>}` |
| `mcp__pitboss__kv_set` | `{path, value: bytes, override_flag?}` | `{version}` |
| `mcp__pitboss__kv_cas` | `{path, expected_version, new_value: bytes, override_flag?}` | `{version, swapped}` |
| `mcp__pitboss__kv_list` | `{glob}` | `{entries: [ListMetadata, ...]}` |
| `mcp__pitboss__kv_wait` | `{path, timeout_secs, min_version?}` | `Entry` when condition met |
| `mcp__pitboss__lease_acquire` | `{name, ttl_secs, wait_secs?}` | `{lease_id, version, ...}` |
| `mcp__pitboss__lease_release` | `{lease_id}` | `{ok: true}` |

### `mcp__pitboss__pause_worker`

Pause a running worker. Two modes, distinguished by `mode`:

- `mode: "cancel"` (default, v0.4.1+) — terminates the subprocess and
  snapshots `claude_session_id` so `continue_worker` can respawn via
  `claude --resume`. Zero context loss on Anthropic's side; some
  reload cost on resume.
- `mode: "freeze"` (v0.5.0+) — SIGSTOPs the subprocess in place.
  `continue_worker` SIGCONTs to resume. No state loss at all, but
  long freezes risk Anthropic dropping the HTTP session on their
  side — use for short pauses only.

Args: `{task_id: string, mode?: "cancel" | "freeze"}`. Fails if
worker is not in `Running` state with an initialized session.

### `mcp__pitboss__continue_worker`

Continue a previously-paused or frozen worker. For paused workers,
spawns `claude --resume <id>`; for frozen workers, SIGCONTs. Args:
`{task_id: string, prompt?: string}` (prompt ignored for frozen
workers — use `reprompt_worker` after continue if you want to
redirect a frozen worker).

### `mcp__pitboss__request_approval`

Gate a *single in-flight action* on operator approval. Block the lead
until the operator approves, rejects, or edits. Args:
`{summary: string, timeout_secs?: number, ttl_secs?: number, fallback?: "auto_reject"|"auto_approve"|"block", plan?: ApprovalPlan}`.
Returns `{approved: bool, comment?: string, edited_summary?: string, reason?: string}`.
When `approved = false`, `reason` carries the operator's rejection explanation if provided.
Policy-gated: see `approval_policy` below.

`ApprovalPlan` (v0.5.0+) is a typed structured schema that the TUI
renders as labeled sections:

```
{
  summary: string,              // required; appears in the modal title
  rationale?: string,           // why this action should be taken
  resources?: [string, ...],    // files / APIs / PRs that will be touched
  risks?: [string, ...],        // known failure modes; TUI highlights in warning color
  rollback?: string,            // how to undo if something goes wrong
}
```

Populate `plan` for any non-trivial approval (deletions, multi-file
edits, irreversible ops). The bare `summary` form still works for
simple approvals.

### `mcp__pitboss__propose_plan`

Gate the *entire run* on operator pre-flight approval. Distinct from
`request_approval`, which gates individual actions mid-run. Args:
`{plan: ApprovalPlan, timeout_secs?: number}`. Returns the same
shape as `request_approval`.

When `[run].require_plan_approval = true`, `spawn_worker` refuses
with `plan approval required: call propose_plan ...` until a plan
submitted via this tool has been operator-approved. The TUI modal
shows `[PRE-FLIGHT PLAN]` in its title (vs `[IN-FLIGHT ACTION]` for
`request_approval`) so operators can tell them apart at a glance. On
rejection, the gate stays closed so the lead can revise and retry.

When `require_plan_approval = false` (the default), calling
`propose_plan` is still valid but purely informational — `spawn_worker`
never checks the result.

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

## Operator keybindings (pitboss-tui, v0.6.0+)

Navigation / views:
- `h j k l` / arrows — navigate tiles
- `Tab` — cycle focus across sub-tree containers (v0.6+; depth-2 runs)
- `Enter` — open Detail view for focused tile (metadata pane + live
  git-diff + scrollable log); on a sub-tree container header,
  toggles expand/collapse
- `a` — focus the approval list pane (right-rail, non-modal; v0.6+)
- `o` — run picker (switch to another run)
- `?` — help overlay (full keybinding reference)
- `q` / `Ctrl-C` — quit
- `Esc` — close any overlay / modal

Mouse:
- Left-click a grid tile — focus + open Detail
- Left-click a run in the picker — open that run
- Right-click inside Detail — exit back to grid
- Scroll wheel inside Detail — scroll log 5 rows/tick

Scroll cadence inside Detail:
- `j` / `k` / arrows — 1 row
- `J` / `K` — 5 rows
- `Ctrl-D` / `Ctrl-U` / `PageDown` / `PageUp` — 10 rows
- `g` / `G` — jump to top / bottom (bottom re-enables auto-follow)

Control plane:
- `x` — confirm+cancel focused worker
- `X` — confirm+cancel entire run (cascades SIGTERM to every worker)
- `p` — pause focused worker (requires initialized session)
- `c` — continue paused worker
- `r` — open reprompt textarea (Ctrl+Enter to submit, Esc to cancel)
- During approval modal: `y` approve, `n` reject (with optional reason
  string, Ctrl+Enter to submit), `e` edit (Ctrl+Enter to submit, Esc to cancel)

Approval list pane (`'a'` to focus, v0.6+):
- `Up` / `Down` — navigate pending approvals
- `Enter` — open detail modal for the highlighted approval

## `[run].approval_policy`

Run-level scalar. Controls handling of `request_approval` calls when
no TUI is attached and no `[[approval_policy]]` rule matches.

- `"block"` (default) — queue until a TUI connects, or fail after
  `lead_timeout_secs`.
- `"auto_approve"` — immediate `{approved: true}`.
- `"auto_reject"` — immediate `{approved: false, comment: "no operator
  available"}`.

## `[[approval_policy]]` blocks (v0.6+)

Ordered list of deterministic rules evaluated in pure Rust before
approvals reach the operator queue. **NOT LLM-evaluated.** First match
wins; unmatched approvals fall through to `[run].approval_policy`.

```toml
[[approval_policy]]
match = { actor = "root", category = "tool_use", tool_name = "Bash" }
action = "auto_approve"

[[approval_policy]]
match = { category = "cost", cost_over = 1.00 }
action = "block"

[[approval_policy]]
match = { actor = "root→S1" }
action = "auto_reject"
```

Match fields (all optional; unset fields match any value):

| Field | Type | Notes |
|---|---|---|
| `actor` | string | `ActorPath` rendered as `"root"` or `"root→S1"` or `"root→S1→W3"` |
| `category` | string | snake_case enum: `"tool_use"`, `"plan"`, `"cost"`, `"other"` |
| `tool_name` | string | Exact tool name; only meaningful when `category = "tool_use"` |
| `cost_over` | float | Matches when the `cost_estimate` hint on the approval exceeds this USD value |

Action values (snake_case):

| Action | Effect |
|---|---|
| `"auto_approve"` | Immediate approved response, no operator queue entry |
| `"auto_reject"` | Immediate rejected response with optional `reason` |
| `"block"` | Force into operator queue regardless of run-level policy |

For full syntax reference and defense-in-depth patterns see:
- [Approval policy reference](https://sds-mode.github.io/pitboss/operator-guide/approval-policy-reference.html)
- [Defense-in-depth](https://sds-mode.github.io/pitboss/security/defense-in-depth.html)

### Reject-with-reason (v0.6+)

When the operator rejects an approval, an optional `reason: string` is
accepted in the modal. The reason flows back through MCP to the
requesting actor's session so Claude can adapt without a separate
reprompt round-trip. Appears in the `reason` field of the approval
response alongside `approved: false`.

### Approval TTL + fallback (v0.6+)

`request_approval` accepts optional `ttl_secs` and `fallback` hints:
- `ttl_secs` — seconds after which the approval auto-resolves
- `fallback` — action to take on TTL expiry: `"auto_reject"` (default),
  `"auto_approve"`, or `"block"` (requeue)

Prevents unreachable operators from permanently stalling a run. Set a
short TTL + `fallback = "auto_reject"` on approvals that should not
block indefinitely if the operator steps away.

---

## Canonical examples

### 1. Fan out a summarization across N files

Flat mode, predeclared tasks.

```toml
[run]
max_parallel_tasks = 3

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
[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[lead]
id = "author-digest"
directory = "/path/to/repo"
prompt = """
List the last 20 commits with `git log --format='%H %an %s' -20`. Group
them by author. Spawn one worker per unique author via
mcp__pitboss__spawn_worker to summarize that author's work in
/tmp/digest/<author-slug>.md. Wait for all via mcp__pitboss__wait_for_worker.
Compose a combined /tmp/digest/SUMMARY.md. Then exit.
"""
max_workers = 6
budget_usd = 1.50
lead_timeout_secs = 1200
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
worktree_cleanup = "never"

[defaults]
model = "claude-haiku-4-5"
use_worktree = false        # read-only audit — no worktree isolation needed

[lead]
id = "refactor-analyst"
directory = "/path/to/target-repo"
prompt = """
[...concrete directions for spawning 4 workers, one per audit angle,
each writing to /tmp/refactor/<angle>.md; then reading them back and
synthesizing into /tmp/refactor/REFACTOR-PLAN.md...]
"""
max_workers = 4
budget_usd = 1.50
lead_timeout_secs = 1500
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
[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[lead]
id = "partial"
directory = "/path/to/repo"
prompt = """
Attempt to spawn 6 workers with mcp__pitboss__spawn_worker, one per file in
src/. When a spawn fails with 'budget exceeded', DO NOT retry — record the
file and move on. Wait for successfully-spawned workers, then compose a
partial summary noting which files were skipped and why.
"""
max_workers = 8
budget_usd = 0.20
lead_timeout_secs = 900
```

Use this pattern when you want to explore *what you can get* within a fixed
spend envelope.

---

## Depth-2 sub-leads (v0.6+)

Pitboss supports a single optional level of nesting beyond the original
hierarchical mode. A root lead with `allow_subleads = true` can spawn
sub-leads at runtime via `spawn_sublead`. Each sub-lead is itself a
Claude session with its own workers. Workers remain terminal — they
cannot spawn anything.

### When to use sub-leads

Use sub-leads when the root lead's plan would otherwise require holding
context for orthogonal sub-tasks simultaneously (e.g., "phase 1 and
phase 2 both need their own decomposition tree, but they don't share
implementation details"). Each sub-lead gets a clean Claude session
focused on its own slice.

Use plain workers (no sub-leads) when each unit of work is a single
self-contained task. Sub-leads add coordination overhead; workers are
cheaper.

### Manifest

```toml
[lead]
id = "root"
directory = "/path/to/repo"
prompt = "..."
budget_usd = 20.00
max_workers = 12
allow_subleads = true
max_subleads = 8                # optional cap
max_sublead_budget_usd = 5.00   # optional cap on per-sub-lead envelope
max_total_workers = 20          # optional cap on total live workers (root + sub-trees)

[sublead_defaults]              # top-level (v0.9+, was [lead.sublead_defaults])
budget_usd = 2.00
max_workers = 4
lead_timeout_secs = 1800
read_down = false
```

### `spawn_sublead` MCP tool

```
spawn_sublead(
  prompt: string,
  model: string,
  budget_usd: float,           # required unless read_down=true
  max_workers: u32,            # required unless read_down=true
  lead_timeout_secs: u64?,
  initial_ref: { string: any }?,
  read_down: bool = false,
)
→ sublead_id: string
```

### Authz model

- **Strict tree by default.** Root cannot read into a sub-tree unless `read_down = true` was passed at spawn time.
- **Strict peer visibility.** At any layer, `/peer/<X>` is readable only by X itself, that layer's lead, or the operator (TUI). Workers within a sub-tree do NOT see each other's peer slots — coordinate via `/shared/*` or leases.
- **Operator (TUI) is super-user.** Read/write across all layers regardless of read-down.

### Lease scope-selection guidance

- Use `/leases/*` (per-layer KV namespace) for resources internal to the sub-tree (e.g., a worker-coordinated counter for "next chunk to process within S1").
- Use `run_lease_acquire(key, ttl)` (run-global, separate primitive) for resources that span sub-trees (e.g., a path on the operator's filesystem that any sub-tree might write to).
- When in doubt: prefer `run_lease_acquire`. Over-serializing is safer than silent cross-tree collision.

### Approval routing

- All approvals route to the operator via TUI. Root lead is not an approval authority.
- Set `[[approval_policy]]` rules in the manifest to auto-approve/auto-reject categories of approvals before they reach the operator. The matcher is deterministic — never evaluated by an LLM.

### Kill-with-reason

`cancel_worker(target, reason)` — when invoked with a reason, the killed actor's direct parent lead receives a synthetic reprompt with the reason text. Use this to correct a misbehaving sub-tree without a separate reprompt round-trip.

### Waiting on sub-leads

Use `wait_actor(sublead_id)` to block until a sub-lead settles.
Returns `ActorTerminalRecord` (a `Sublead(SubleadTerminalRecord)` variant).
`wait_for_worker` only accepts worker ids — call `wait_actor` for sub-leads.

### Cancel cascade

Cancellation is depth-first. Root cancel → sub-leads → their workers, with the existing two-phase drain at each layer.

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

The current version is declared in the frontmatter at the top of this
file (`pitboss_version`). Schema may evolve; `pitboss validate` is the
source of truth. This document should stay self-contained — if something
here conflicts with the actual binary, the binary wins. File a PR.
