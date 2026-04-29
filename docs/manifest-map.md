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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:358`](../crates/pitboss-cli/src/manifest/schema.rs#L358).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `name` | text | no | Human-readable label used to group related runs in the console (e.g. "build-db", "nightly-sync"). When unset, the manifest filename is used as fallback. | [`../crates/pitboss-cli/src/manifest/schema.rs#L369`](../crates/pitboss-cli/src/manifest/schema.rs#L369) |
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L378`](../crates/pitboss-cli/src/manifest/schema.rs#L378) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L385`](../crates/pitboss-cli/src/manifest/schema.rs#L385) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L391`](../crates/pitboss-cli/src/manifest/schema.rs#L391) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L399`](../crates/pitboss-cli/src/manifest/schema.rs#L399) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L406`](../crates/pitboss-cli/src/manifest/schema.rs#L406) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L434`](../crates/pitboss-cli/src/manifest/schema.rs#L434) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L442`](../crates/pitboss-cli/src/manifest/schema.rs#L442) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L450`](../crates/pitboss-cli/src/manifest/schema.rs#L450) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:483`](../crates/pitboss-cli/src/manifest/schema.rs#L483).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L488`](../crates/pitboss-cli/src/manifest/schema.rs#L488) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L494`](../crates/pitboss-cli/src/manifest/schema.rs#L494) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L499`](../crates/pitboss-cli/src/manifest/schema.rs#L499) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L504`](../crates/pitboss-cli/src/manifest/schema.rs#L504) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L509`](../crates/pitboss-cli/src/manifest/schema.rs#L509) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L515`](../crates/pitboss-cli/src/manifest/schema.rs#L515) |

## `[container]` — `ContainerConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:71`](../crates/pitboss-cli/src/manifest/schema.rs#L71).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `image` | text | no | Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest. | [`../crates/pitboss-cli/src/manifest/schema.rs#L77`](../crates/pitboss-cli/src/manifest/schema.rs#L77) |
| `runtime` | enum (`docker` \| `podman` \| `auto`) | no | Container runtime to invoke. "auto" prefers podman. | [`../crates/pitboss-cli/src/manifest/schema.rs#L84`](../crates/pitboss-cli/src/manifest/schema.rs#L84) |
| `extra_args` | string list | no | Verbatim podman/docker run flags. Use for networking, capabilities, DNS, resources, etc. Example: ["--network=corp-fw", "--dns=10.0.0.53", "--cap-add=NET_ADMIN"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L95`](../crates/pitboss-cli/src/manifest/schema.rs#L95) |
| `extra_apt` | string list | no | Debian/Ubuntu packages installed inside the container before pitboss dispatch starts. Adds 30–90s spin-up per dispatch. Example: ["mdbook", "jq"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L110`](../crates/pitboss-cli/src/manifest/schema.rs#L110) |
| `workdir` | path | no | cwd inside the container; defaults to the first mount's container path. | [`../crates/pitboss-cli/src/manifest/schema.rs#L131`](../crates/pitboss-cli/src/manifest/schema.rs#L131) |

## `[[container.mount]]` — `MountSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:137`](../crates/pitboss-cli/src/manifest/schema.rs#L137).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `host` | path | **yes** | Absolute host path. ~ is expanded. | [`../crates/pitboss-cli/src/manifest/schema.rs#L140`](../crates/pitboss-cli/src/manifest/schema.rs#L140) |
| `container` | path | **yes** | Absolute path inside the container. | [`../crates/pitboss-cli/src/manifest/schema.rs#L143`](../crates/pitboss-cli/src/manifest/schema.rs#L143) |
| `readonly` | boolean | no | Mount read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L147`](../crates/pitboss-cli/src/manifest/schema.rs#L147) |

## `[[task]]` — `Task`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:546`](../crates/pitboss-cli/src/manifest/schema.rs#L546).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L551`](../crates/pitboss-cli/src/manifest/schema.rs#L551) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L556`](../crates/pitboss-cli/src/manifest/schema.rs#L556) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L562`](../crates/pitboss-cli/src/manifest/schema.rs#L562) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L567`](../crates/pitboss-cli/src/manifest/schema.rs#L567) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L573`](../crates/pitboss-cli/src/manifest/schema.rs#L573) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L578`](../crates/pitboss-cli/src/manifest/schema.rs#L578) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L580`](../crates/pitboss-cli/src/manifest/schema.rs#L580) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L586`](../crates/pitboss-cli/src/manifest/schema.rs#L586) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L588`](../crates/pitboss-cli/src/manifest/schema.rs#L588) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L590`](../crates/pitboss-cli/src/manifest/schema.rs#L590) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L595`](../crates/pitboss-cli/src/manifest/schema.rs#L595) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L601`](../crates/pitboss-cli/src/manifest/schema.rs#L601) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:611`](../crates/pitboss-cli/src/manifest/schema.rs#L611).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L618`](../crates/pitboss-cli/src/manifest/schema.rs#L618) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L625`](../crates/pitboss-cli/src/manifest/schema.rs#L625) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L637`](../crates/pitboss-cli/src/manifest/schema.rs#L637) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L645`](../crates/pitboss-cli/src/manifest/schema.rs#L645) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L648`](../crates/pitboss-cli/src/manifest/schema.rs#L648) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L655`](../crates/pitboss-cli/src/manifest/schema.rs#L655) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L661`](../crates/pitboss-cli/src/manifest/schema.rs#L661) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L667`](../crates/pitboss-cli/src/manifest/schema.rs#L667) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L673`](../crates/pitboss-cli/src/manifest/schema.rs#L673) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L679`](../crates/pitboss-cli/src/manifest/schema.rs#L679) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L688`](../crates/pitboss-cli/src/manifest/schema.rs#L688) |
| `budget_usd` | float | no | Soft cap on lead spend with reservation accounting. spawn_worker fails once spent + reserved + next_estimate > budget. | [`../crates/pitboss-cli/src/manifest/schema.rs#L697`](../crates/pitboss-cli/src/manifest/schema.rs#L697) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L706`](../crates/pitboss-cli/src/manifest/schema.rs#L706) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_a (default) makes pitboss the sole permission authority. path_b routes claude's gate through pitboss (rejected at validate time pending stabilization). | [`../crates/pitboss-cli/src/manifest/schema.rs#L719`](../crates/pitboss-cli/src/manifest/schema.rs#L719) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L728`](../crates/pitboss-cli/src/manifest/schema.rs#L728) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L735`](../crates/pitboss-cli/src/manifest/schema.rs#L735) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L742`](../crates/pitboss-cli/src/manifest/schema.rs#L742) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L750`](../crates/pitboss-cli/src/manifest/schema.rs#L750) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:757`](../crates/pitboss-cli/src/manifest/schema.rs#L757).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L762`](../crates/pitboss-cli/src/manifest/schema.rs#L762) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L767`](../crates/pitboss-cli/src/manifest/schema.rs#L767) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L772`](../crates/pitboss-cli/src/manifest/schema.rs#L772) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L778`](../crates/pitboss-cli/src/manifest/schema.rs#L778) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:308`](../crates/pitboss-cli/src/manifest/schema.rs#L308).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L319`](../crates/pitboss-cli/src/manifest/schema.rs#L319) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:182`](../crates/pitboss-cli/src/manifest/schema.rs#L182).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L188`](../crates/pitboss-cli/src/manifest/schema.rs#L188) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L194`](../crates/pitboss-cli/src/manifest/schema.rs#L194) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L198`](../crates/pitboss-cli/src/manifest/schema.rs#L198) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L205`](../crates/pitboss-cli/src/manifest/schema.rs#L205) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:530`](../crates/pitboss-cli/src/manifest/schema.rs#L530).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L535`](../crates/pitboss-cli/src/manifest/schema.rs#L535) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L541`](../crates/pitboss-cli/src/manifest/schema.rs#L541) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:285`](../crates/pitboss-cli/src/manifest/schema.rs#L285).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L296`](../crates/pitboss-cli/src/manifest/schema.rs#L296) |

