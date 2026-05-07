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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:469`](../crates/pitboss-cli/src/manifest/schema.rs#L469).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `name` | text | no | Human-readable label used to group related runs in the console (e.g. "build-db", "nightly-sync"). When unset, the manifest filename is used as fallback. | [`../crates/pitboss-cli/src/manifest/schema.rs#L480`](../crates/pitboss-cli/src/manifest/schema.rs#L480) |
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L489`](../crates/pitboss-cli/src/manifest/schema.rs#L489) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L496`](../crates/pitboss-cli/src/manifest/schema.rs#L496) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L502`](../crates/pitboss-cli/src/manifest/schema.rs#L502) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L510`](../crates/pitboss-cli/src/manifest/schema.rs#L510) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L517`](../crates/pitboss-cli/src/manifest/schema.rs#L517) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L545`](../crates/pitboss-cli/src/manifest/schema.rs#L545) |
| `denial_termination_policy` | enum (`adapt` \| `reclassify`) | no | How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected. | [`../crates/pitboss-cli/src/manifest/schema.rs#L556`](../crates/pitboss-cli/src/manifest/schema.rs#L556) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L564`](../crates/pitboss-cli/src/manifest/schema.rs#L564) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L572`](../crates/pitboss-cli/src/manifest/schema.rs#L572) |
| `require_actor_type` | boolean | no | When true, every spawn_worker and spawn_sublead call must name a declared [[worker_type]]/[[sublead_type]]. Default false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L583`](../crates/pitboss-cli/src/manifest/schema.rs#L583) |
| `untyped_actor_policy` | enum (`bridge` \| `block`) | no | Path-B-only behavior for un-typed callers reaching permission_prompt. `bridge` (default) routes to the operator queue; `block` auto-denies via a synthesized empty profile. | [`../crates/pitboss-cli/src/manifest/schema.rs#L600`](../crates/pitboss-cli/src/manifest/schema.rs#L600) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:738`](../crates/pitboss-cli/src/manifest/schema.rs#L738).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L743`](../crates/pitboss-cli/src/manifest/schema.rs#L743) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L749`](../crates/pitboss-cli/src/manifest/schema.rs#L749) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L754`](../crates/pitboss-cli/src/manifest/schema.rs#L754) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L759`](../crates/pitboss-cli/src/manifest/schema.rs#L759) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L764`](../crates/pitboss-cli/src/manifest/schema.rs#L764) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L770`](../crates/pitboss-cli/src/manifest/schema.rs#L770) |

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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:801`](../crates/pitboss-cli/src/manifest/schema.rs#L801).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L806`](../crates/pitboss-cli/src/manifest/schema.rs#L806) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L811`](../crates/pitboss-cli/src/manifest/schema.rs#L811) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L817`](../crates/pitboss-cli/src/manifest/schema.rs#L817) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L822`](../crates/pitboss-cli/src/manifest/schema.rs#L822) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L828`](../crates/pitboss-cli/src/manifest/schema.rs#L828) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L833`](../crates/pitboss-cli/src/manifest/schema.rs#L833) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L835`](../crates/pitboss-cli/src/manifest/schema.rs#L835) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L841`](../crates/pitboss-cli/src/manifest/schema.rs#L841) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L843`](../crates/pitboss-cli/src/manifest/schema.rs#L843) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L845`](../crates/pitboss-cli/src/manifest/schema.rs#L845) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L850`](../crates/pitboss-cli/src/manifest/schema.rs#L850) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L856`](../crates/pitboss-cli/src/manifest/schema.rs#L856) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:866`](../crates/pitboss-cli/src/manifest/schema.rs#L866).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L873`](../crates/pitboss-cli/src/manifest/schema.rs#L873) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L880`](../crates/pitboss-cli/src/manifest/schema.rs#L880) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L892`](../crates/pitboss-cli/src/manifest/schema.rs#L892) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L900`](../crates/pitboss-cli/src/manifest/schema.rs#L900) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L903`](../crates/pitboss-cli/src/manifest/schema.rs#L903) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L910`](../crates/pitboss-cli/src/manifest/schema.rs#L910) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L916`](../crates/pitboss-cli/src/manifest/schema.rs#L916) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L922`](../crates/pitboss-cli/src/manifest/schema.rs#L922) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L928`](../crates/pitboss-cli/src/manifest/schema.rs#L928) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L934`](../crates/pitboss-cli/src/manifest/schema.rs#L934) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L943`](../crates/pitboss-cli/src/manifest/schema.rs#L943) |
| `budget_usd` | float | no | Soft cap on lead spend with reservation accounting. spawn_worker fails once spent + reserved + next_estimate > budget. | [`../crates/pitboss-cli/src/manifest/schema.rs#L952`](../crates/pitboss-cli/src/manifest/schema.rs#L952) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L961`](../crates/pitboss-cli/src/manifest/schema.rs#L961) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_a (default) makes pitboss the sole permission authority. path_b routes claude's gate through pitboss (rejected at validate time pending stabilization). | [`../crates/pitboss-cli/src/manifest/schema.rs#L974`](../crates/pitboss-cli/src/manifest/schema.rs#L974) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L983`](../crates/pitboss-cli/src/manifest/schema.rs#L983) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L990`](../crates/pitboss-cli/src/manifest/schema.rs#L990) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L997`](../crates/pitboss-cli/src/manifest/schema.rs#L997) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1005`](../crates/pitboss-cli/src/manifest/schema.rs#L1005) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1012`](../crates/pitboss-cli/src/manifest/schema.rs#L1012).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1017`](../crates/pitboss-cli/src/manifest/schema.rs#L1017) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1022`](../crates/pitboss-cli/src/manifest/schema.rs#L1022) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1027`](../crates/pitboss-cli/src/manifest/schema.rs#L1027) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1033`](../crates/pitboss-cli/src/manifest/schema.rs#L1033) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:419`](../crates/pitboss-cli/src/manifest/schema.rs#L419).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L430`](../crates/pitboss-cli/src/manifest/schema.rs#L430) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:187`](../crates/pitboss-cli/src/manifest/schema.rs#L187).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L193`](../crates/pitboss-cli/src/manifest/schema.rs#L193) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L199`](../crates/pitboss-cli/src/manifest/schema.rs#L199) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L203`](../crates/pitboss-cli/src/manifest/schema.rs#L203) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L210`](../crates/pitboss-cli/src/manifest/schema.rs#L210) |
| `scope` | text | no | Optional injection scope. Form: "type:<id>" — narrows this server to actors spawned under that worker_type/sublead_type. Unset = inject into all actors. | [`../crates/pitboss-cli/src/manifest/schema.rs#L223`](../crates/pitboss-cli/src/manifest/schema.rs#L223) |

## `[[worker_type]]` — `WorkerType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:659`](../crates/pitboss-cli/src/manifest/schema.rs#L659).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_worker(worker_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L666`](../crates/pitboss-cli/src/manifest/schema.rs#L666) |
| `tools` | string list | no | Allowed tool surface for this worker class. Lead's spawn_worker(tools=...) must be a subset; omit means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L674`](../crates/pitboss-cli/src/manifest/schema.rs#L674) |
| `allowed_models` | string list | no | When non-empty, spawn_worker(model=...) must be in this list. Empty means any model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L682`](../crates/pitboss-cli/src/manifest/schema.rs#L682) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_worker(timeout_secs=...). Lead values clamp DOWN; smaller values pass through. | [`../crates/pitboss-cli/src/manifest/schema.rs#L690`](../crates/pitboss-cli/src/manifest/schema.rs#L690) |

## `[[sublead_type]]` — `SubleadType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:698`](../crates/pitboss-cli/src/manifest/schema.rs#L698).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_sublead(sublead_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L705`](../crates/pitboss-cli/src/manifest/schema.rs#L705) |
| `tools` | string list | no | Allowed tool surface for this sub-lead class. spawn_sublead(tools=...) must be a subset; empty means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L712`](../crates/pitboss-cli/src/manifest/schema.rs#L712) |
| `allowed_models` | string list | no | When non-empty, spawn_sublead(model=...) must be in this list. | [`../crates/pitboss-cli/src/manifest/schema.rs#L719`](../crates/pitboss-cli/src/manifest/schema.rs#L719) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_sublead(lead_timeout_secs=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L726`](../crates/pitboss-cli/src/manifest/schema.rs#L726) |
| `max_budget_usd` | float | no | Hard cap on spawn_sublead(budget_usd=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L733`](../crates/pitboss-cli/src/manifest/schema.rs#L733) |

## `[communication]` — `CommunicationConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:260`](../crates/pitboss-cli/src/manifest/schema.rs#L260).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `mode` | enum (`disabled` \| `parent_child`) | no | Actor communication policy mode. "disabled" hides the message_* and artifact_* tools (conservative default). "parent_child" enables them with parent/sublead/worker authz. | [`../crates/pitboss-cli/src/manifest/schema.rs#L267`](../crates/pitboss-cli/src/manifest/schema.rs#L267) |
| `max_message_bytes` | integer | no | Maximum UTF-8 body size accepted by message_send. | [`../crates/pitboss-cli/src/manifest/schema.rs#L273`](../crates/pitboss-cli/src/manifest/schema.rs#L273) |
| `max_artifact_bytes` | integer | no | Maximum decoded artifact payload size accepted by artifact_put. | [`../crates/pitboss-cli/src/manifest/schema.rs#L279`](../crates/pitboss-cli/src/manifest/schema.rs#L279) |
| `max_artifacts_per_actor` | integer | no | Maximum artifacts one actor may publish in a single run. | [`../crates/pitboss-cli/src/manifest/schema.rs#L285`](../crates/pitboss-cli/src/manifest/schema.rs#L285) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:785`](../crates/pitboss-cli/src/manifest/schema.rs#L785).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L790`](../crates/pitboss-cli/src/manifest/schema.rs#L790) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L796`](../crates/pitboss-cli/src/manifest/schema.rs#L796) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:396`](../crates/pitboss-cli/src/manifest/schema.rs#L396).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L407`](../crates/pitboss-cli/src/manifest/schema.rs#L407) |

