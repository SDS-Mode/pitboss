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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:533`](../crates/pitboss-cli/src/manifest/schema.rs#L533).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `name` | text | no | Human-readable label used to group related runs in the console (e.g. "build-db", "nightly-sync"). When unset, the manifest filename is used as fallback. | [`../crates/pitboss-cli/src/manifest/schema.rs#L544`](../crates/pitboss-cli/src/manifest/schema.rs#L544) |
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L553`](../crates/pitboss-cli/src/manifest/schema.rs#L553) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L560`](../crates/pitboss-cli/src/manifest/schema.rs#L560) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L566`](../crates/pitboss-cli/src/manifest/schema.rs#L566) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L574`](../crates/pitboss-cli/src/manifest/schema.rs#L574) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L581`](../crates/pitboss-cli/src/manifest/schema.rs#L581) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L609`](../crates/pitboss-cli/src/manifest/schema.rs#L609) |
| `denial_termination_policy` | enum (`adapt` \| `reclassify`) | no | How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected. | [`../crates/pitboss-cli/src/manifest/schema.rs#L620`](../crates/pitboss-cli/src/manifest/schema.rs#L620) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L628`](../crates/pitboss-cli/src/manifest/schema.rs#L628) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L636`](../crates/pitboss-cli/src/manifest/schema.rs#L636) |
| `require_actor_type` | boolean | no | When true, every spawn_worker and spawn_sublead call must name a declared [[worker_type]]/[[sublead_type]]. Default false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L647`](../crates/pitboss-cli/src/manifest/schema.rs#L647) |
| `untyped_actor_policy` | enum (`bridge` \| `block`) | no | Path-B-only behavior for un-typed callers reaching permission_prompt. `bridge` (default) routes to the operator queue; `block` auto-denies via a synthesized empty profile. | [`../crates/pitboss-cli/src/manifest/schema.rs#L664`](../crates/pitboss-cli/src/manifest/schema.rs#L664) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:891`](../crates/pitboss-cli/src/manifest/schema.rs#L891).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L896`](../crates/pitboss-cli/src/manifest/schema.rs#L896) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L902`](../crates/pitboss-cli/src/manifest/schema.rs#L902) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L907`](../crates/pitboss-cli/src/manifest/schema.rs#L907) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L912`](../crates/pitboss-cli/src/manifest/schema.rs#L912) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L917`](../crates/pitboss-cli/src/manifest/schema.rs#L917) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L923`](../crates/pitboss-cli/src/manifest/schema.rs#L923) |

## `[container]` — `ContainerConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:78`](../crates/pitboss-cli/src/manifest/schema.rs#L78).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `image` | text | no | Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest. | [`../crates/pitboss-cli/src/manifest/schema.rs#L84`](../crates/pitboss-cli/src/manifest/schema.rs#L84) |
| `runtime` | enum (`docker` \| `podman` \| `auto`) | no | Container runtime to invoke. "auto" prefers podman. | [`../crates/pitboss-cli/src/manifest/schema.rs#L91`](../crates/pitboss-cli/src/manifest/schema.rs#L91) |
| `extra_args` | string list | no | Verbatim podman/docker run flags. Use for networking, capabilities, DNS, resources, etc. Example: ["--network=corp-fw", "--dns=10.0.0.53", "--cap-add=NET_ADMIN"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L102`](../crates/pitboss-cli/src/manifest/schema.rs#L102) |
| `extra_apt` | string list | no | Debian/Ubuntu packages installed inside the container before pitboss dispatch starts. Adds 30–90s spin-up per dispatch. Example: ["mdbook", "jq"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L117`](../crates/pitboss-cli/src/manifest/schema.rs#L117) |
| `workdir` | path | no | cwd inside the container; defaults to the first mount's container path. | [`../crates/pitboss-cli/src/manifest/schema.rs#L138`](../crates/pitboss-cli/src/manifest/schema.rs#L138) |
| `claude_mount_rw` | boolean | no | Set true to mount the auto-injected ~/.claude as rw (needed for OAuth token refresh). Default false = read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L155`](../crates/pitboss-cli/src/manifest/schema.rs#L155) |

## `[[container.mount]]` — `MountSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:161`](../crates/pitboss-cli/src/manifest/schema.rs#L161).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `host` | path | **yes** | Absolute host path. ~ is expanded. | [`../crates/pitboss-cli/src/manifest/schema.rs#L164`](../crates/pitboss-cli/src/manifest/schema.rs#L164) |
| `container` | path | **yes** | Absolute path inside the container. | [`../crates/pitboss-cli/src/manifest/schema.rs#L167`](../crates/pitboss-cli/src/manifest/schema.rs#L167) |
| `readonly` | boolean | no | Mount read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L171`](../crates/pitboss-cli/src/manifest/schema.rs#L171) |

## `[[task]]` — `Task`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:954`](../crates/pitboss-cli/src/manifest/schema.rs#L954).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L959`](../crates/pitboss-cli/src/manifest/schema.rs#L959) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L964`](../crates/pitboss-cli/src/manifest/schema.rs#L964) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L970`](../crates/pitboss-cli/src/manifest/schema.rs#L970) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L975`](../crates/pitboss-cli/src/manifest/schema.rs#L975) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L981`](../crates/pitboss-cli/src/manifest/schema.rs#L981) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L986`](../crates/pitboss-cli/src/manifest/schema.rs#L986) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L988`](../crates/pitboss-cli/src/manifest/schema.rs#L988) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L994`](../crates/pitboss-cli/src/manifest/schema.rs#L994) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L996`](../crates/pitboss-cli/src/manifest/schema.rs#L996) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L998`](../crates/pitboss-cli/src/manifest/schema.rs#L998) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1003`](../crates/pitboss-cli/src/manifest/schema.rs#L1003) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1009`](../crates/pitboss-cli/src/manifest/schema.rs#L1009) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. The profile's system_prompt is prepended; its env/model/tools defaults fill any slot the task didn't set. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1019`](../crates/pitboss-cli/src/manifest/schema.rs#L1019) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1029`](../crates/pitboss-cli/src/manifest/schema.rs#L1029).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1036`](../crates/pitboss-cli/src/manifest/schema.rs#L1036) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1043`](../crates/pitboss-cli/src/manifest/schema.rs#L1043) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1055`](../crates/pitboss-cli/src/manifest/schema.rs#L1055) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1063`](../crates/pitboss-cli/src/manifest/schema.rs#L1063) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1066`](../crates/pitboss-cli/src/manifest/schema.rs#L1066) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1073`](../crates/pitboss-cli/src/manifest/schema.rs#L1073) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1079`](../crates/pitboss-cli/src/manifest/schema.rs#L1079) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1085`](../crates/pitboss-cli/src/manifest/schema.rs#L1085) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1091`](../crates/pitboss-cli/src/manifest/schema.rs#L1091) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1097`](../crates/pitboss-cli/src/manifest/schema.rs#L1097) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1106`](../crates/pitboss-cli/src/manifest/schema.rs#L1106) |
| `budget_usd` | float | no | Soft cap on the run's total spend (workers + lead + sub-leads). spawn_worker fails once total_spent + reserved + next_estimate > budget; the lead is aborted if a reconciled turn drives total_spent past the cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1117`](../crates/pitboss-cli/src/manifest/schema.rs#L1117) |
| `lead_budget_usd` | float | no | Optional separate cap on lead + sub-lead token spend (orchestration cost). Independent of budget_usd. Lead is aborted when reached. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1127`](../crates/pitboss-cli/src/manifest/schema.rs#L1127) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1136`](../crates/pitboss-cli/src/manifest/schema.rs#L1136) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_b (default) routes claude's per-tool gate through pitboss's permission_prompt MCP tool — layered enforcement (mcp_server tools allowlist, operator policy, typed profile). path_a bypasses claude's gate via --dangerously-skip-permissions and makes pitboss the sole authority. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1154`](../crates/pitboss-cli/src/manifest/schema.rs#L1154) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1163`](../crates/pitboss-cli/src/manifest/schema.rs#L1163) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1170`](../crates/pitboss-cli/src/manifest/schema.rs#L1170) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1177`](../crates/pitboss-cli/src/manifest/schema.rs#L1177) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1185`](../crates/pitboss-cli/src/manifest/schema.rs#L1185) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. The profile's system_prompt is prepended to [lead].prompt; its env/model/tools defaults fill any slot the lead didn't set. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1195`](../crates/pitboss-cli/src/manifest/schema.rs#L1195) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1202`](../crates/pitboss-cli/src/manifest/schema.rs#L1202).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1207`](../crates/pitboss-cli/src/manifest/schema.rs#L1207) |
| `lead_budget_usd` | float | no | Per-sub-lead cap on the sub-lead's own token spend (orchestration cost), independent of budget_usd. Honored when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1212`](../crates/pitboss-cli/src/manifest/schema.rs#L1212) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1217`](../crates/pitboss-cli/src/manifest/schema.rs#L1217) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1222`](../crates/pitboss-cli/src/manifest/schema.rs#L1222) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1228`](../crates/pitboss-cli/src/manifest/schema.rs#L1228) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:483`](../crates/pitboss-cli/src/manifest/schema.rs#L483).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L494`](../crates/pitboss-cli/src/manifest/schema.rs#L494) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:210`](../crates/pitboss-cli/src/manifest/schema.rs#L210).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L216`](../crates/pitboss-cli/src/manifest/schema.rs#L216) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L222`](../crates/pitboss-cli/src/manifest/schema.rs#L222) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L226`](../crates/pitboss-cli/src/manifest/schema.rs#L226) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L233`](../crates/pitboss-cli/src/manifest/schema.rs#L233) |
| `scope` | text | no | Optional injection scope. Form: "type:<id>" — narrows this server to actors spawned under that worker_type/sublead_type. Unset = inject into all actors. | [`../crates/pitboss-cli/src/manifest/schema.rs#L246`](../crates/pitboss-cli/src/manifest/schema.rs#L246) |
| `tools` | string list | no | Optional per-tool allowlist for this MCP server. Path B: enforced at validate + spawn argv + permission_prompt runtime. Path A: no runtime effect (claude bypasses the permission layer). Unset = no restriction. | [`../crates/pitboss-cli/src/manifest/schema.rs#L279`](../crates/pitboss-cli/src/manifest/schema.rs#L279) |

## `[[worker_type]]` — `WorkerType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:790`](../crates/pitboss-cli/src/manifest/schema.rs#L790).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_worker(worker_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L797`](../crates/pitboss-cli/src/manifest/schema.rs#L797) |
| `tools` | string list | no | Allowed tool surface for this worker class. Lead's spawn_worker(tools=...) must be a subset; omit means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L805`](../crates/pitboss-cli/src/manifest/schema.rs#L805) |
| `allowed_models` | string list | no | When non-empty, spawn_worker(model=...) must be in this list. Empty means any model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L813`](../crates/pitboss-cli/src/manifest/schema.rs#L813) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_worker(timeout_secs=...). Lead values clamp DOWN; smaller values pass through. | [`../crates/pitboss-cli/src/manifest/schema.rs#L821`](../crates/pitboss-cli/src/manifest/schema.rs#L821) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. Workers spawned under this worker_type inherit the profile's system_prompt prelude + env/model/tools defaults. | [`../crates/pitboss-cli/src/manifest/schema.rs#L832`](../crates/pitboss-cli/src/manifest/schema.rs#L832) |

## `[[sublead_type]]` — `SubleadType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:840`](../crates/pitboss-cli/src/manifest/schema.rs#L840).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_sublead(sublead_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L847`](../crates/pitboss-cli/src/manifest/schema.rs#L847) |
| `tools` | string list | no | Allowed tool surface for this sub-lead class. spawn_sublead(tools=...) must be a subset; empty means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L854`](../crates/pitboss-cli/src/manifest/schema.rs#L854) |
| `allowed_models` | string list | no | When non-empty, spawn_sublead(model=...) must be in this list. | [`../crates/pitboss-cli/src/manifest/schema.rs#L861`](../crates/pitboss-cli/src/manifest/schema.rs#L861) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_sublead(lead_timeout_secs=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L868`](../crates/pitboss-cli/src/manifest/schema.rs#L868) |
| `max_budget_usd` | float | no | Hard cap on spawn_sublead(budget_usd=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L875`](../crates/pitboss-cli/src/manifest/schema.rs#L875) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. Sub-leads spawned under this sublead_type inherit the profile's system_prompt prelude + env/model/tools defaults. | [`../crates/pitboss-cli/src/manifest/schema.rs#L886`](../crates/pitboss-cli/src/manifest/schema.rs#L886) |

## `[communication]` — `CommunicationConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:316`](../crates/pitboss-cli/src/manifest/schema.rs#L316).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `mode` | enum (`disabled` \| `parent_child`) | no | Actor communication policy mode. "disabled" hides the message_* and artifact_* tools (conservative default). "parent_child" enables them with parent/sublead/worker authz. | [`../crates/pitboss-cli/src/manifest/schema.rs#L323`](../crates/pitboss-cli/src/manifest/schema.rs#L323) |
| `max_message_bytes` | integer | no | Maximum UTF-8 body size accepted by message_send. | [`../crates/pitboss-cli/src/manifest/schema.rs#L329`](../crates/pitboss-cli/src/manifest/schema.rs#L329) |
| `max_artifact_bytes` | integer | no | Maximum decoded artifact payload size accepted by artifact_put. | [`../crates/pitboss-cli/src/manifest/schema.rs#L335`](../crates/pitboss-cli/src/manifest/schema.rs#L335) |
| `max_artifacts_per_actor` | integer | no | Maximum artifacts one actor may publish in a single run. | [`../crates/pitboss-cli/src/manifest/schema.rs#L341`](../crates/pitboss-cli/src/manifest/schema.rs#L341) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:938`](../crates/pitboss-cli/src/manifest/schema.rs#L938).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L943`](../crates/pitboss-cli/src/manifest/schema.rs#L943) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L949`](../crates/pitboss-cli/src/manifest/schema.rs#L949) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:460`](../crates/pitboss-cli/src/manifest/schema.rs#L460).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L471`](../crates/pitboss-cli/src/manifest/schema.rs#L471) |

