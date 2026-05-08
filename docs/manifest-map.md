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

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:490`](../crates/pitboss-cli/src/manifest/schema.rs#L490).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `name` | text | no | Human-readable label used to group related runs in the console (e.g. "build-db", "nightly-sync"). When unset, the manifest filename is used as fallback. | [`../crates/pitboss-cli/src/manifest/schema.rs#L501`](../crates/pitboss-cli/src/manifest/schema.rs#L501) |
| `max_parallel_tasks` | integer | no | Flat-mode concurrency cap for [[task]] runs. Default 4. Overridden by ANTHROPIC_MAX_CONCURRENT. | [`../crates/pitboss-cli/src/manifest/schema.rs#L510`](../crates/pitboss-cli/src/manifest/schema.rs#L510) |
| `halt_on_failure` | boolean | no | Stop remaining flat-mode tasks on first failure. | [`../crates/pitboss-cli/src/manifest/schema.rs#L517`](../crates/pitboss-cli/src/manifest/schema.rs#L517) |
| `run_dir` | path | no | Where per-run artifacts land. Default ~/.local/share/pitboss/runs. | [`../crates/pitboss-cli/src/manifest/schema.rs#L523`](../crates/pitboss-cli/src/manifest/schema.rs#L523) |
| `worktree_cleanup` | enum (`always` \| `on_success` \| `never`) | no | What to do with each worker's git worktree after it finishes. | [`../crates/pitboss-cli/src/manifest/schema.rs#L531`](../crates/pitboss-cli/src/manifest/schema.rs#L531) |
| `emit_event_stream` | boolean | no | Write a JSONL event stream alongside summary.jsonl. | [`../crates/pitboss-cli/src/manifest/schema.rs#L538`](../crates/pitboss-cli/src/manifest/schema.rs#L538) |
| `default_approval_policy` | enum (`block` \| `auto_approve` \| `auto_reject`) | no | Default action for request_approval / propose_plan when no rule matches. `auto_approve`/`auto_reject` are unconditional; `block` routes to the operator if attached, else queues. | [`../crates/pitboss-cli/src/manifest/schema.rs#L566`](../crates/pitboss-cli/src/manifest/schema.rs#L566) |
| `denial_termination_policy` | enum (`adapt` \| `reclassify`) | no | How a denied permission_prompt affects the actor's terminal status. `adapt` (default) keeps the actor's exit code as-is; `reclassify` re-labels clean exits within 30s of a denial as ApprovalRejected. | [`../crates/pitboss-cli/src/manifest/schema.rs#L577`](../crates/pitboss-cli/src/manifest/schema.rs#L577) |
| `dump_shared_store` | boolean | no | Write shared-store.json into the run directory on finalize. | [`../crates/pitboss-cli/src/manifest/schema.rs#L585`](../crates/pitboss-cli/src/manifest/schema.rs#L585) |
| `require_plan_approval` | boolean | no | When true, spawn_worker is blocked until propose_plan has been approved. | [`../crates/pitboss-cli/src/manifest/schema.rs#L593`](../crates/pitboss-cli/src/manifest/schema.rs#L593) |
| `require_actor_type` | boolean | no | When true, every spawn_worker and spawn_sublead call must name a declared [[worker_type]]/[[sublead_type]]. Default false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L604`](../crates/pitboss-cli/src/manifest/schema.rs#L604) |
| `untyped_actor_policy` | enum (`bridge` \| `block`) | no | Path-B-only behavior for un-typed callers reaching permission_prompt. `bridge` (default) routes to the operator queue; `block` auto-denies via a synthesized empty profile. | [`../crates/pitboss-cli/src/manifest/schema.rs#L621`](../crates/pitboss-cli/src/manifest/schema.rs#L621) |

## `[defaults]` — `Defaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:759`](../crates/pitboss-cli/src/manifest/schema.rs#L759).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `model` | text | no | Claude model id (e.g. claude-haiku-4-5, claude-sonnet-4-6, claude-opus-4-7). | [`../crates/pitboss-cli/src/manifest/schema.rs#L764`](../crates/pitboss-cli/src/manifest/schema.rs#L764) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Maps to the claude --effort flag. | [`../crates/pitboss-cli/src/manifest/schema.rs#L770`](../crates/pitboss-cli/src/manifest/schema.rs#L770) |
| `tools` | string list | no | Allowed tool surface. Pitboss auto-appends its MCP tools for leads and workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L775`](../crates/pitboss-cli/src/manifest/schema.rs#L775) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. No default (no cap). | [`../crates/pitboss-cli/src/manifest/schema.rs#L780`](../crates/pitboss-cli/src/manifest/schema.rs#L780) |
| `use_worktree` | boolean | no | Isolate each worker in a git worktree. Default true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L785`](../crates/pitboss-cli/src/manifest/schema.rs#L785) |
| `env` | key-value map | no | Environment variables passed to the claude subprocess. | [`../crates/pitboss-cli/src/manifest/schema.rs#L791`](../crates/pitboss-cli/src/manifest/schema.rs#L791) |

## `[container]` — `ContainerConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:77`](../crates/pitboss-cli/src/manifest/schema.rs#L77).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `image` | text | no | Container image reference. Defaults to ghcr.io/sds-mode/pitboss-with-claude:latest. | [`../crates/pitboss-cli/src/manifest/schema.rs#L83`](../crates/pitboss-cli/src/manifest/schema.rs#L83) |
| `runtime` | enum (`docker` \| `podman` \| `auto`) | no | Container runtime to invoke. "auto" prefers podman. | [`../crates/pitboss-cli/src/manifest/schema.rs#L90`](../crates/pitboss-cli/src/manifest/schema.rs#L90) |
| `extra_args` | string list | no | Verbatim podman/docker run flags. Use for networking, capabilities, DNS, resources, etc. Example: ["--network=corp-fw", "--dns=10.0.0.53", "--cap-add=NET_ADMIN"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L101`](../crates/pitboss-cli/src/manifest/schema.rs#L101) |
| `extra_apt` | string list | no | Debian/Ubuntu packages installed inside the container before pitboss dispatch starts. Adds 30–90s spin-up per dispatch. Example: ["mdbook", "jq"]. | [`../crates/pitboss-cli/src/manifest/schema.rs#L116`](../crates/pitboss-cli/src/manifest/schema.rs#L116) |
| `workdir` | path | no | cwd inside the container; defaults to the first mount's container path. | [`../crates/pitboss-cli/src/manifest/schema.rs#L137`](../crates/pitboss-cli/src/manifest/schema.rs#L137) |

## `[[container.mount]]` — `MountSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:143`](../crates/pitboss-cli/src/manifest/schema.rs#L143).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `host` | path | **yes** | Absolute host path. ~ is expanded. | [`../crates/pitboss-cli/src/manifest/schema.rs#L146`](../crates/pitboss-cli/src/manifest/schema.rs#L146) |
| `container` | path | **yes** | Absolute path inside the container. | [`../crates/pitboss-cli/src/manifest/schema.rs#L149`](../crates/pitboss-cli/src/manifest/schema.rs#L149) |
| `readonly` | boolean | no | Mount read-only. | [`../crates/pitboss-cli/src/manifest/schema.rs#L153`](../crates/pitboss-cli/src/manifest/schema.rs#L153) |

## `[[task]]` — `Task`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:822`](../crates/pitboss-cli/src/manifest/schema.rs#L822).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug. Alphanumeric + _ + -. Used in logs and worktree names. | [`../crates/pitboss-cli/src/manifest/schema.rs#L827`](../crates/pitboss-cli/src/manifest/schema.rs#L827) |
| `directory` | path | **yes** | Working directory. Must be inside a git repo if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L832`](../crates/pitboss-cli/src/manifest/schema.rs#L832) |
| `prompt` | text (multi-line) | no | Prompt body sent to claude via -p. Mutually exclusive with `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L838`](../crates/pitboss-cli/src/manifest/schema.rs#L838) |
| `template` | text | no | Reference to a [[template]] entry. Mutually exclusive with `prompt`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L843`](../crates/pitboss-cli/src/manifest/schema.rs#L843) |
| `vars` | key-value map | no | Substitutions for {placeholders} when using `template`. | [`../crates/pitboss-cli/src/manifest/schema.rs#L849`](../crates/pitboss-cli/src/manifest/schema.rs#L849) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L854`](../crates/pitboss-cli/src/manifest/schema.rs#L854) |
| `model` | text | no | Per-task override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L856`](../crates/pitboss-cli/src/manifest/schema.rs#L856) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-task override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L862`](../crates/pitboss-cli/src/manifest/schema.rs#L862) |
| `tools` | string list | no | Per-task override of [defaults].tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L864`](../crates/pitboss-cli/src/manifest/schema.rs#L864) |
| `timeout_secs` | integer | no | Per-task wall-clock cap. | [`../crates/pitboss-cli/src/manifest/schema.rs#L866`](../crates/pitboss-cli/src/manifest/schema.rs#L866) |
| `use_worktree` | boolean | no | Per-task override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L871`](../crates/pitboss-cli/src/manifest/schema.rs#L871) |
| `env` | key-value map | no | Per-task env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L877`](../crates/pitboss-cli/src/manifest/schema.rs#L877) |

## `[lead]` — `Lead`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:887`](../crates/pitboss-cli/src/manifest/schema.rs#L887).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique slug used as the TUI tile label and in run artifact paths. | [`../crates/pitboss-cli/src/manifest/schema.rs#L894`](../crates/pitboss-cli/src/manifest/schema.rs#L894) |
| `directory` | path | **yes** | Working directory for the lead's claude subprocess. Must be a git work-tree if use_worktree = true. | [`../crates/pitboss-cli/src/manifest/schema.rs#L901`](../crates/pitboss-cli/src/manifest/schema.rs#L901) |
| `prompt` | text (multi-line) | **yes** | Operator instructions passed to claude via -p. Must appear before any [lead.X] subtable in the source. | [`../crates/pitboss-cli/src/manifest/schema.rs#L913`](../crates/pitboss-cli/src/manifest/schema.rs#L913) |
| `branch` | text | no | Worktree branch name. Auto-generated if omitted. | [`../crates/pitboss-cli/src/manifest/schema.rs#L921`](../crates/pitboss-cli/src/manifest/schema.rs#L921) |
| `model` | text | no | Per-lead override of [defaults].model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L924`](../crates/pitboss-cli/src/manifest/schema.rs#L924) |
| `effort` | enum (`low` \| `medium` \| `high` \| `xhigh` \| `max`) | no | Per-lead override of [defaults].effort. | [`../crates/pitboss-cli/src/manifest/schema.rs#L931`](../crates/pitboss-cli/src/manifest/schema.rs#L931) |
| `tools` | string list | no | Per-lead override of [defaults].tools. Pitboss auto-appends its MCP tools. | [`../crates/pitboss-cli/src/manifest/schema.rs#L937`](../crates/pitboss-cli/src/manifest/schema.rs#L937) |
| `timeout_secs` | integer | no | Per-actor subprocess wall-clock cap (claude --timeout). | [`../crates/pitboss-cli/src/manifest/schema.rs#L943`](../crates/pitboss-cli/src/manifest/schema.rs#L943) |
| `use_worktree` | boolean | no | Per-lead override of [defaults].use_worktree. | [`../crates/pitboss-cli/src/manifest/schema.rs#L949`](../crates/pitboss-cli/src/manifest/schema.rs#L949) |
| `env` | key-value map | no | Per-lead env vars merged on top of [defaults].env. | [`../crates/pitboss-cli/src/manifest/schema.rs#L955`](../crates/pitboss-cli/src/manifest/schema.rs#L955) |
| `max_workers` | integer | no | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. | [`../crates/pitboss-cli/src/manifest/schema.rs#L964`](../crates/pitboss-cli/src/manifest/schema.rs#L964) |
| `budget_usd` | float | no | Soft cap on lead spend with reservation accounting. spawn_worker fails once spent + reserved + next_estimate > budget. | [`../crates/pitboss-cli/src/manifest/schema.rs#L973`](../crates/pitboss-cli/src/manifest/schema.rs#L973) |
| `lead_timeout_secs` | integer | no | Wall-clock cap on the lead session. Default 3600. | [`../crates/pitboss-cli/src/manifest/schema.rs#L982`](../crates/pitboss-cli/src/manifest/schema.rs#L982) |
| `permission_routing` | enum (`path_a` \| `path_b`) | no | path_a (default) makes pitboss the sole permission authority. path_b routes claude's gate through pitboss (rejected at validate time pending stabilization). | [`../crates/pitboss-cli/src/manifest/schema.rs#L995`](../crates/pitboss-cli/src/manifest/schema.rs#L995) |
| `allow_subleads` | boolean | no | Expose spawn_sublead to the root lead. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1004`](../crates/pitboss-cli/src/manifest/schema.rs#L1004) |
| `max_subleads` | integer | no | Hard cap on total live sub-leads under this root. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1011`](../crates/pitboss-cli/src/manifest/schema.rs#L1011) |
| `max_sublead_budget_usd` | float | no | Cap on the per-sub-lead budget envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1018`](../crates/pitboss-cli/src/manifest/schema.rs#L1018) |
| `max_total_workers` | integer | no | Cap on total live workers across the entire tree (root + sub-trees). | [`../crates/pitboss-cli/src/manifest/schema.rs#L1026`](../crates/pitboss-cli/src/manifest/schema.rs#L1026) |

## `[sublead_defaults]` — `SubleadDefaults`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:1033`](../crates/pitboss-cli/src/manifest/schema.rs#L1033).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `budget_usd` | float | no | Per-sub-lead envelope when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1038`](../crates/pitboss-cli/src/manifest/schema.rs#L1038) |
| `max_workers` | integer | no | Per-sub-lead worker pool when read_down = false. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1043`](../crates/pitboss-cli/src/manifest/schema.rs#L1043) |
| `lead_timeout_secs` | integer | no | Wall-clock cap for the sub-lead session. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1048`](../crates/pitboss-cli/src/manifest/schema.rs#L1048) |
| `read_down` | boolean | no | When true, sub-lead shares root's budget + worker pool instead of carving its own envelope. | [`../crates/pitboss-cli/src/manifest/schema.rs#L1054`](../crates/pitboss-cli/src/manifest/schema.rs#L1054) |

## `[[approval_policy]]` — `ApprovalRuleSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:440`](../crates/pitboss-cli/src/manifest/schema.rs#L440).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `action` | enum (`auto_approve` \| `auto_reject` \| `block`) | **yes** | Action when this rule matches. | [`../crates/pitboss-cli/src/manifest/schema.rs#L451`](../crates/pitboss-cli/src/manifest/schema.rs#L451) |

## `[[mcp_server]]` — `McpServerSpec`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:192`](../crates/pitboss-cli/src/manifest/schema.rs#L192).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Key under mcpServers in the generated config. | [`../crates/pitboss-cli/src/manifest/schema.rs#L198`](../crates/pitboss-cli/src/manifest/schema.rs#L198) |
| `command` | text | **yes** | Executable to launch (e.g. npx, uvx, or an absolute path). | [`../crates/pitboss-cli/src/manifest/schema.rs#L204`](../crates/pitboss-cli/src/manifest/schema.rs#L204) |
| `args` | string list | no | Arguments passed to the command. | [`../crates/pitboss-cli/src/manifest/schema.rs#L208`](../crates/pitboss-cli/src/manifest/schema.rs#L208) |
| `env` | key-value map | no | Environment variables injected into the MCP server process. | [`../crates/pitboss-cli/src/manifest/schema.rs#L215`](../crates/pitboss-cli/src/manifest/schema.rs#L215) |
| `scope` | text | no | Optional injection scope. Form: "type:<id>" — narrows this server to actors spawned under that worker_type/sublead_type. Unset = inject into all actors. | [`../crates/pitboss-cli/src/manifest/schema.rs#L228`](../crates/pitboss-cli/src/manifest/schema.rs#L228) |
| `tools` | string list | no | Optional per-tool allowlist for this MCP server. Validate rejects any actor `tools` entry like `mcp__<id>__X` whose `X` is not in this list. Unset = no restriction. | [`../crates/pitboss-cli/src/manifest/schema.rs#L244`](../crates/pitboss-cli/src/manifest/schema.rs#L244) |

## `[[worker_type]]` — `WorkerType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:680`](../crates/pitboss-cli/src/manifest/schema.rs#L680).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_worker(worker_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L687`](../crates/pitboss-cli/src/manifest/schema.rs#L687) |
| `tools` | string list | no | Allowed tool surface for this worker class. Lead's spawn_worker(tools=...) must be a subset; omit means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L695`](../crates/pitboss-cli/src/manifest/schema.rs#L695) |
| `allowed_models` | string list | no | When non-empty, spawn_worker(model=...) must be in this list. Empty means any model. | [`../crates/pitboss-cli/src/manifest/schema.rs#L703`](../crates/pitboss-cli/src/manifest/schema.rs#L703) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_worker(timeout_secs=...). Lead values clamp DOWN; smaller values pass through. | [`../crates/pitboss-cli/src/manifest/schema.rs#L711`](../crates/pitboss-cli/src/manifest/schema.rs#L711) |

## `[[sublead_type]]` — `SubleadType`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:719`](../crates/pitboss-cli/src/manifest/schema.rs#L719).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Unique id used in spawn_sublead(sublead_type = "<id>"). Match: ^[A-Za-z0-9_-]+$. | [`../crates/pitboss-cli/src/manifest/schema.rs#L726`](../crates/pitboss-cli/src/manifest/schema.rs#L726) |
| `tools` | string list | no | Allowed tool surface for this sub-lead class. spawn_sublead(tools=...) must be a subset; empty means "give me the whole allowlist." | [`../crates/pitboss-cli/src/manifest/schema.rs#L733`](../crates/pitboss-cli/src/manifest/schema.rs#L733) |
| `allowed_models` | string list | no | When non-empty, spawn_sublead(model=...) must be in this list. | [`../crates/pitboss-cli/src/manifest/schema.rs#L740`](../crates/pitboss-cli/src/manifest/schema.rs#L740) |
| `max_timeout_secs` | integer | no | Hard cap on spawn_sublead(lead_timeout_secs=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L747`](../crates/pitboss-cli/src/manifest/schema.rs#L747) |
| `max_budget_usd` | float | no | Hard cap on spawn_sublead(budget_usd=...). Clamps DOWN. | [`../crates/pitboss-cli/src/manifest/schema.rs#L754`](../crates/pitboss-cli/src/manifest/schema.rs#L754) |

## `[communication]` — `CommunicationConfig`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:281`](../crates/pitboss-cli/src/manifest/schema.rs#L281).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `mode` | enum (`disabled` \| `parent_child`) | no | Actor communication policy mode. "disabled" hides the message_* and artifact_* tools (conservative default). "parent_child" enables them with parent/sublead/worker authz. | [`../crates/pitboss-cli/src/manifest/schema.rs#L288`](../crates/pitboss-cli/src/manifest/schema.rs#L288) |
| `max_message_bytes` | integer | no | Maximum UTF-8 body size accepted by message_send. | [`../crates/pitboss-cli/src/manifest/schema.rs#L294`](../crates/pitboss-cli/src/manifest/schema.rs#L294) |
| `max_artifact_bytes` | integer | no | Maximum decoded artifact payload size accepted by artifact_put. | [`../crates/pitboss-cli/src/manifest/schema.rs#L300`](../crates/pitboss-cli/src/manifest/schema.rs#L300) |
| `max_artifacts_per_actor` | integer | no | Maximum artifacts one actor may publish in a single run. | [`../crates/pitboss-cli/src/manifest/schema.rs#L306`](../crates/pitboss-cli/src/manifest/schema.rs#L306) |

## `[[template]]` — `Template`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:806`](../crates/pitboss-cli/src/manifest/schema.rs#L806).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `id` | text | **yes** | Slug referenced from [[task]].template. | [`../crates/pitboss-cli/src/manifest/schema.rs#L811`](../crates/pitboss-cli/src/manifest/schema.rs#L811) |
| `prompt` | text (multi-line) | **yes** | Prompt body. Supports {var} placeholders supplied by [[task]].vars. | [`../crates/pitboss-cli/src/manifest/schema.rs#L817`](../crates/pitboss-cli/src/manifest/schema.rs#L817) |

## `[lifecycle]` — `Lifecycle`

Defined at [`../crates/pitboss-cli/src/manifest/schema.rs:417`](../crates/pitboss-cli/src/manifest/schema.rs#L417).

| Field | Type | Required | Help | Source |
|---|---|---|---|---|
| `survive_parent` | boolean | no | Allow this dispatch to outlive its parent process. Requires a notify target. | [`../crates/pitboss-cli/src/manifest/schema.rs#L428`](../crates/pitboss-cli/src/manifest/schema.rs#L428) |

