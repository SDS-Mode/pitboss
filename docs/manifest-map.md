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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:439`](../crates/pitboss-cli/src/manifest/schema.rs#L439).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `name` | text | no | Human-readable label used to group related runs in the console (e.g. "build-db", "nightly-sync"). When unset, the manifest filename is used as fallback. | [`../crates/pitboss-cli/src/manifest/schema.rs#L450`](../crates/pitboss-cli/src/manifest/schema.rs#L450) |
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L459`](../crates/pitboss-cli/src/manifest/schema.rs#L459) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L466`](../crates/pitboss-cli/src/manifest/schema.rs#L466) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L472`](../crates/pitboss-cli/src/manifest/schema.rs#L472) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L480`](../crates/pitboss-cli/src/manifest/schema.rs#L480) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L487`](../crates/pitboss-cli/src/manifest/schema.rs#L487) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L515`](../crates/pitboss-cli/src/manifest/schema.rs#L515) |
| `denial_termination_policy` | enum (`adapt` \| `reclassify`) | no | How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected. | [`../crates/pitboss-cli/src/manifest/schema.rs#L526`](../crates/pitboss-cli/src/manifest/schema.rs#L526) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L534`](../crates/pitboss-cli/src/manifest/schema.rs#L534) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L542`](../crates/pitboss-cli/src/manifest/schema.rs#L542) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:576`](../crates/pitboss-cli/src/manifest/schema.rs#L576).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L581`](../crates/pitboss-cli/src/manifest/schema.rs#L581) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L587`](../crates/pitboss-cli/src/manifest/schema.rs#L587) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L592`](../crates/pitboss-cli/src/manifest/schema.rs#L592) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L597`](../crates/pitboss-cli/src/manifest/schema.rs#L597) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L602`](../crates/pitboss-cli/src/manifest/schema.rs#L602) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L608`](../crates/pitboss-cli/src/manifest/schema.rs#L608) |

## `[container]` — `ContainerConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:72`](../crates/pitboss-cli/src/manifest/schema.rs#L72).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `image` | text | no | Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest. | [`../crates/pitboss-cli/src/manifest/schema.rs#L78`](../crates/pitboss-cli/src/manifest/schema.rs#L78) |
| `runtime` | enum (`docker` \| `podman` \| `auto`) | no | Container runtime to invoke. "auto" prefers podman. | [`../crates/pitboss-cli/src/manifest/schema.rs#L85`](../crates/pitboss-cli/src/manifest/schema.rs#L85) |
| `extra_args` | string list | no | Verbatim podman/docker run flags. Use for networking, capabilities, DNS, resources, etc. Example: ["--network=corp-fw", "--dns=10.0.0.53", "--cap-add=NET_ADMIN"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L96`](../crates/pitboss-cli/src/manifest/schema.rs#L96) |
| `extra_apt` | string list | no | Debian/Ubuntu packages installed inside the container before pitboss dispatch starts. Adds 30–90s spin-up per dispatch. Example: ["mdbook", "jq"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L111`](../crates/pitboss-cli/src/manifest/schema.rs#L111) |
| `workdir` | path | no | cwd inside the container; defaults to the first mount's container path. | [`../crates/pitboss-cli/src/manifest/schema.rs#L132`](../crates/pitboss-cli/src/manifest/schema.rs#L132) |

## `[[container.mount]]` — `MountSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:138`](../crates/pitboss-cli/src/manifest/schema.rs#L138).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `host` | path | **yes** | Absolute host path. ~ is expanded. | [`../crates/pitboss-cli/src/manifest/schema.rs#L141`](../crates/pitboss-cli/src/manifest/schema.rs#L141) |
| `container` | path | **yes** | Absolute path inside the container. | [`../crates/pitboss-cli/src/manifest/schema.rs#L144`](../crates/pitboss-cli/src/manifest/schema.rs#L144) |
| `readonly` | boolean | no | Mount read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L148`](../crates/pitboss-cli/src/manifest/schema.rs#L148) |

## `[[task]]` — `Task`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:639`](../crates/pitboss-cli/src/manifest/schema.rs#L639).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L644`](../crates/pitboss-cli/src/manifest/schema.rs#L644) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L649`](../crates/pitboss-cli/src/manifest/schema.rs#L649) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L655`](../crates/pitboss-cli/src/manifest/schema.rs#L655) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L660`](../crates/pitboss-cli/src/manifest/schema.rs#L660) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L666`](../crates/pitboss-cli/src/manifest/schema.rs#L666) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L671`](../crates/pitboss-cli/src/manifest/schema.rs#L671) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L673`](../crates/pitboss-cli/src/manifest/schema.rs#L673) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L679`](../crates/pitboss-cli/src/manifest/schema.rs#L679) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L681`](../crates/pitboss-cli/src/manifest/schema.rs#L681) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L683`](../crates/pitboss-cli/src/manifest/schema.rs#L683) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L688`](../crates/pitboss-cli/src/manifest/schema.rs#L688) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L694`](../crates/pitboss-cli/src/manifest/schema.rs#L694) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:704`](../crates/pitboss-cli/src/manifest/schema.rs#L704).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L711`](../crates/pitboss-cli/src/manifest/schema.rs#L711) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L718`](../crates/pitboss-cli/src/manifest/schema.rs#L718) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L730`](../crates/pitboss-cli/src/manifest/schema.rs#L730) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L738`](../crates/pitboss-cli/src/manifest/schema.rs#L738) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L741`](../crates/pitboss-cli/src/manifest/schema.rs#L741) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L748`](../crates/pitboss-cli/src/manifest/schema.rs#L748) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L754`](../crates/pitboss-cli/src/manifest/schema.rs#L754) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L760`](../crates/pitboss-cli/src/manifest/schema.rs#L760) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L766`](../crates/pitboss-cli/src/manifest/schema.rs#L766) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L772`](../crates/pitboss-cli/src/manifest/schema.rs#L772) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L781`](../crates/pitboss-cli/src/manifest/schema.rs#L781) |
| `budget_usd` | float | no | Soft cap on lead spend with reservation accounting. spawn_worker fails once spent + reserved + next_estimate > budget. | [`../crates/pitboss-cli/src/manifest/schema.rs#L790`](../crates/pitboss-cli/src/manifest/schema.rs#L790) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L799`](../crates/pitboss-cli/src/manifest/schema.rs#L799) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_a (default) makes pitboss the sole permission authority. path_b routes claude's gate through pitboss (rejected at validate time pending stabilization). | [`../crates/pitboss-cli/src/manifest/schema.rs#L812`](../crates/pitboss-cli/src/manifest/schema.rs#L812) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L821`](../crates/pitboss-cli/src/manifest/schema.rs#L821) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L828`](../crates/pitboss-cli/src/manifest/schema.rs#L828) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L835`](../crates/pitboss-cli/src/manifest/schema.rs#L835) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L843`](../crates/pitboss-cli/src/manifest/schema.rs#L843) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:850`](../crates/pitboss-cli/src/manifest/schema.rs#L850).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L855`](../crates/pitboss-cli/src/manifest/schema.rs#L855) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L860`](../crates/pitboss-cli/src/manifest/schema.rs#L860) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L865`](../crates/pitboss-cli/src/manifest/schema.rs#L865) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L871`](../crates/pitboss-cli/src/manifest/schema.rs#L871) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:389`](../crates/pitboss-cli/src/manifest/schema.rs#L389).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L400`](../crates/pitboss-cli/src/manifest/schema.rs#L400) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:183`](../crates/pitboss-cli/src/manifest/schema.rs#L183).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L189`](../crates/pitboss-cli/src/manifest/schema.rs#L189) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L195`](../crates/pitboss-cli/src/manifest/schema.rs#L195) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L199`](../crates/pitboss-cli/src/manifest/schema.rs#L199) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L206`](../crates/pitboss-cli/src/manifest/schema.rs#L206) |

## `[communication]` — `CommunicationConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:243`](../crates/pitboss-cli/src/manifest/schema.rs#L243).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `mode` | enum (`disabled` \| `parent_child`) | no | Actor communication policy mode. "disabled" hides the message_* and artifact_* tools (conservative default). "parent_child" enables them with parent/sublead/worker authz. | [`../crates/pitboss-cli/src/manifest/schema.rs#L250`](../crates/pitboss-cli/src/manifest/schema.rs#L250) |
| `max_message_bytes` | integer | no | Maximum UTF-8 body size accepted by message_send. | [`../crates/pitboss-cli/src/manifest/schema.rs#L256`](../crates/pitboss-cli/src/manifest/schema.rs#L256) |
| `max_artifact_bytes` | integer | no | Maximum decoded artifact payload size accepted by artifact_put. | [`../crates/pitboss-cli/src/manifest/schema.rs#L262`](../crates/pitboss-cli/src/manifest/schema.rs#L262) |
| `max_artifacts_per_actor` | integer | no | Maximum artifacts one actor may publish in a single run. | [`../crates/pitboss-cli/src/manifest/schema.rs#L268`](../crates/pitboss-cli/src/manifest/schema.rs#L268) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:623`](../crates/pitboss-cli/src/manifest/schema.rs#L623).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L628`](../crates/pitboss-cli/src/manifest/schema.rs#L628) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L634`](../crates/pitboss-cli/src/manifest/schema.rs#L634) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:366`](../crates/pitboss-cli/src/manifest/schema.rs#L366).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L377`](../crates/pitboss-cli/src/manifest/schema.rs#L377) |

