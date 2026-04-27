# Pitboss manifest map

> **Auto-generated. Do not edit by hand.** Regenerate with:
> ```
> pitboss schema --format=map > docs/manifest-map.md
> ```
> CI verifies the checked-in file matches the generator output via
> `pitboss schema --format=map --check docs/manifest-map.md`.

This document maps every TOML field in the v0.9 manifest schema to its
Rust struct field and source location. For schema *explanations* see
[`book/src/operator-guide/manifest-schema.md`](../book/src/operator-guide/manifest-schema.md).
The annotated example lives at [`pitboss.example.toml`](../pitboss.example.toml).

## `[run]` — `RunConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:313`](../crates/pitboss-cli/src/manifest/schema.rs#L313).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L322`](../crates/pitboss-cli/src/manifest/schema.rs#L322) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L329`](../crates/pitboss-cli/src/manifest/schema.rs#L329) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L335`](../crates/pitboss-cli/src/manifest/schema.rs#L335) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L343`](../crates/pitboss-cli/src/manifest/schema.rs#L343) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L350`](../crates/pitboss-cli/src/manifest/schema.rs#L350) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no TUI is attached and no rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L361`](../crates/pitboss-cli/src/manifest/schema.rs#L361) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L369`](../crates/pitboss-cli/src/manifest/schema.rs#L369) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L377`](../crates/pitboss-cli/src/manifest/schema.rs#L377) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:409`](../crates/pitboss-cli/src/manifest/schema.rs#L409).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L414`](../crates/pitboss-cli/src/manifest/schema.rs#L414) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L420`](../crates/pitboss-cli/src/manifest/schema.rs#L420) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L425`](../crates/pitboss-cli/src/manifest/schema.rs#L425) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L430`](../crates/pitboss-cli/src/manifest/schema.rs#L430) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L435`](../crates/pitboss-cli/src/manifest/schema.rs#L435) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L441`](../crates/pitboss-cli/src/manifest/schema.rs#L441) |

## `[container]` — `ContainerConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:71`](../crates/pitboss-cli/src/manifest/schema.rs#L71).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `image` | text | no | Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest. | [`../crates/pitboss-cli/src/manifest/schema.rs#L77`](../crates/pitboss-cli/src/manifest/schema.rs#L77) |
| `runtime` | enum (`docker` \| `podman` \| `auto`) | no | Container runtime to invoke. "auto" prefers podman. | [`../crates/pitboss-cli/src/manifest/schema.rs#L84`](../crates/pitboss-cli/src/manifest/schema.rs#L84) |
| `extra_args` | string list | no | Args inserted verbatim before the image name in the run invocation. | [`../crates/pitboss-cli/src/manifest/schema.rs#L91`](../crates/pitboss-cli/src/manifest/schema.rs#L91) |
| `workdir` | path | no | cwd inside the container; defaults to the first mount's container path. | [`../crates/pitboss-cli/src/manifest/schema.rs#L103`](../crates/pitboss-cli/src/manifest/schema.rs#L103) |

## `[[container.mount]]` — `MountSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:109`](../crates/pitboss-cli/src/manifest/schema.rs#L109).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `host` | path | **yes** | Absolute host path. ~ is expanded. | [`../crates/pitboss-cli/src/manifest/schema.rs#L112`](../crates/pitboss-cli/src/manifest/schema.rs#L112) |
| `container` | path | **yes** | Absolute path inside the container. | [`../crates/pitboss-cli/src/manifest/schema.rs#L115`](../crates/pitboss-cli/src/manifest/schema.rs#L115) |
| `readonly` | boolean | no | Mount read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L119`](../crates/pitboss-cli/src/manifest/schema.rs#L119) |

## `[[task]]` — `Task`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:472`](../crates/pitboss-cli/src/manifest/schema.rs#L472).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L477`](../crates/pitboss-cli/src/manifest/schema.rs#L477) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L482`](../crates/pitboss-cli/src/manifest/schema.rs#L482) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L488`](../crates/pitboss-cli/src/manifest/schema.rs#L488) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L493`](../crates/pitboss-cli/src/manifest/schema.rs#L493) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L499`](../crates/pitboss-cli/src/manifest/schema.rs#L499) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L504`](../crates/pitboss-cli/src/manifest/schema.rs#L504) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L506`](../crates/pitboss-cli/src/manifest/schema.rs#L506) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L512`](../crates/pitboss-cli/src/manifest/schema.rs#L512) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L514`](../crates/pitboss-cli/src/manifest/schema.rs#L514) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L516`](../crates/pitboss-cli/src/manifest/schema.rs#L516) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L521`](../crates/pitboss-cli/src/manifest/schema.rs#L521) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L527`](../crates/pitboss-cli/src/manifest/schema.rs#L527) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:537`](../crates/pitboss-cli/src/manifest/schema.rs#L537).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L544`](../crates/pitboss-cli/src/manifest/schema.rs#L544) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L551`](../crates/pitboss-cli/src/manifest/schema.rs#L551) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L563`](../crates/pitboss-cli/src/manifest/schema.rs#L563) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L571`](../crates/pitboss-cli/src/manifest/schema.rs#L571) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L574`](../crates/pitboss-cli/src/manifest/schema.rs#L574) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L581`](../crates/pitboss-cli/src/manifest/schema.rs#L581) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L587`](../crates/pitboss-cli/src/manifest/schema.rs#L587) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L593`](../crates/pitboss-cli/src/manifest/schema.rs#L593) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L599`](../crates/pitboss-cli/src/manifest/schema.rs#L599) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L605`](../crates/pitboss-cli/src/manifest/schema.rs#L605) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L614`](../crates/pitboss-cli/src/manifest/schema.rs#L614) |
| `budget_usd` | float | no | Soft cap on lead spend with reservation accounting. spawn_worker fails once spent + reserved + next_estimate > budget. | [`../crates/pitboss-cli/src/manifest/schema.rs#L623`](../crates/pitboss-cli/src/manifest/schema.rs#L623) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L632`](../crates/pitboss-cli/src/manifest/schema.rs#L632) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_a (default) makes pitboss the sole permission authority. path_b routes claude's gate through pitboss (rejected at validate time pending stabilization). | [`../crates/pitboss-cli/src/manifest/schema.rs#L645`](../crates/pitboss-cli/src/manifest/schema.rs#L645) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L654`](../crates/pitboss-cli/src/manifest/schema.rs#L654) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L661`](../crates/pitboss-cli/src/manifest/schema.rs#L661) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L668`](../crates/pitboss-cli/src/manifest/schema.rs#L668) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L676`](../crates/pitboss-cli/src/manifest/schema.rs#L676) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:683`](../crates/pitboss-cli/src/manifest/schema.rs#L683).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L688`](../crates/pitboss-cli/src/manifest/schema.rs#L688) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L693`](../crates/pitboss-cli/src/manifest/schema.rs#L693) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L698`](../crates/pitboss-cli/src/manifest/schema.rs#L698) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L704`](../crates/pitboss-cli/src/manifest/schema.rs#L704) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:263`](../crates/pitboss-cli/src/manifest/schema.rs#L263).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L274`](../crates/pitboss-cli/src/manifest/schema.rs#L274) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:137`](../crates/pitboss-cli/src/manifest/schema.rs#L137).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L143`](../crates/pitboss-cli/src/manifest/schema.rs#L143) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L149`](../crates/pitboss-cli/src/manifest/schema.rs#L149) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L153`](../crates/pitboss-cli/src/manifest/schema.rs#L153) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L160`](../crates/pitboss-cli/src/manifest/schema.rs#L160) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:456`](../crates/pitboss-cli/src/manifest/schema.rs#L456).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L461`](../crates/pitboss-cli/src/manifest/schema.rs#L461) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L467`](../crates/pitboss-cli/src/manifest/schema.rs#L467) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:240`](../crates/pitboss-cli/src/manifest/schema.rs#L240).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L251`](../crates/pitboss-cli/src/manifest/schema.rs#L251) |

