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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:524`](../crates/pitboss-cli/src/manifest/schema.rs#L524).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `name` | text | no | Human-readable label used to group related runs in the console (e.g. "build-db", "nightly-sync"). When unset, the manifest filename is used as fallback. | [`../crates/pitboss-cli/src/manifest/schema.rs#L535`](../crates/pitboss-cli/src/manifest/schema.rs#L535) |
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L544`](../crates/pitboss-cli/src/manifest/schema.rs#L544) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L551`](../crates/pitboss-cli/src/manifest/schema.rs#L551) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L557`](../crates/pitboss-cli/src/manifest/schema.rs#L557) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L565`](../crates/pitboss-cli/src/manifest/schema.rs#L565) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L572`](../crates/pitboss-cli/src/manifest/schema.rs#L572) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L600`](../crates/pitboss-cli/src/manifest/schema.rs#L600) |
| `denial_termination_policy` | enum (`adapt` \| `reclassify`) | no | How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected. | [`../crates/pitboss-cli/src/manifest/schema.rs#L611`](../crates/pitboss-cli/src/manifest/schema.rs#L611) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L619`](../crates/pitboss-cli/src/manifest/schema.rs#L619) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L627`](../crates/pitboss-cli/src/manifest/schema.rs#L627) |
| `require_actor_type` | boolean | no | When true, every spawn_worker and spawn_sublead call must name a declared [[worker_type]]/[[sublead_type]]. Default false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L638`](../crates/pitboss-cli/src/manifest/schema.rs#L638) |
| `untyped_actor_policy` | enum (`bridge` \| `block`) | no | Path-B-only behavior for un-typed callers reaching permission_prompt. `bridge` (default) routes to the operator queue; `block` auto-denies via a synthesized empty profile. | [`../crates/pitboss-cli/src/manifest/schema.rs#L655`](../crates/pitboss-cli/src/manifest/schema.rs#L655) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:793`](../crates/pitboss-cli/src/manifest/schema.rs#L793).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L798`](../crates/pitboss-cli/src/manifest/schema.rs#L798) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L804`](../crates/pitboss-cli/src/manifest/schema.rs#L804) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L809`](../crates/pitboss-cli/src/manifest/schema.rs#L809) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L814`](../crates/pitboss-cli/src/manifest/schema.rs#L814) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L819`](../crates/pitboss-cli/src/manifest/schema.rs#L819) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L825`](../crates/pitboss-cli/src/manifest/schema.rs#L825) |

## `[container]` — `ContainerConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:77`](../crates/pitboss-cli/src/manifest/schema.rs#L77).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `image` | text | no | Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest. | [`../crates/pitboss-cli/src/manifest/schema.rs#L83`](../crates/pitboss-cli/src/manifest/schema.rs#L83) |
| `runtime` | enum (`docker` \| `podman` \| `auto`) | no | Container runtime to invoke. "auto" prefers podman. | [`../crates/pitboss-cli/src/manifest/schema.rs#L90`](../crates/pitboss-cli/src/manifest/schema.rs#L90) |
| `extra_args` | string list | no | Verbatim podman/docker run flags. Use for networking, capabilities, DNS, resources, etc. Example: ["--network=corp-fw", "--dns=10.0.0.53", "--cap-add=NET_ADMIN"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L101`](../crates/pitboss-cli/src/manifest/schema.rs#L101) |
| `extra_apt` | string list | no | Debian/Ubuntu packages installed inside the container before pitboss dispatch starts. Adds 30–90s spin-up per dispatch. Example: ["mdbook", "jq"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L116`](../crates/pitboss-cli/src/manifest/schema.rs#L116) |
| `workdir` | path | no | cwd inside the container; defaults to the first mount's container path. | [`../crates/pitboss-cli/src/manifest/schema.rs#L137`](../crates/pitboss-cli/src/manifest/schema.rs#L137) |
| `claude_mount_rw` | boolean | no | Set true to mount the auto-injected ~/.claude as rw (needed for OAuth token refresh). Default false = read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L154`](../crates/pitboss-cli/src/manifest/schema.rs#L154) |

## `[[container.mount]]` — `MountSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:160`](../crates/pitboss-cli/src/manifest/schema.rs#L160).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `host` | path | **yes** | Absolute host path. ~ is expanded. | [`../crates/pitboss-cli/src/manifest/schema.rs#L163`](../crates/pitboss-cli/src/manifest/schema.rs#L163) |
| `container` | path | **yes** | Absolute path inside the container. | [`../crates/pitboss-cli/src/manifest/schema.rs#L166`](../crates/pitboss-cli/src/manifest/schema.rs#L166) |
| `readonly` | boolean | no | Mount read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L170`](../crates/pitboss-cli/src/manifest/schema.rs#L170) |

## `[[task]]` — `Task`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:856`](../crates/pitboss-cli/src/manifest/schema.rs#L856).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L861`](../crates/pitboss-cli/src/manifest/schema.rs#L861) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L866`](../crates/pitboss-cli/src/manifest/schema.rs#L866) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L872`](../crates/pitboss-cli/src/manifest/schema.rs#L872) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L877`](../crates/pitboss-cli/src/manifest/schema.rs#L877) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L883`](../crates/pitboss-cli/src/manifest/schema.rs#L883) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L888`](../crates/pitboss-cli/src/manifest/schema.rs#L888) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L890`](../crates/pitboss-cli/src/manifest/schema.rs#L890) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L896`](../crates/pitboss-cli/src/manifest/schema.rs#L896) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L898`](../crates/pitboss-cli/src/manifest/schema.rs#L898) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L900`](../crates/pitboss-cli/src/manifest/schema.rs#L900) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L905`](../crates/pitboss-cli/src/manifest/schema.rs#L905) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L911`](../crates/pitboss-cli/src/manifest/schema.rs#L911) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:921`](../crates/pitboss-cli/src/manifest/schema.rs#L921).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L928`](../crates/pitboss-cli/src/manifest/schema.rs#L928) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L935`](../crates/pitboss-cli/src/manifest/schema.rs#L935) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L947`](../crates/pitboss-cli/src/manifest/schema.rs#L947) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L955`](../crates/pitboss-cli/src/manifest/schema.rs#L955) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L958`](../crates/pitboss-cli/src/manifest/schema.rs#L958) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L965`](../crates/pitboss-cli/src/manifest/schema.rs#L965) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L971`](../crates/pitboss-cli/src/manifest/schema.rs#L971) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L977`](../crates/pitboss-cli/src/manifest/schema.rs#L977) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L983`](../crates/pitboss-cli/src/manifest/schema.rs#L983) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L989`](../crates/pitboss-cli/src/manifest/schema.rs#L989) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L998`](../crates/pitboss-cli/src/manifest/schema.rs#L998) |
| `budget_usd` | float | no | Soft cap on the run's total spend (workers + lead + sub-leads). spawn_worker fails once total_spent + reserved + next_estimate > budget; the lead is aborted if a reconciled turn drives total_spent past the cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1009`](../crates/pitboss-cli/src/manifest/schema.rs#L1009) |
| `lead_budget_usd` | float | no | Optional separate cap on lead + sub-lead token spend (orchestration cost). Independent of budget_usd. Lead is aborted when reached. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1019`](../crates/pitboss-cli/src/manifest/schema.rs#L1019) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1028`](../crates/pitboss-cli/src/manifest/schema.rs#L1028) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_b (default) routes claude's per-tool gate through pitboss's permission_prompt MCP tool — layered enforcement (mcp_server tools allowlist, operator policy, typed profile). path_a bypasses claude's gate via --dangerously-skip-permissions and makes pitboss the sole authority. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1046`](../crates/pitboss-cli/src/manifest/schema.rs#L1046) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1055`](../crates/pitboss-cli/src/manifest/schema.rs#L1055) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1062`](../crates/pitboss-cli/src/manifest/schema.rs#L1062) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1069`](../crates/pitboss-cli/src/manifest/schema.rs#L1069) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1077`](../crates/pitboss-cli/src/manifest/schema.rs#L1077) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1084`](../crates/pitboss-cli/src/manifest/schema.rs#L1084).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1089`](../crates/pitboss-cli/src/manifest/schema.rs#L1089) |
| `lead_budget_usd` | float | no | Per-sub-lead cap on the sub-lead's own token spend (orchestration cost), independent of budget_usd. Honored when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1094`](../crates/pitboss-cli/src/manifest/schema.rs#L1094) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1099`](../crates/pitboss-cli/src/manifest/schema.rs#L1099) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1104`](../crates/pitboss-cli/src/manifest/schema.rs#L1104) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1110`](../crates/pitboss-cli/src/manifest/schema.rs#L1110) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:474`](../crates/pitboss-cli/src/manifest/schema.rs#L474).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L485`](../crates/pitboss-cli/src/manifest/schema.rs#L485) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:209`](../crates/pitboss-cli/src/manifest/schema.rs#L209).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L215`](../crates/pitboss-cli/src/manifest/schema.rs#L215) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L221`](../crates/pitboss-cli/src/manifest/schema.rs#L221) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L225`](../crates/pitboss-cli/src/manifest/schema.rs#L225) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L232`](../crates/pitboss-cli/src/manifest/schema.rs#L232) |
| `scope` | text | no | Optional injection scope. Form: "type:<id>" — narrows this server to actors spawned under that worker_type/sublead_type. Unset = inject into all actors. | [`../crates/pitboss-cli/src/manifest/schema.rs#L245`](../crates/pitboss-cli/src/manifest/schema.rs#L245) |
| `tools` | string list | no | Optional per-tool allowlist for this MCP server. Path B: enforced at validate + spawn argv + permission_prompt runtime. Path A: no runtime effect (claude bypasses the permission layer). Unset = no restriction. | [`../crates/pitboss-cli/src/manifest/schema.rs#L278`](../crates/pitboss-cli/src/manifest/schema.rs#L278) |

## `[[worker_type]]` — `WorkerType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:714`](../crates/pitboss-cli/src/manifest/schema.rs#L714).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_worker(worker_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L721`](../crates/pitboss-cli/src/manifest/schema.rs#L721) |
| `tools` | string list | no | Allowed tool surface for this worker class. Lead's spawn_worker(tools=...) must be a subset; omit means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L729`](../crates/pitboss-cli/src/manifest/schema.rs#L729) |
| `allowed_models` | string list | no | When non-empty, spawn_worker(model=...) must be in this list. Empty means any model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L737`](../crates/pitboss-cli/src/manifest/schema.rs#L737) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_worker(timeout_secs=...). Lead values clamp DOWN; smaller values pass through. | [`../crates/pitboss-cli/src/manifest/schema.rs#L745`](../crates/pitboss-cli/src/manifest/schema.rs#L745) |

## `[[sublead_type]]` — `SubleadType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:753`](../crates/pitboss-cli/src/manifest/schema.rs#L753).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_sublead(sublead_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L760`](../crates/pitboss-cli/src/manifest/schema.rs#L760) |
| `tools` | string list | no | Allowed tool surface for this sub-lead class. spawn_sublead(tools=...) must be a subset; empty means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L767`](../crates/pitboss-cli/src/manifest/schema.rs#L767) |
| `allowed_models` | string list | no | When non-empty, spawn_sublead(model=...) must be in this list. | [`../crates/pitboss-cli/src/manifest/schema.rs#L774`](../crates/pitboss-cli/src/manifest/schema.rs#L774) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_sublead(lead_timeout_secs=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L781`](../crates/pitboss-cli/src/manifest/schema.rs#L781) |
| `max_budget_usd` | float | no | Hard cap on spawn_sublead(budget_usd=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L788`](../crates/pitboss-cli/src/manifest/schema.rs#L788) |

## `[communication]` — `CommunicationConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:315`](../crates/pitboss-cli/src/manifest/schema.rs#L315).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `mode` | enum (`disabled` \| `parent_child`) | no | Actor communication policy mode. "disabled" hides the message_* and artifact_* tools (conservative default). "parent_child" enables them with parent/sublead/worker authz. | [`../crates/pitboss-cli/src/manifest/schema.rs#L322`](../crates/pitboss-cli/src/manifest/schema.rs#L322) |
| `max_message_bytes` | integer | no | Maximum UTF-8 body size accepted by message_send. | [`../crates/pitboss-cli/src/manifest/schema.rs#L328`](../crates/pitboss-cli/src/manifest/schema.rs#L328) |
| `max_artifact_bytes` | integer | no | Maximum decoded artifact payload size accepted by artifact_put. | [`../crates/pitboss-cli/src/manifest/schema.rs#L334`](../crates/pitboss-cli/src/manifest/schema.rs#L334) |
| `max_artifacts_per_actor` | integer | no | Maximum artifacts one actor may publish in a single run. | [`../crates/pitboss-cli/src/manifest/schema.rs#L340`](../crates/pitboss-cli/src/manifest/schema.rs#L340) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:840`](../crates/pitboss-cli/src/manifest/schema.rs#L840).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L845`](../crates/pitboss-cli/src/manifest/schema.rs#L845) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L851`](../crates/pitboss-cli/src/manifest/schema.rs#L851) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:451`](../crates/pitboss-cli/src/manifest/schema.rs#L451).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L462`](../crates/pitboss-cli/src/manifest/schema.rs#L462) |

