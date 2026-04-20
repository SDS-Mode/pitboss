# Manifest schema

Pitboss manifests are TOML files, typically named `pitboss.toml`. A manifest is either **flat** (one or more `[[task]]` entries) or **hierarchical** (exactly one `[[lead]]` entry). The two are mutually exclusive.

Always validate before dispatching:

```bash
pitboss validate pitboss.toml
```

---

## `[run]` — run-wide configuration

| Key | Type | Required? | Default | Notes |
|-----|------|-----------|---------|-------|
| `max_parallel` | int | no | 4 | Flat mode: concurrency cap. Overridden by `ANTHROPIC_MAX_CONCURRENT` env. |
| `halt_on_failure` | bool | no | false | Flat mode: stop remaining tasks on first failure. |
| `run_dir` | string path | no | `~/.local/share/pitboss/runs` | Where per-run artifacts land. |
| `worktree_cleanup` | `"always"` \| `"on_success"` \| `"never"` | no | `"on_success"` | What to do with each worker's worktree after completion. Use `"never"` for inspection-heavy runs or when you plan to resume. |
| `emit_event_stream` | bool | no | false | Emit a JSONL event stream (pause/cancel/approval events) alongside `summary.jsonl`. |
| `max_workers` | int | if `[[lead]]` present | unset | Hierarchical: hard cap on concurrent + queued workers (1–16). |
| `budget_usd` | float | if `[[lead]]` present | unset | Hierarchical: soft cap with reservation accounting. `spawn_worker` fails with `budget exceeded` once `spent + reserved + next_estimate > budget`. |
| `lead_timeout_secs` | int | no | 3600 | Hierarchical: wall-clock cap on the lead. Set generously for multi-hour plans (e.g., `21600` for a 6-hour plan executor). |
| `approval_policy` | `"block"` \| `"auto_approve"` \| `"auto_reject"` | no | `"block"` | Hierarchical: how `request_approval` / `propose_plan` behave when no TUI is attached. |
| `require_plan_approval` | bool | no | false | Hierarchical (v0.5.0+): when true, `spawn_worker` refuses until a `propose_plan` call has been approved. |
| `dump_shared_store` | bool | no | false | Hierarchical: write `shared-store.json` into the run directory on finalize. |

---

## `[defaults]` — task/lead defaults

Inherited by every `[[task]]` and `[[lead]]` unless overridden at the task level.

| Key | Type | Notes |
|-----|------|-------|
| `model` | string | e.g., `claude-haiku-4-5`, `claude-sonnet-4-6`, `claude-opus-4-7`. Dated suffixes allowed. |
| `effort` | `"low"` \| `"medium"` \| `"high"` | Maps to `claude --effort`. |
| `tools` | array of string | `--allowedTools` value. Pitboss auto-appends its MCP tools for leads and workers. Default: `["Read", "Write", "Edit", "Bash", "Glob", "Grep"]`. See [Security → Defense-in-depth → Read-only lead pattern](../security/defense-in-depth.md) for guidance on restricting this per worker. |
| `timeout_secs` | int | Per-task wall-clock cap. No default (no cap). |
| `use_worktree` | bool | Default `true`. Set `false` for read-only analysis runs. |
| `env` | table | Env vars passed to the `claude` subprocess. |

---

## `[[task]]` — flat mode (repeat for each task)

| Key | Required? | Notes |
|-----|-----------|-------|
| `id` | yes | Short slug. Alphanumeric + `_` + `-`. Unique within manifest. Used in logs, worktree names. |
| `directory` | yes | Must be inside a git repo if `use_worktree = true`. |
| `prompt` | yes | Sent to the `claude` subprocess via `-p`. |
| `branch` | no | Worktree branch name. Auto-generated if omitted. |
| `model`, `effort`, `tools`, `timeout_secs`, `use_worktree`, `env` | no | Per-task overrides of `[defaults]`. |

---

## `[[lead]]` — hierarchical mode (exactly one)

Same fields as `[[task]]`. The lead is a single Claude session that receives the MCP orchestration toolset. Mutually exclusive with `[[task]]`.

Additional fields on `[[lead]]` for depth-2 sub-leads (v0.6+):

| Key | Type | Notes |
|-----|------|-------|
| `allow_subleads` | bool | Default false. Set `true` to expose `spawn_sublead` to the root lead. |
| `max_subleads` | int | Optional cap on total sub-leads spawned. |
| `max_sublead_budget_usd` | float | Optional cap on the per-sub-lead `budget_usd` envelope. |
| `max_workers_across_tree` | int | Optional cap on total live workers across all sub-trees. |

### `[lead.sublead_defaults]`

Optional defaults for sub-leads spawned via `spawn_sublead`. Any field omitted in the `spawn_sublead` call falls back to these values.

| Key | Type |
|-----|------|
| `budget_usd` | float |
| `max_workers` | int |
| `lead_timeout_secs` | int |
| `read_down` | bool |

---

## `[[approval_policy]]` — declarative approval rules (v0.6+)

Zero or more policy blocks, evaluated in order. First matching rule wins.

```toml
[[approval_policy]]
match = { actor = "root→S1", category = "tool_use" }
action = "auto_approve"

[[approval_policy]]
match = { category = "plan" }
action = "block"
```

**Match fields** (all optional; unset fields always match):

| Field | Type | Notes |
|-------|------|-------|
| `actor` | string | Actor path, e.g., `"root→S1"` or `"root→S1→W3"`. |
| `category` | string | `"tool_use"`, `"plan"`, `"cost"`, etc. |
| `tool_name` | string | Specific MCP tool name. |
| `cost_over` | float | Fires when the request's `cost_estimate` exceeds this value (USD). |

**Actions:** `"auto_approve"`, `"auto_reject"`, `"block"` (forces operator review).

Rules are evaluated in pure Rust — deterministic, fast, never LLM-evaluated.

---

## Annotated example

The [pitboss.example.toml](https://github.com/SDS-Mode/pitboss/blob/main/pitboss.example.toml) in the repository root has every field annotated with usage notes. It is a good starting point for new manifests.

---

## Run artifacts

After dispatch, the run directory (`~/.local/share/pitboss/runs/<run-id>/`) contains:

| File | Contents |
|------|----------|
| `manifest.snapshot.toml` | Exact manifest bytes used |
| `resolved.json` | Fully resolved manifest (defaults applied) |
| `meta.json` | `run_id`, `started_at`, `claude_version`, `pitboss_version` |
| `summary.json` | Full structured summary (written on clean finalize) |
| `summary.jsonl` | Incremental task records as they finish |
| `tasks/<id>/stdout.log` | Raw stream-JSON from the task's subprocess |
| `tasks/<id>/stderr.log` | Stderr |
| `lead-mcp-config.json` | Hierarchical only: the `--mcp-config` pointing at the MCP bridge |
| `shared-store.json` | Hierarchical only: written when `dump_shared_store = true` |
