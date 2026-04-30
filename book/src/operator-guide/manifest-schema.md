# Manifest schema

Pitboss manifests are TOML files, typically named `pitboss.toml`. A manifest is either **flat** (one or more `[[task]]` entries) or **hierarchical** (exactly one `[lead]` entry). The two are mutually exclusive.

> **v0.9 schema** — collapses the v0.8 `[[lead]]`/`[lead]` split into one canonical `[lead]` (single-table) form, moves lead-level caps off `[run]` onto `[lead]`, promotes `[lead.sublead_defaults]` to top-level `[sublead_defaults]`, and renames a few fields for consistency. See the [Migration table](#migration-from-v08--v09) below.

Always validate before dispatching:

```bash
pitboss validate pitboss.toml
```

`pitboss validate` recognises pre-v0.9 manifests and reports each renamed/moved field as an actionable migration line.

---

## `[run]` — run-wide configuration

`[run]` carries settings that apply to the whole dispatch run. Lead-level caps that lived here in v0.8 moved to `[lead]` in v0.9.

| Key | Type | Required? | Default | Notes |
|-----|------|-----------|---------|-------|
| `max_parallel_tasks` | int | no | 4 | Flat mode: concurrency cap for `[[task]]` runs. Overridden by `ANTHROPIC_MAX_CONCURRENT` env. Renamed from `max_parallel` in v0.9. |
| `halt_on_failure` | bool | no | false | Flat mode: stop remaining tasks on first failure. |
| `run_dir` | string path | no | `~/.local/share/pitboss/runs` | Where per-run artifacts land. |
| `worktree_cleanup` | `"always"` \| `"on_success"` \| `"never"` | no | `"on_success"` | What to do with each worker's worktree after completion. Use `"never"` for inspection-heavy runs or when you plan to resume. |
| `emit_event_stream` | bool | no | false | Emit a JSONL event stream (pause/cancel/approval events) alongside `summary.jsonl`. |
| `default_approval_policy` | `"block"` \| `"auto_approve"` \| `"auto_reject"` | no | `"block"` | Hierarchical: default action for `request_approval` / `propose_plan` when no TUI is attached and no `[[approval_policy]]` rule matches. Renamed from `approval_policy` in v0.9. |
| `require_plan_approval` | bool | no | false | Hierarchical: when true, `spawn_worker` refuses until a `propose_plan` call has been approved. |
| `dump_shared_store` | bool | no | false | Hierarchical: write `shared-store.json` into the run directory on finalize. |

---

## `[defaults]` — per-actor inheritable knobs

Inherited by every `[[task]]` and `[lead]` unless overridden at the actor level.

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

## `[lead]` — hierarchical mode (single-table, exactly one)

The lead is a single Claude session that receives the MCP orchestration toolset. Mutually exclusive with `[[task]]`. The v0.8 `[[lead]]` array form is gone.

> **Important:** `prompt =` must appear **before** any subtable declaration (e.g. `[lead.env]`) in the TOML source. A `prompt =` after a subtable header is silently moved to that subtable's scope; `pitboss validate` catches the resulting empty prompt.

Required and per-actor fields:

| Key | Type | Required? | Notes |
|-----|------|-----------|-------|
| `id` | string | yes | Short slug used in logs, worktree names, TUI tile labels. |
| `directory` | path | yes | Working directory for the lead's claude subprocess. Must be a git work-tree if `use_worktree = true`. |
| `prompt` | string | yes | Operator instructions passed via `-p`. |
| `branch` | string | no | Worktree branch name. Auto-generated if omitted. |
| `model`, `effort`, `tools`, `timeout_secs`, `use_worktree`, `env` | various | no | Per-lead overrides of `[defaults]`. |

Lead-level caps (moved from `[run]` in v0.9):

| Key | Type | Default | Notes |
|-----|------|---------|-------|
| `max_workers` | int | unset | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. |
| `budget_usd` | float | unset | Soft cap with reservation accounting. `spawn_worker` fails with `budget exceeded` once `spent + reserved + next_estimate > budget`. |
| `lead_timeout_secs` | int | 3600 | Wall-clock cap on the lead session. No upper bound — set generously for multi-hour plans. |

Depth-2 controls (sub-leads):

| Key | Type | Default | Notes |
|-----|------|---------|-------|
| `allow_subleads` | bool | `false` | Set `true` to expose `spawn_sublead` to the root lead. |
| `max_subleads` | int | unset | Cap on total sub-leads spawned. |
| `max_sublead_budget_usd` | float | unset | Cap on the per-sub-lead `budget_usd` envelope. |
| `max_total_workers` | int | unset | Cap on total live workers across the entire tree (root + sub-trees). Renamed from `max_workers_across_tree` in v0.9. |
| `permission_routing` | `"path_a"` \| `"path_b"` | `"path_a"` | `"path_a"` (default) sets `CLAUDE_CODE_ENTRYPOINT=sdk-ts` — pitboss is the sole permission authority. `"path_b"` routes claude's permission gate through the `permission_prompt` MCP tool — rejected at validation time until stabilization (issues #92–#94). |

---

## `[sublead_defaults]` (top-level, v0.9+)

Promoted from `[lead.sublead_defaults]` in v0.9. Optional defaults applied to `spawn_sublead` calls that omit the corresponding parameters.

```toml
[sublead_defaults]
budget_usd = 2.00
max_workers = 4
lead_timeout_secs = 1800
read_down = false
```

| Key | Type | Notes |
|-----|------|-------|
| `budget_usd` | float | Per-sub-lead envelope when `read_down = false`. |
| `max_workers` | int | Per-sub-lead worker pool when `read_down = false`. |
| `lead_timeout_secs` | int | Wall-clock cap for the sub-lead session. |
| `read_down` | bool | When true, the sub-lead shares the root's budget and worker pool instead of carving its own envelope. |

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

## `[container]` — container dispatch (v0.8+)

Enables `pitboss container-dispatch`. When present, task and lead `directory` fields are container-side paths (host-filesystem existence is not checked at validation time).

```toml
[container]
image   = "ghcr.io/sds-mode/pitboss-with-goose:latest"  # optional
runtime = "auto"     # "docker", "podman", or "auto"
workdir = "/project" # optional; defaults to first mount's container path

[[container.mount]]
host      = "~/projects/myapp"
container = "/project"
readonly  = false
```

| Field | Type | Description |
|-------|------|-------------|
| `image` | string | Container image. Default: `ghcr.io/sds-mode/pitboss-with-goose:latest`. |
| `runtime` | string | `"docker"`, `"podman"`, or `"auto"` (prefers podman when available). |
| `extra_args` | string[] | Extra args inserted verbatim before the image name in the `run` invocation. |
| `workdir` | path | Working directory inside the container. Defaults to the first mount's container path, or `/home/pitboss`. |
| `[[container.mount]]` | array | Bind mount entries. |
| `mount.host` | path | Absolute host path. Tilde (`~`) is expanded. |
| `mount.container` | path | Absolute path inside the container. |
| `mount.readonly` | bool | Mount read-only. Default: `false`. |

Goose auth/state directories, the run artifact directory, and the manifest are auto-injected. `~/.claude` is also mounted for `claude-acp` pass-through compatibility. See [Container dispatch](./container-dispatch.md) for the full guide.

---

## Migration from v0.8 → v0.9

Pre-v0.9 manifests are rejected. `pitboss validate` scans for the migration patterns below and emits guidance:

| v0.8 form | v0.9 form |
|-----------|-----------|
| `[[lead]]` (array) | `[lead]` (single-table) |
| `[run].max_workers` | `[lead].max_workers` |
| `[run].budget_usd` | `[lead].budget_usd` |
| `[run].lead_timeout_secs` | `[lead].lead_timeout_secs` |
| `[run].max_parallel` | `[run].max_parallel_tasks` |
| `[run].approval_policy` | `[run].default_approval_policy` |
| `[lead].max_workers_across_tree` | `[lead].max_total_workers` |
| `[lead.sublead_defaults]` | top-level `[sublead_defaults]` |
| `[lead].id` and `[lead].directory` optional | both required |

In-flight runs (`resolved.json` snapshots in run-dirs) remain readable — `#[serde(alias)]` on the renamed `ResolvedManifest` fields preserves resume.

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
