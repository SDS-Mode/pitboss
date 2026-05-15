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
| `claude_setting_sources` | text | no | --setting-sources override for every worker claude spawn. Comma-separated subset of {user, project, local}. Defaults to project,local in container-dispatch and unfiltered on host. | [`../crates/pitboss-cli/src/manifest/schema.rs#L603`](../crates/pitboss-cli/src/manifest/schema.rs#L603) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L631`](../crates/pitboss-cli/src/manifest/schema.rs#L631) |
| `denial_termination_policy` | enum (`adapt` \| `reclassify`) | no | How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected. | [`../crates/pitboss-cli/src/manifest/schema.rs#L642`](../crates/pitboss-cli/src/manifest/schema.rs#L642) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L650`](../crates/pitboss-cli/src/manifest/schema.rs#L650) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L658`](../crates/pitboss-cli/src/manifest/schema.rs#L658) |
| `require_actor_type` | boolean | no | When true, every spawn_worker and spawn_sublead call must name a declared [[worker_type]]/[[sublead_type]]. Default false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L669`](../crates/pitboss-cli/src/manifest/schema.rs#L669) |
| `untyped_actor_policy` | enum (`bridge` \| `block`) | no | Path-B-only behavior for un-typed callers reaching permission_prompt. `bridge` (default) routes to the operator queue; `block` auto-denies via a synthesized empty profile. | [`../crates/pitboss-cli/src/manifest/schema.rs#L686`](../crates/pitboss-cli/src/manifest/schema.rs#L686) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:914`](../crates/pitboss-cli/src/manifest/schema.rs#L914).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L919`](../crates/pitboss-cli/src/manifest/schema.rs#L919) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L925`](../crates/pitboss-cli/src/manifest/schema.rs#L925) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L930`](../crates/pitboss-cli/src/manifest/schema.rs#L930) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L935`](../crates/pitboss-cli/src/manifest/schema.rs#L935) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L940`](../crates/pitboss-cli/src/manifest/schema.rs#L940) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L946`](../crates/pitboss-cli/src/manifest/schema.rs#L946) |

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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:977`](../crates/pitboss-cli/src/manifest/schema.rs#L977).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L982`](../crates/pitboss-cli/src/manifest/schema.rs#L982) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L987`](../crates/pitboss-cli/src/manifest/schema.rs#L987) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L993`](../crates/pitboss-cli/src/manifest/schema.rs#L993) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L998`](../crates/pitboss-cli/src/manifest/schema.rs#L998) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1004`](../crates/pitboss-cli/src/manifest/schema.rs#L1004) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1009`](../crates/pitboss-cli/src/manifest/schema.rs#L1009) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1011`](../crates/pitboss-cli/src/manifest/schema.rs#L1011) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1017`](../crates/pitboss-cli/src/manifest/schema.rs#L1017) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1019`](../crates/pitboss-cli/src/manifest/schema.rs#L1019) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1021`](../crates/pitboss-cli/src/manifest/schema.rs#L1021) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1026`](../crates/pitboss-cli/src/manifest/schema.rs#L1026) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1032`](../crates/pitboss-cli/src/manifest/schema.rs#L1032) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. The profile's system_prompt is prepended; its env/model/tools defaults fill any slot the task didn't set. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1042`](../crates/pitboss-cli/src/manifest/schema.rs#L1042) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1052`](../crates/pitboss-cli/src/manifest/schema.rs#L1052).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1059`](../crates/pitboss-cli/src/manifest/schema.rs#L1059) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1066`](../crates/pitboss-cli/src/manifest/schema.rs#L1066) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1078`](../crates/pitboss-cli/src/manifest/schema.rs#L1078) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1086`](../crates/pitboss-cli/src/manifest/schema.rs#L1086) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1089`](../crates/pitboss-cli/src/manifest/schema.rs#L1089) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1096`](../crates/pitboss-cli/src/manifest/schema.rs#L1096) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1102`](../crates/pitboss-cli/src/manifest/schema.rs#L1102) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1108`](../crates/pitboss-cli/src/manifest/schema.rs#L1108) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1114`](../crates/pitboss-cli/src/manifest/schema.rs#L1114) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1120`](../crates/pitboss-cli/src/manifest/schema.rs#L1120) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1129`](../crates/pitboss-cli/src/manifest/schema.rs#L1129) |
| `budget_usd` | float | no | Soft cap on the run's total spend (workers + lead + sub-leads). spawn_worker fails once total_spent + reserved + next_estimate > budget; the lead is aborted if a reconciled turn drives total_spent past the cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1140`](../crates/pitboss-cli/src/manifest/schema.rs#L1140) |
| `lead_budget_usd` | float | no | Optional separate cap on lead + sub-lead token spend (orchestration cost). Independent of budget_usd. Lead is aborted when reached. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1150`](../crates/pitboss-cli/src/manifest/schema.rs#L1150) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1159`](../crates/pitboss-cli/src/manifest/schema.rs#L1159) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_b (default) routes claude's per-tool gate through pitboss's permission_prompt MCP tool — layered enforcement (mcp_server tools allowlist, operator policy, typed profile). path_a bypasses claude's gate via --dangerously-skip-permissions and makes pitboss the sole authority. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1177`](../crates/pitboss-cli/src/manifest/schema.rs#L1177) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1186`](../crates/pitboss-cli/src/manifest/schema.rs#L1186) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1193`](../crates/pitboss-cli/src/manifest/schema.rs#L1193) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1200`](../crates/pitboss-cli/src/manifest/schema.rs#L1200) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1208`](../crates/pitboss-cli/src/manifest/schema.rs#L1208) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. The profile's system_prompt is prepended to [lead].prompt; its env/model/tools defaults fill any slot the lead didn't set. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1218`](../crates/pitboss-cli/src/manifest/schema.rs#L1218) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1225`](../crates/pitboss-cli/src/manifest/schema.rs#L1225).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1230`](../crates/pitboss-cli/src/manifest/schema.rs#L1230) |
| `lead_budget_usd` | float | no | Per-sub-lead cap on the sub-lead's own token spend (orchestration cost), independent of budget_usd. Honored when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1235`](../crates/pitboss-cli/src/manifest/schema.rs#L1235) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1240`](../crates/pitboss-cli/src/manifest/schema.rs#L1240) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1245`](../crates/pitboss-cli/src/manifest/schema.rs#L1245) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1251`](../crates/pitboss-cli/src/manifest/schema.rs#L1251) |

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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:813`](../crates/pitboss-cli/src/manifest/schema.rs#L813).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_worker(worker_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L820`](../crates/pitboss-cli/src/manifest/schema.rs#L820) |
| `tools` | string list | no | Allowed tool surface for this worker class. Lead's spawn_worker(tools=...) must be a subset; omit means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L828`](../crates/pitboss-cli/src/manifest/schema.rs#L828) |
| `allowed_models` | string list | no | When non-empty, spawn_worker(model=...) must be in this list. Empty means any model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L836`](../crates/pitboss-cli/src/manifest/schema.rs#L836) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_worker(timeout_secs=...). Lead values clamp DOWN; smaller values pass through. | [`../crates/pitboss-cli/src/manifest/schema.rs#L844`](../crates/pitboss-cli/src/manifest/schema.rs#L844) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. Workers spawned under this worker_type inherit the profile's system_prompt prelude + env/model/tools defaults. | [`../crates/pitboss-cli/src/manifest/schema.rs#L855`](../crates/pitboss-cli/src/manifest/schema.rs#L855) |

## `[[sublead_type]]` — `SubleadType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:863`](../crates/pitboss-cli/src/manifest/schema.rs#L863).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_sublead(sublead_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L870`](../crates/pitboss-cli/src/manifest/schema.rs#L870) |
| `tools` | string list | no | Allowed tool surface for this sub-lead class. spawn_sublead(tools=...) must be a subset; empty means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L877`](../crates/pitboss-cli/src/manifest/schema.rs#L877) |
| `allowed_models` | string list | no | When non-empty, spawn_sublead(model=...) must be in this list. | [`../crates/pitboss-cli/src/manifest/schema.rs#L884`](../crates/pitboss-cli/src/manifest/schema.rs#L884) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_sublead(lead_timeout_secs=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L891`](../crates/pitboss-cli/src/manifest/schema.rs#L891) |
| `max_budget_usd` | float | no | Hard cap on spawn_sublead(budget_usd=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L898`](../crates/pitboss-cli/src/manifest/schema.rs#L898) |
| `agent_profile` | text | no | Reference to an [[agent_profile]].id. Sub-leads spawned under this sublead_type inherit the profile's system_prompt prelude + env/model/tools defaults. | [`../crates/pitboss-cli/src/manifest/schema.rs#L909`](../crates/pitboss-cli/src/manifest/schema.rs#L909) |

## `[communication]` — `CommunicationConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:316`](../crates/pitboss-cli/src/manifest/schema.rs#L316).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `mode` | enum (`disabled` \| `parent_child`) | no | Actor communication policy mode. "disabled" hides the message_* and artifact_* tools (conservative default). "parent_child" enables them with parent/sublead/worker authz. | [`../crates/pitboss-cli/src/manifest/schema.rs#L323`](../crates/pitboss-cli/src/manifest/schema.rs#L323) |
| `max_message_bytes` | integer | no | Maximum UTF-8 body size accepted by message_send. | [`../crates/pitboss-cli/src/manifest/schema.rs#L329`](../crates/pitboss-cli/src/manifest/schema.rs#L329) |
| `max_artifact_bytes` | integer | no | Maximum decoded artifact payload size accepted by artifact_put. | [`../crates/pitboss-cli/src/manifest/schema.rs#L335`](../crates/pitboss-cli/src/manifest/schema.rs#L335) |
| `max_artifacts_per_actor` | integer | no | Maximum artifacts one actor may publish in a single run. | [`../crates/pitboss-cli/src/manifest/schema.rs#L341`](../crates/pitboss-cli/src/manifest/schema.rs#L341) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:961`](../crates/pitboss-cli/src/manifest/schema.rs#L961).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L966`](../crates/pitboss-cli/src/manifest/schema.rs#L966) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L972`](../crates/pitboss-cli/src/manifest/schema.rs#L972) |

## `[[agent_profile]]` — `AgentProfile`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:755`](../crates/pitboss-cli/src/manifest/schema.rs#L755).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id referenced from [lead]/[[task]]/[[worker_type]]/[[sublead_type]] .agent_profile. Match: ^[A-Za-z0-9_/-]+$. The pitboss/ namespace is reserved for built-ins. | [`../crates/pitboss-cli/src/manifest/schema.rs#L764`](../crates/pitboss-cli/src/manifest/schema.rs#L764) |
| `system_prompt` | text (multi-line) | no | Role-shared prelude prepended to the operator's prompt with `\n\n--- TASK ---\n\n` separator. | [`../crates/pitboss-cli/src/manifest/schema.rs#L774`](../crates/pitboss-cli/src/manifest/schema.rs#L774) |
| `model` | text | no | Default Claude model id when the actor's `model =` is unset. Not a cap — operator config wins. | [`../crates/pitboss-cli/src/manifest/schema.rs#L783`](../crates/pitboss-cli/src/manifest/schema.rs#L783) |
| `env` | key-value map | no | Env merged between [defaults].env and the per-actor env. Operator wins on collision. | [`../crates/pitboss-cli/src/manifest/schema.rs#L791`](../crates/pitboss-cli/src/manifest/schema.rs#L791) |
| `tools` | string list | no | Default tool allowlist when the actor's `tools =` is unset. Not a cap — operator config wins. | [`../crates/pitboss-cli/src/manifest/schema.rs#L800`](../crates/pitboss-cli/src/manifest/schema.rs#L800) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:460`](../crates/pitboss-cli/src/manifest/schema.rs#L460).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L471`](../crates/pitboss-cli/src/manifest/schema.rs#L471) |

