---
document: pitboss-agent-instructions
schema_version: 1
pitboss_version: 0.16.0
last_updated: 2026-05-20
audience: ai-agent
canonical_url: https://github.com/SDS-Mode/pitboss/blob/main/AGENTS.md
---

# Pitboss for agents

Instructions for an AI agent (Claude, GPT, whatever) operating `pitboss` on
behalf of a human. If you're a human: read `README.md`. If you're an agent
that needs to orchestrate pitboss from natural language, stay here.

> **Agents:** the YAML frontmatter above is a stable machine-readable identity
> block. Filter on `document: pitboss-agent-instructions` + `pitboss_version`
> to decide whether this applies to the binary you're orchestrating. If
> `pitboss --version` disagrees with `pitboss_version` above, trust the
> binary — regenerate the manifest against its actual schema via
> `pitboss validate`.

---

## Mission

Pitboss is a Rust dispatcher that runs multiple `claude` subprocesses in
parallel under a concurrency cap and captures structured artifacts per run.
It has two modes:

- **Flat**: the operator predeclares N tasks; pitboss runs them.
- **Hierarchical**: the operator declares one **lead**; the lead dynamically
  spawns **worker** subprocesses via MCP tool calls, under house rules the
  operator set.

You invoke pitboss from a shell. You do not need to touch rust source to
use it. You write a TOML manifest, validate it, dispatch it, read the run
directory.

---

## When to reach for pitboss (decision tree)

Pitboss is the right tool when **all** of the following hold:

1. The task decomposes into **≥ 2 units** that could run in parallel.
2. Each unit is substantial enough to justify a fresh claude subprocess
   (order of ≥30 seconds of work), *not* a one-liner the caller could do
   inline.
3. You want **isolated git worktrees** per unit (or you've set
   `use_worktree = false` and accepted the shared-directory tradeoff).
4. You want **structured artifacts** — per-task logs, token usage, session
   ids, summary.json — not just "the output scrolled by in a terminal."
5. The wall-clock win from parallelism beats the setup cost (~1-2 seconds
   per worker for process spawn + worktree prep).

Anti-patterns — **don't use pitboss** for:
- Single-shot work. One claude call is simpler than writing a manifest.
- Tightly coupled work where units must communicate mid-execution. Pitboss
  workers cannot message each other (by design).
- Work where the operator needs to inspect intermediate state
  interactively. Pitboss runs are batch; the TUI is read-only.
- Deep recursion. Max depth is 2 (root lead → sub-leads → workers).
  Workers cannot spawn anything. Sub-leads are available via `spawn_sublead`
  when `allow_subleads = true`; use them for orthogonal phases, not for
  general recursion.

---

## Flat vs hierarchical — which one

| | Flat | Hierarchical |
|---|---|---|
| **When you know the decomposition up front** | ✓ | |
| **When the decomposition depends on the input** | | ✓ |
| **Manifest declares every task statically** | ✓ | |
| **Lead observes + reacts to intermediate results** | | ✓ |
| **Number of workers** | fixed | dynamic, bounded by `max_workers` |
| **Budget enforcement** | no | yes, `budget_usd` |
| **MCP server runs** | no | yes, on a unix socket |

Rule of thumb: **if the operator can write out every `[[task]]` before
running, use flat. If the operator is describing a *policy* (e.g., "one
worker per file in this directory", "one worker per unique author"), use
hierarchical.**

---

## Vocabulary

| Term | Meaning |
|---|---|
| **Pitboss** | The `pitboss` binary you invoke. |
| **Run** | One `pitboss dispatch` invocation. Produces `~/.local/share/pitboss/runs/<run-id>/`. |
| **Lead** | In hierarchical mode, the first claude subprocess. Receives the operator's prompt + the full MCP orchestration toolset. Decides how many workers to spawn. |
| **Worker** | A claude subprocess executing a single task, either declared in `[[task]]` (flat) or dynamically spawned by the lead (hierarchical). |
| **House rules** | Hierarchical guardrails: `max_workers` (≤16), `budget_usd`, `lead_timeout_secs`. For depth-2 runs: also `max_subleads`, `max_sublead_budget_usd`, `max_total_workers`. |
| **Worktree** | A per-task git worktree under a fresh branch, isolating concurrent work. `use_worktree = true` by default. |

---

## Manifest schema

TOML, typically named `pitboss.toml`. Every field annotated below.

> **Need a starting point?** `pitboss init [output] --template simple|full`
> emits a valid v0.9 manifest skeleton. `simple` is one `[lead]` driving a
> flat worker pool (the 80% case); `full` includes coordinator + sub-leads +
> commented optional sections. Both render to stdout if no output path is
> given.
>
> **Need the complete machine-readable schema?** `pitboss schema --format=map`
> emits the markdown field map (every key, every default, every file:line
> ref); `pitboss schema --format=example` emits a complete reference TOML
> with every field present as a placeholder. Useful when generating
> manifests programmatically rather than reading this doc.

> **v0.9 schema** — collapses the v0.8 `[[lead]]`/`[lead]` split into one
> canonical `[lead]` (single-table) form, moves lead-level caps off `[run]`
> and onto `[lead]`, promotes `[lead.sublead_defaults]` to top-level
> `[sublead_defaults]`, and renames a few fields for consistency. See the
> `Migration from v0.8 → v0.9` table at the bottom of this section. Pre-v0.9
> manifests are rejected; `pitboss validate` provides per-field migration
> guidance.

### Top-level `[run]` (run-wide infrastructure config)

`[run]` carries settings that apply to the whole dispatch run. Lead-level
caps (which used to live here in v0.8) moved to `[lead]` in v0.9.

| Key | Type | Required? | Default | Notes |
|---|---|---|---|---|
| `name` | string | no | unset | Human-readable label used to group related runs in the operational console (e.g. `"build-db"`, `"nightly-sync"`). When unset, the console falls back to the manifest filename. The canonical reference to a run remains its UUIDv7 `run_id`; this name is purely for cross-run grouping. |
| `max_parallel_tasks` | int | no | 4 | Concurrency cap for flat-mode `[[task]]` runs. Overridden by `ANTHROPIC_MAX_CONCURRENT` env. Renamed from `max_parallel` in v0.9. |
| `halt_on_failure` | bool | no | false | Flat mode. If a task fails, skip remaining tasks. |
| `run_dir` | string path | no | `~/.local/share/pitboss/runs` | Where per-run artifacts land. |
| `worktree_cleanup` | `"always"` \| `"on_success"` \| `"never"` | no | `"on_success"` | What to do with each worker's worktree after completion. `"never"` for inspection-heavy runs. |
| `emit_event_stream` | bool | no | false | When true, the dispatcher persists every control-plane envelope (sub-lead lifecycle, worker failures, approval requests, etc.) to `<run-dir>/events.jsonl` as it fires on the wire. Off by default for back-compat — older runs have no such file. Different from the per-actor `tasks/<id>/events.jsonl` audit log: that's always-on for tool-denial / pause / reprompt rows; this one is run-wide, opt-in, and carries the same envelope shape as the live SSE stream. See [`events.jsonl` structure](#eventsjsonl-structure-v013-opt-in) below. |
| `resource_sample_secs` | int | no | 5 | How often the dispatcher samples per-actor RSS / VSZ / CPU% and container-cgroup memory headroom. `0` disables sampling entirely; the watcher task is not spawned. Default `5`. Each tick broadcasts a `resource_sample` envelope on the control-event bus (which `summary.json` rolls up into `resource_high_water` at finalize and which `events.jsonl` persists when `emit_event_stream = true`). Memory-pressure transitions emit a separate `resource_pressure { warn | error | clear }` envelope that the SPA renders as a banner. Pressure thresholds: warn at ≥70% of available memory, error at ≥90%, clear at sustained <60% (2-sample debounce). On macOS host flat dispatch the sampler logs once at INFO and exits (no `/proc`); inside `pitboss container-dispatch` the sampler runs in the linux VM regardless of host OS, so macOS still gets full pressure visibility. (#553) |
| `claude_setting_sources` | string | no | (omitted) | `--setting-sources` override forwarded to every worker `claude … -p` spawn. Comma-separated subset of `user`, `project`, `local`. **When omitted**, pitboss applies its built-in default: `project,local` inside container-dispatch (host hooks don't leak in, #426); no filter on host-dispatch (operator's full `~/.claude/settings.json` flows through). **When set**, the operator value wins in both modes — the canonical use case is filtering user-scope `SessionStart` hooks (e.g. claude-code's "explanatory" output style that injects `★ Insight ─` blocks) out of unattended host-dispatch workers. Set to `"project,local"` to exclude `user`-scope settings; set to `"local"` to also exclude project-scope. Validated at validate time (`pitboss validate`); empty strings, unknown tokens, whitespace inside the value, and duplicate tokens all reject with hints pointing at the canonical comma-separated form. (#555) |
| `default_approval_policy` | `"block"` \| `"auto_approve"` \| `"auto_reject"` | no | `"block"` | Hierarchical: default action for `request_approval` / `propose_plan` when no `[[approval_policy]]` rule matches. Since v0.9.2, `auto_approve` and `auto_reject` short-circuit unconditionally — they fire even when a TUI is attached; only `"block"` routes through the operator queue. Renamed from `approval_policy` in v0.9 to disambiguate from the rules array. |
| `denial_termination_policy` | `"adapt"` \| `"reclassify"` | no | `"adapt"` | Path B only: how a denied `permission_prompt` affects the actor's terminal status. `"adapt"` (default, post-#377) trusts the actor's exit code; per-actor `events.jsonl::tool_denied` rows and the `approvals_rejected` counter remain authoritative for what was blocked. `"reclassify"` re-labels clean exits within 30s of a denial as `ApprovalRejected` (legacy heuristic, kept for operators who want fast-give-up distinguished in the status table — accepts that successful adaptation within the window will misclassify as failure). |
| `require_plan_approval` | bool | no | false | Hierarchical: when true, `spawn_worker` refuses until a plan submitted via `propose_plan` has been operator-approved. |
| `dump_shared_store` | bool | no | false | Hierarchical: at run finalize, write `shared-store.json` into the run dir for post-mortem inspection. |
| `require_actor_type` | bool | no | false | When true, every `spawn_worker` and `spawn_sublead` call MUST name a declared `[[worker_type]]` / `[[sublead_type]]`; type-less spawns are rejected at the dispatcher. Default `false` for back-compat with manifests that pre-date typed profiles (#252). |
| `untyped_actor_policy` | `"bridge"` \| `"block"` | no | `"bridge"` | Path-B-only behavior for un-typed callers reaching `permission_prompt`. `"bridge"` (default) routes to the operator approval queue, preserving pre-#252 semantics. `"block"` synthesizes an empty profile so anything outside `--allowedTools` auto-denies via `denied_by_profile` — the strict counterpart for headless production runs once every actor's profile has been declared. |

### `[[notification]]` sinks (v0.4.1+)

Optional notification sinks. Multiple blocks allowed, one per sink.

| Key | Required? | Notes |
|---|---|---|
| `kind` | yes | `"log"`, `"webhook"`, `"slack"`, `"discord"` |
| `url` | for webhook/slack/discord | Endpoint; `${PITBOSS_VAR}` substitution supported (v0.7.1+: only `PITBOSS_`-prefixed env vars may be substituted) |
| `events` | no | Filter by event category. Defaults to all. |
| `severity_min` | no | Minimum severity to fire (`info` / `warning` / `error` / `critical`). |

Event categories:

| Category | Fires when |
|---|---|
| `"approval_request"` | A `request_approval` or `propose_plan` call is queued for operator action |
| `"approval_pending"` | An approval enqueues and awaits operator action (v0.6+); use for alerting when a run is blocked |
| `"run_finished"` | A run reaches a terminal state |
| `"budget_exceeded"` | A `spawn_worker` call is rejected due to budget exhaustion |

Example — Slack alert on blocked approvals:

```toml
[[notification]]
kind = "slack"
url = "${PITBOSS_SLACK_WEBHOOK_URL}"
events = ["approval_pending", "run_finished"]
```

### `[defaults]`

Inherited by every `[[task]]` and `[lead]` unless overridden.

| Key | Type | Notes |
|---|---|---|
| `model` | string | e.g. `claude-haiku-4-5`, `claude-sonnet-4-6`, `claude-opus-4-7`. Dated suffixes allowed. |
| `effort` | `"low"` \| `"medium"` \| `"high"` | Maps to claude's `--effort` flag. |
| `tools` | array of string | `--allowedTools` value. Defaults to `["Read", "Write", "Edit", "Bash", "Glob", "Grep"]` if unset. |
| `timeout_secs` | int | Per-task wall-clock cap. |
| `use_worktree` | bool | Default `true`. Set `false` for read-only analysis runs. |
| `env` | table (string → string) | Env vars passed to the claude subprocess. |

### `[[task]]` (flat mode, repeat)

| Key | Required? | Notes |
|---|---|---|
| `id` | yes | Short slug used in logs, worktree names. Alphanumeric + `_` + `-`. Unique within manifest. |
| `directory` | yes | Must be inside a git repo if `use_worktree = true`. |
| `prompt` | yes | What the claude subprocess receives via `-p`. |
| `branch` | no | Branch name for the worktree. Defaults to a generated name. |
| `model`, `effort`, `tools`, `timeout_secs`, `use_worktree`, `env` | no | Per-task overrides of `[defaults]`. |

### `[lead]` (hierarchical mode, exactly one, mutually exclusive with `[[task]]`)

`[lead]` is a single-table block (no array form — the v0.8 `[[lead]]` array
form was removed in v0.9). `id` is used as the tile label in the TUI.

> **Important:** `prompt =` must appear **before** any subtable declaration
> (e.g. `[lead.env]`) in the TOML source. A `prompt =` key that appears
> after a subtable header is parsed into that subtable's scope and silently
> dropped; `pitboss validate` catches this and reports `"prompt is required
> but is empty"`.

Required and per-actor fields:

| Key | Type | Required? | Notes |
|---|---|---|---|
| `id` | string | yes | Short slug used in logs, worktree names, TUI tiles. Alphanumeric + `_` + `-`. |
| `directory` | string path | yes | Working dir for the lead's claude subprocess. Must be a git work-tree if `use_worktree = true`. |
| `prompt` | string | yes | Operator instructions passed via `-p`. Must come before any `[lead.X]` subtable. |
| `branch` | string | no | Branch name for the lead's worktree. Auto-generated if omitted. |
| `model`, `effort`, `tools`, `timeout_secs`, `use_worktree`, `env` | various | no | Per-lead overrides of `[defaults]`. |

Lead-level caps (moved from `[run]` in v0.9 — they're properties of the
lead, not the run):

| Key | Type | Default | Notes |
|---|---|---|---|
| `max_workers` | int | unset | Hard cap on the lead's concurrent + queued worker pool (1–16). Required when the lead spawns workers. |
| `budget_usd` | float | unset | Soft cap with reservation accounting on the **run-wide total** (workers + lead + sub-leads). `spawn_worker` fails with `budget exceeded` once `spent + reserved + next_estimate > budget`; the lead is aborted if a reconciled turn drives total spend past the cap. |
| `lead_budget_usd` | float | unset | Optional separate cap on **lead + sub-lead orchestration cost** (the lead session's own token spend plus every sub-lead session's own token spend — **worker spend does NOT count against this cap**). Independent of `budget_usd`. When set, the lead is aborted once accumulated lead/sub-lead spend exceeds this cap, even if `budget_usd` still has headroom. Use this to bound orchestration spend without constraining worker spend. (#253) |
| `lead_timeout_secs` | int | 3600 fallback | Wall-clock cap on the lead session. No upper bound — set generously for multi-hour orchestration plans. |

Depth-2 controls (sub-leads):

| Key | Type | Default | Notes |
|---|---|---|---|
| `allow_subleads` | bool | `false` | Expose `spawn_sublead` in the root lead's `--allowedTools`. Required to enable depth-2. |
| `max_subleads` | int | unset | Cap on total sub-leads the root lead may spawn. |
| `max_sublead_budget_usd` | float | unset | Per-sub-lead envelope cap; `spawn_sublead` rejects envelopes exceeding this. |
| `max_total_workers` | int | unset | Cap on total live workers including all sub-tree workers. Renamed from `max_workers_across_tree` in v0.9. |
| `permission_routing` | `"path_a"` \| `"path_b"` | `"path_b"` (since v0.12) | `"path_b"` (default) leaves the entrypoint unset so claude's gate is active and wires `--permission-prompt-tool mcp__pitboss__permission_prompt`; each per-tool check the model wants outside its `--allowedTools` routes through pitboss's approval queue and returns the SDK `PermissionResult` shape (`{behavior:"allow",updatedInput?}` or `{behavior:"deny",message,interrupt?}`). Denials are non-terminating — claude receives the structured response and adapts. Per-actor `<run_dir>/tasks/<actor>/events.jsonl` records each denial as a `tool_denied` row, and profile-driven approves as `tool_auto_approved`. `"path_a"` is the opt-out: sets `CLAUDE_CODE_ENTRYPOINT=sdk-ts` plus `--dangerously-skip-permissions`, bypassing claude's gate entirely so pitboss's `[[approval_policy]]` rules + bridge become the sole permission authority. **Order of evaluation under Path B**: `[[approval_policy]]` rules first (operator override), then the caller's `[[worker_type]] / [[sublead_type]]` profile `tools` allowlist (#388) — membership ⇒ Allow, absence ⇒ Deny via `denied_by_profile`, no operator round-trip — finally bridge fallback for untyped callers (or auto-deny via synthesized empty profile when `[run].untyped_actor_policy = "block"`, #392). See `[run].denial_termination_policy` for how denials affect the actor's terminal status. |

### Top-level `[sublead_defaults]` (v0.9+, promoted from `[lead.sublead_defaults]`)

Optional defaults applied to `spawn_sublead` calls that omit the
corresponding parameters. Top-level in v0.9 — the v0.8 nested
`[lead.sublead_defaults]` form is gone.

```toml
[sublead_defaults]
budget_usd = 2.00
max_workers = 4
lead_timeout_secs = 1800
read_down = false
```

| Key | Type | Notes |
|---|---|---|
| `budget_usd` | float | Per-sub-lead envelope when `read_down = false`. |
| `lead_budget_usd` | float | Per-sub-lead cap on the sub-lead's own token spend (orchestration cost only — **does not count its workers' spend**). Analogous to `[lead].lead_budget_usd` but scoped to one sub-lead. Independent of `budget_usd`. Honored when `read_down = false`. |
| `max_workers` | int | Per-sub-lead worker pool when `read_down = false`. |
| `lead_timeout_secs` | int | Wall-clock cap for the sub-lead session. |
| `read_down` | bool | When true, the sub-lead shares the root's budget and worker pool instead of carving its own envelope. |

### `[container]` (v0.8+)

The `[container]` section enables `pitboss container-dispatch`, which assembles and execs a Docker/Podman run command from the manifest. Task and lead `directory` fields are interpreted as container-side paths when `[container]` is present.

To pre-flight a container-mode manifest without dispatching, use `pitboss validate --container <manifest>` (#255). The flag skips the host-side directory-existence check that would otherwise fail on the in-container mount paths. Without `--container`, validate emits a hint pointing at the flag rather than failing silently.

| Key | Type | Default | Notes |
|---|---|---|---|
| `image` | string | `ghcr.io/sds-mode/pitboss-with-claude:latest` | Container image to run. |
| `runtime` | `"docker"` \| `"podman"` \| `"auto"` | `"auto"` | Runtime selector. `"auto"` prefers podman when available. |
| `extra_args` | array of string | `[]` | Verbatim flags for `podman run` / `docker run`. The escape hatch for any container-runtime concern: networking (`--network=corp-fw`, `--dns=10.0.0.53`, `--add-host=svc:1.2.3.4`), narrow capabilities (`--cap-add=NET_ADMIN`), resource limits (`--memory=4g`, `--cpus=2`). **Validated denylist** (`validate` rejects at parse time): `--privileged`, `--{pid,ipc,uts,userns,cgroupns}=host`, `--cap-add={ALL,SYS_ADMIN,SYS_PTRACE,SYS_MODULE}`, `--security-opt={seccomp,apparmor}=unconfined` / `label=disable`, `--device=…`, `-v` / `--volume` / `--mount` (use `[[container.mount]]`), `-u` / `--user` (UID alignment is pitboss-managed), `--entrypoint`. Each container-dispatch invocation is recorded NDJSON-style at `<runs-base>/container-dispatch.log` for post-facto audit. |
| `extra_apt` | array of string | `[]` | Debian/Ubuntu packages installed inside the container. Two paths: by default they are installed at dispatch start (~30–90 s per run, **running as UID 0 during the apt phase** before `runuser` drops to `pitboss`); after `pitboss container-build`, they are baked into a derived image and dispatch starts as the unprivileged `pitboss` user. Each entry must match `[a-zA-Z0-9][a-zA-Z0-9.+-]*`; rejected at validate time otherwise. For production, prefer the `pitboss container-build` derived-image path to avoid the apt-phase root window. |
| `workdir` | string | first mount's container path, else `/home/pitboss` | Working directory inside the container. |
| `claude_mount_rw` | bool | `false` | Auto-injected `~/.claude` mount mode. `false` (default) = read-only — prevents a buggy/compromised worker from rewriting host credentials or injecting `settings.json` hooks. `true` = read-write, needed only for in-place OAuth token refresh. |

#### `[[container.mount]]`

| Key | Required? | Notes |
|---|---|---|
| `host` | yes | Absolute host path. Tilde (`~`) is expanded. |
| `container` | yes | Absolute path inside the container. |
| `readonly` | no | Default `false`. |

Two mounts are always auto-injected: `~/.claude → /home/pitboss/.claude` (OAuth — **read-only by default**; set `[container].claude_mount_rw = true` to allow OAuth token refresh) and the run artifact directory; the manifest itself is injected at `/run/pitboss.toml` read-only.

#### macOS+Podman: auto-injected `XDG_RUNTIME_DIR=/tmp` (#550)

On macOS hosts using Podman, the auto-mounted runs directory is virtiofs-backed and rejects AF_UNIX `bind()` with `EINVAL`. To keep the in-container control + MCP sockets bindable, pitboss auto-injects `-e XDG_RUNTIME_DIR=/tmp` into the `podman run` argv, which redirects the per-run socket paths onto container-overlay `/tmp`. Pairs with the control-bridge TCP forward (#474/#546) so host-side `pitboss-web` still reaches the bridge despite the socket being on container-overlay.

The auto-inject is **suppressed** when the operator already sets the variable via `[container].extra_args` (`-e XDG_RUNTIME_DIR=…`, `--env XDG_RUNTIME_DIR=…`, or the `=`-joined forms) — the operator's value wins and only one `-e` lands in the dispatch audit log. Linux dispatches are unaffected — the operator's systemd-provided `$XDG_RUNTIME_DIR` continues to flow through unchanged.

#### `[[container.copy]]`

Files baked into a derived image at `pitboss container-build` time. Unlike `[[container.mount]]`, these are layered into the image — they're available even when no host bind mount is in place, and they require a build step before `container-dispatch` will run.

| Key | Required? | Notes |
|---|---|---|
| `host` | yes | Absolute host path or tilde-prefixed (`~/...`). May be a file or directory. Read at build time only — edits to the host file invalidate the derived tag and force a rebuild. |
| `container` | yes | Absolute path inside the container. |

Declaring `[[container.copy]]` makes a `pitboss container-build <manifest>` call **mandatory** before `pitboss container-dispatch` will run — the dispatcher refuses to fall back to the stock image because the COPY contents would be missing.

#### `pitboss container-build` (v0.9.2+)

Synthesizes a thin Dockerfile from `[container]` and builds a derived image tagged deterministically as `pitboss-derived-<sha>:local`. The `<sha>` hashes the base image, sorted `extra_apt`, and sorted `[[container.copy]]` entries (host file CONTENTS, not paths). Idempotent: re-running with the same inputs is a no-op once the tag exists. `--no-cache` forces a rebuild.

```bash
pitboss container-build pitboss.toml             # build (or skip if cached)
pitboss container-build pitboss.toml --no-cache  # force rebuild
pitboss container-build pitboss.toml --print-dockerfile  # preview
pitboss container-build pitboss.toml --dry-run   # print podman/docker invocation
```

`pitboss container-dispatch` automatically picks up the derived tag when `extra_apt` or `copy` is non-empty and the tag exists locally — no explicit wiring needed in the manifest.

#### `pitboss container-prune` (v0.9.2+)

Sweeps stale `pitboss-derived-*:local` tags from the local image store. Cross-references against derived tags computed from manifests passed on the CLI; everything not referenced by any of them is "stale".

```bash
pitboss container-prune                       # dry-run, all derived tags shown as stale
pitboss container-prune m1.toml m2.toml       # dry-run, tags from m1/m2 marked active, rest stale
pitboss container-prune m1.toml m2.toml --apply  # remove the stale tags
```

The output is tab-separated for grep / awk consumption (`pitboss container-prune | awk '$1=="stale"'`). Never touches images outside the `pitboss-derived-*:local` namespace, so it's safe to run on hosts with unrelated images. Time-based eviction (`--keep-recent N` etc.) is intentionally deferred — see #267 for the design discussion.

#### Image tag cadence (`ghcr.io/sds-mode/pitboss-with-claude`)

Three tag families are published:

| Tag family | Mutability | When it moves | Use when |
|---|---|---|---|
| `:0.9.1`, `:0.9`, `:0` | **Immutable snapshot** | Never (released) | You want a fixed, auditable point release. These tags do **not** receive backports — fixes that land after a release tag are only available via `:main` or `:latest`. |
| `:main`, `:latest` | **Rolling** | Every push to main | You want the latest fixes since the most recent release. Both move together; `:latest` is a convenience alias for `:main`. |
| `:main-<short-sha>` | Per-commit immutable | One per main commit | You need to pin a `container-dispatch` to a specific main-HEAD commit for reproducibility (debugging, soak tests). |

Pushes to main rebuild the image regardless of whether the commit touched code paths — including README/CHANGELOG-only commits. This is deliberate: the cost of a stale published image (operators hitting "already-fixed" bugs in their containers) is higher than the runner-minute cost of always rebuilding. PRs that touch only README/CHANGELOG still skip CI; the rebuild fires when the squash commit lands on main.

### `[[mcp_server]]` (v0.9+)

Declare external MCP servers to inject into actors' `--mcp-config`. By default servers are injected into **every actor** (lead, sub-leads, and workers); the optional `scope` field narrows injection to actors of a typed profile (#252 Phase 1.5, v0.12+).

```toml
[[mcp_server]]
id      = "context7"
command = "npx"
args    = ["-y", "@upstash/context7-mcp"]

[[mcp_server]]
id      = "my-tool"
command = "/usr/local/bin/my-mcp-server"
args    = ["--port", "3000"]
env     = { MY_TOKEN = "abc" }

[[mcp_server]]
id      = "fs-writer"
command = "/usr/local/bin/fs-mcp"
scope   = "type:writer"   # only injected into worker_type/sublead_type "writer"
```

| Key | Required? | Notes |
|---|---|---|
| `id` | yes | Key name in the generated `mcpServers` JSON. Must be unique within the manifest. |
| `command` | yes | Executable to launch (e.g. `"npx"`, `"uvx"`, absolute path). |
| `args` | no | Arguments passed to the command. Default `[]`. |
| `env` | no | Environment variables injected into the server process. Default `{}`. |
| `scope` | no | Optional injection scope. Form `"type:<id>"` references a `[[worker_type]]` or `[[sublead_type]]`. Validated at manifest load — unknown ids are rejected. Untyped actors (no `worker_type` / `sublead_type` arg on spawn) NEVER receive scoped servers. Unset (default) = inject into all actors. |
| `tools` | no | Optional per-tool allowlist for THIS server. Enforces at three layers (most-restrictive-wins): (1) **manifest validate** rejects any actor surface (`[lead].tools`, `[[task]].tools`, `[[worker_type]].tools`, `[[sublead_type]].tools`) that references `mcp__<id>__X` for an `X` not in this list; (2) **spawn-time argv (Path B)** drops non-allowlisted entries from the spawned actor's `--allowedTools` so the call routes through `permission_prompt`; (3) **runtime `permission_prompt` (Path B)** denies any routed `mcp__<id>__X` call where `X` isn't in the allowlist with `DeniedByMcpServerAllowlist`. `tools = []` is rejected as self-defeating. Unset (default) = no per-tool restriction. **Path A note:** `--dangerously-skip-permissions` bypasses the claude permission layer entirely; the spawn-time + runtime gates have no effect under Path A (validate still enforces). Operators picking Path A retain its "skip all gates" semantic. (#391 / #397 / #399) |

**Tools from injected servers are available immediately** — no additional `--allowedTools` configuration is needed; claude's MCP client discovers the tools from the server at startup.

### `[[worker_type]]` / `[[sublead_type]]` (v0.12+, typed actor profiles)

Declare per-class capability caps that the dispatcher enforces at spawn time. The lead can never widen these caps — `tools` arg must be a subset of the profile's allowlist; `model` must be in `allowed_models` (when set); `timeout_secs` / `budget_usd` only clamp DOWN. (#252)

```toml
[[worker_type]]
id               = "extraction"
tools            = ["Read", "Glob", "Grep"]
allowed_models   = ["claude-haiku-4-5", "claude-sonnet-4-6"]
max_timeout_secs = 900

[[worker_type]]
id    = "writer"
tools = ["Read", "Glob", "Grep", "Write"]

[[sublead_type]]
id              = "planner"
tools           = ["Read", "Glob", "Grep"]
allowed_models  = ["claude-opus-4-7"]
max_budget_usd  = 2.00
```

The `spawn_worker` and `spawn_sublead` MCP tools gain optional `worker_type` / `sublead_type` args. **Naming a profile overrides the legacy `[lead].tools` cascade** for that spawn — the manifest profile is the single source of truth for capability:

1. Unknown id → spawn rejected at the dispatcher.
2. `tools` arg must be a **subset** of the profile's allowlist — any tool not in the allowlist rejects the spawn with `denied: tool 'X' is not in worker_type 'Y' allowlist`.
3. When `tools` is omitted, the spawn gets the profile's full allowlist verbatim — NOT the legacy `[lead].tools` cascade. Untyped spawns continue to follow the cascade unchanged.
4. `model` must be in `allowed_models` when non-empty (empty = unrestricted).
5. `timeout_secs` is clamped DOWN to `max_timeout_secs`; smaller values pass through.
6. `[[sublead_type]]` additionally clamps `budget_usd` down to `max_budget_usd`.

Set `[run].require_actor_type = true` to make every `spawn_worker` / `spawn_sublead` call REQUIRE a profile arg — type-less spawns are rejected. The flag is inert in flat mode (warning logged at validate time).

**`[run].untyped_actor_policy`** (Path-B-only, default `"bridge"`) controls what happens when an un-typed Worker / Sublead reaches `permission_prompt`. `"bridge"` (default) routes to the operator approval bridge — pre-#252 behavior, preserved for back-compat. `"block"` synthesizes an empty profile so anything outside `--allowedTools` auto-denies via `denied_by_profile` with the sentinel `actor_type = "<synthetic>"` on the audit row — closes the un-typed escape hatch for headless production runs. Validate rejects `"block"` when no profiles are declared (every spawn would auto-deny → self-defeating manifest).

The resolved profile id is persisted to each `TaskRecord.actor_type`, surfaced in `summary.json` / `summary.jsonl` so the TUI, `pitboss-web`, and `pitboss status` can group actors by class without re-deriving from the manifest snapshot.

**Phase 1.5 (v0.12, landed):**
- Per-actor MCP server scoping via `[[mcp_server]].scope = "type:<id>"` — see the `[[mcp_server]]` section above.
- Resumed workers (`continue_worker` / `reprompt_worker`) preserve `actor_type` on the appended record via the in-memory `workers.actor_types` map populated at spawn.
- Synthesized cancellation records on lead-exit cleanup keep `actor_type` from the same map.
- SQLite-backed runs round-trip `actor_type` (migration v10 adds the column; both backends are now at parity with `JsonFileStore`).

### `[[agent_profile]]` (v0.14+, reusable role preludes)

Reusable role profiles. Each profile carries a **prepended `system_prompt` prelude** plus optional **`model` / `tools` / `env` defaults**. Unlike `[[worker_type]]` (capability *caps*), agent profiles are default-providers — the operator's per-actor config (`[lead]`, `[[task]]`, spawn args) always wins.

Three built-ins ship bundled, listable via `pitboss schema --format=agent-profiles`:

- `pitboss/lead-opus` — root-lead orchestration body on Opus.
- `pitboss/sublead-sonnet` — sub-tree-lead body on Sonnet.
- `pitboss/worker-haiku` — leaf-worker publish-ceremony body on Haiku.

A manifest may declare additional profiles and shadow a built-in by matching its id exactly. The `pitboss/` namespace is otherwise reserved (validate rejects novel `pitboss/<other>` ids).

```toml
[[agent_profile]]
id            = "review/domain-worker"
system_prompt = """
You are a domain reviewer. Publish findings via artifact_put; do not Bash.
"""
model = "claude-haiku-4-5"
tools = ["Read", "Glob", "Grep"]
env   = { PITBOSS_ACTOR_ROLE = "review-worker" }

[[worker_type]]
id            = "domain-worker"
tools         = ["Read", "Glob", "Grep"]
agent_profile = "review/domain-worker"

[lead]
id            = "review-lead"
directory     = "/path/to/repo"
prompt        = "Review the codebase, then synthesize findings."
agent_profile = "pitboss/lead-opus"     # built-in
max_workers   = 4
budget_usd    = 5.0
```

**Reference surfaces.** `agent_profile = "<id>"` is accepted on `[lead]`, `[[task]]`, `[[worker_type]]`, and `[[sublead_type]]`. References on `[lead]`/`[[task]]` apply at resolve time (prepended into the resolved prompt); references on `[[worker_type]]`/`[[sublead_type]]` apply at MCP-spawn time (composed before `worker_spawn_args`/`sublead_spawn_args`). Dangling references hard-fail at resolve (`[lead]`/`[[task]]`) or validate (`[[worker_type]]`/`[[sublead_type]]`).

**Precedence (later wins).** `built-in default → profile → [defaults] → per-actor (lead/task/spawn args)`. The `system_prompt` is **prepended** to the operator's prompt with a fixed `\n\n--- TASK ---\n\n` separator (never replaced). The `env` map merges between `[defaults].env` and the per-actor env. The `model` / `tools` defaults fill any slot the operator left unset.

### `[communication]` (v0.10+, opt-in)

Declares the policy for the Pitboss-owned mailbox + artifact MCP tools.
**Default `mode = "disabled"`** — KV remains the only inter-actor surface
unless an operator opts in. The mailbox is for short coordination notes
referencing artifact ids; the artifact store is for binary payloads
(parents passing context to workers, workers handing back outputs)
without forcing actors to base64-encode blobs into KV.

```toml
[communication]
mode                    = "parent_child"   # "disabled" (default) | "parent_child"
max_message_bytes       = 8192              # body cap on message_send
max_artifact_bytes      = 10485760          # decoded payload cap on artifact_put
max_artifacts_per_actor = 128               # per-actor publish ceiling per run
```

**`parent_child` semantics:**
- Root lead reads / sends to every actor.
- Sub-leads read / send within their own sub-tree; may also message `"root"` upward.
- Workers may message only their direct parent (via the alias `"parent"`); they may read messages they sent or received.
- Sibling-worker artifact transfer requires the parent (or root) to call `artifact_grant` — there is no peer-to-peer write channel.

**Artifact storage** lands at `<run_dir>/communication/artifacts/<artifact_id>`. Auto-mounted under `pitboss container-dispatch`; cleaned with the run dir by `pitboss prune --remove`.

**Resume persistence (v0.14+, #282).** Every mutating handler (`message_send`, `message_ack`, `artifact_put`, `artifact_grant`) appends a typed JSON record to `<run_dir>/communication/messages.jsonl` after its in-memory write succeeds. `CommunicationStore::new` replays the file at run-start, so `pitboss resume` rehydrates the pre-crash mailbox + artifact metadata + per-actor activity counters. Replay is tolerant of torn trailing writes (warns + skips, same pattern as `summary.jsonl`); artifact bytes on disk under `communication/artifacts/<id>` survive independently. Pre-v0.14 runs have no journal and resume with an empty mailbox as before.

**Disabled mode** (the default) hides the 8 tools from `list_tools` and rejects every handler call with `CommunicationError::Disabled`. Tool names still appear in the lead/sublead/worker `--allowedTools` argv (cosmetic only — Claude does not call tools missing from `list_tools`).

See `examples/communication-parent-child-demo.toml` for a full walkthrough.

### Migration from v0.8 → v0.9

Pre-v0.9 manifests are rejected. `pitboss validate` scans for the migration
patterns below and emits guidance:

| v0.8 form | v0.9 form |
|---|---|
| `[[lead]]` (array) | `[lead]` (single-table) |
| `[run].max_workers` | `[lead].max_workers` |
| `[run].budget_usd` | `[lead].budget_usd` |
| `[run].lead_timeout_secs` | `[lead].lead_timeout_secs` |
| `[run].max_parallel` | `[run].max_parallel_tasks` |
| `[run].approval_policy` | `[run].default_approval_policy` |
| `[lead].max_workers_across_tree` | `[lead].max_total_workers` |
| `[lead.sublead_defaults]` | top-level `[sublead_defaults]` |
| `[lead].id` and `[lead].directory` optional | both required |

In-flight runs (`resolved.json` snapshots in run-dirs) remain readable —
`#[serde(alias)]` on the renamed `ResolvedManifest` fields preserves resume.

---

## Invocation patterns

### Validate before dispatch

```bash
pitboss validate pitboss.toml
```

Exit 0 = valid. Non-zero = parse error or semantic error. **Always validate
first.** This catches all the class-of-error issues (mixed `[[task]]` + `[lead]`,
`max_workers = 17`, `budget_usd = 0`, missing `id`, directory doesn't exist,
pre-v0.9 schema usage) before any claude subprocess is spawned.

### Pre-flight cost gate (`pitboss tree`)

```bash
pitboss tree pitboss.toml                 # render dispatch tree + worst-case envelope
pitboss tree pitboss.toml --check 20      # CI gate: exit non-zero if envelope > $20 or unbounded
```

Renders the dispatch tree (root lead, depth-2 controls, `[sublead_defaults]`,
or the flat-mode task list) alongside every per-actor knob the manifest is
implicitly committing to, and aggregates the worst-case budget envelope.

`--check <USD>` turns the same walk into a hard gate that exits non-zero
when the envelope exceeds the threshold OR when a required cap
(`max_sublead_budget_usd` etc) is unbounded. Drop into a CI workflow
between `validate` and `dispatch` to fail loudly before any spend lands —
catches cases where a manifest is structurally valid but committed to
unbounded fan-out.

### Dispatch

```bash
pitboss dispatch pitboss.toml
```

Blocks until all tasks finish. Exit codes:
- `0` — all tasks succeeded
- `1` — one or more tasks failed (but pitboss itself ran cleanly)
- `2` — manifest error, claude binary missing, etc.
- `130` — interrupted (Ctrl-C drained gracefully)

### Background dispatch (v0.10+)

```bash
pitboss dispatch pitboss.toml --background
# {"run_id":"019d…","manifest_path":"pitboss.toml","started_at":"…","child_pid":12345}
```

`nohup`-equivalent: detaches the dispatcher and returns immediately
with a JSON announcement on stdout. Exit code is 0 on successful spawn;
the run's actual outcome is observed out-of-band.

Use this when you (the agent) are wrapping pitboss for an orchestrator
context — a Discord bot's slash-command handler, a webhook receiver,
a CI script — that needs to dispatch and stay responsive rather than
block for the duration of the run.

The flag is mode-agnostic: works with both flat (`[[task]]`) and
hierarchical (`[lead]`) manifests. Whether a lead claude wraps the
dispatch is a manifest authoring decision, kept orthogonal to this
flag's attached-vs-detached lifecycle concern.

To learn how a backgrounded run finishes, three out-of-band channels:

| Channel | When to use |
|---|---|
| `[lifecycle].notify` webhook | Push delivery on `RunFinished` (no polling) |
| `pitboss list --active` | Survey of currently-running dispatchers |
| `pitboss status <run-id>` | On-demand snapshot of one run's task table |

The announced `run_id` matches the id that lands in `summary.json`
byte-for-byte (the parent pre-mints and forwards it to the child),
so orchestrators can correlate the parent's stdout with later
notifications and on-disk artifacts using a single key.

`--background --dry-run` is rejected — they're nonsensical together.

### Resume

```bash
pitboss resume <run-id>
```

Re-runs a prior dispatch. For flat-mode runs, each task respawns with its
original `claude_session_id`. For hierarchical runs, only the lead resumes
(`--resume <session-id>`); the lead decides whether to spawn fresh workers.

**Gotcha:** if the original run used `worktree_cleanup = "on_success"` (the
default), the worktrees are gone — claude can't find its sessions by cwd.
Use `worktree_cleanup = "never"` on runs you know you want to resume.

### Attach (v0.5.0+)

```bash
pitboss attach <run-id> <task-id>
pitboss attach <run-id> <task-id> --raw            # stream raw stream-json
pitboss attach <run-id> <task-id> --lines 200      # larger backfill
```

Follow-mode log viewer for a single worker. Run-id is resolved by
prefix; first 8 chars are plenty when unique. Formatted output matches
the TUI focus pane; `--raw` dumps the underlying jsonl. Exits on
Ctrl-C or when the worker emits its terminal `Event::Result`. Use
this when you want to watch one worker interactively without pulling
up the whole TUI.

### Diff

```bash
pitboss diff <run-a> <run-b>
```

Compares two runs side-by-side. Useful for A/B testing prompts or models.

---

## Headless mode (agents dispatching pitboss without a terminal)

If you (the agent) are dispatching pitboss without a terminal attached —
running in a container, under systemd, from another orchestrator — the
behavior diverges from interactive TUI use in several ways. Read this
section before writing manifests for headless dispatch.

### Permission model

**Pitboss is the sole permission authority for every claude subprocess
it spawns.** As of v0.7.1 every spawned claude (lead, sub-lead, worker)
receives:

1. `CLAUDE_CODE_ENTRYPOINT=sdk-ts` in env — closes claude's MCP-tool
   permission gate. Operator-overridable via `[defaults.env]`.
2. `--dangerously-skip-permissions` on the CLI — closes claude's
   filesystem (read/write outside cwd), bash-with-`$VAR`-expansion, and
   bash-with-`&&` gates. **Set unconditionally; not env-overridable.**

Without (1) sub-leads in headless dispatch exited in ~7 seconds
reporting `Success` with no output (apologizing that they couldn't get
MCP permission). Without (2), even after the MCP gate was closed, every
sublead's `echo x >> "$WORK_DIR/file"` returned `"Contains
simple_expansion"` — `-p` mode has no UI to answer the prompt — and the
orchestration plan collapsed silently with empty registries and null
kv reads.

Pitboss replaces the closed claude gates with its own approval surface:
`[run].approval_policy` (block / auto_approve / auto_reject),
`[[approval_policy]]` rules with TTL+fallback, the `request_approval`
and `propose_plan` MCP tools, and the TUI's approve/reject modal. If
you need claude's own gate fully back, do not use pitboss's headless
dispatch — drive the claude CLI interactively instead.

The trust boundary: anything you wouldn't run in your own claude
session under `--dangerously-skip-permissions` should not be in a
pitboss manifest. Operator-supplied prompts have full filesystem and
shell access at the `[lead].directory` cwd. Treat manifests as
production code.

### Approval policy — set `auto_approve` (or use rules)

Without a TUI to approve things, pitboss's own approval mechanisms hang
forever (or until timeout) if not configured. Set:

```toml
[run]
default_approval_policy = "auto_approve"  # or "auto_reject" for strict dry-run dispatch
```

For finer control, use `[[approval_policy]]` rules with `ttl_secs`
fallbacks. Pitboss now emits a startup warning to stderr when it detects
`default_approval_policy = "block"` (or unset) AND no TTY on stdout — read it
before assuming a hang is something else.

### `require_plan_approval = true` is usually wrong headless

Setting `[run].require_plan_approval = true` blocks the run until an
operator approves the lead's first `propose_plan` call. In headless mode
this hangs unless the lead's `propose_plan` includes a `ttl_secs` +
`fallback`. If you see a task land with `status: "ApprovalRejected"` (v0.7+)
or `"ApprovalTimedOut"` (v0.8+, TTL elapsed and fallback fired), this gate is
the most likely cause.

### Hierarchical mode with `use_worktree = true` requires a git repo

When `use_worktree = true` (the default), both the lead and each worker
directory must be inside a git repository — pitboss calls `git worktree add`
to create an isolated checkout per actor. With `use_worktree = false` no git
repository is required; actors share the directory directly.

If you dispatch a hierarchical manifest in a fresh workspace with worktrees
enabled, initialise the repo first:

```bash
git init /workspace
git -C /workspace commit --allow-empty -m "init"
```

Flat mode (`[[task]]` only, no `[lead]`) follows the same rule — it only
needs a git repo when `use_worktree = true`.

### Reading run status without the TUI

```bash
# Snapshot table of all tasks (in-flight or completed):
pitboss status <run-id-prefix>

# Machine-readable JSON array:
pitboss status <run-id-prefix> --json
```

Other low-level options:

- `pitboss attach <run-prefix> <task-id>` — follow a specific worker's stream-json
- `cat <run-dir>/summary.jsonl` — completed tasks (streamed append-only)
- `cat <run-dir>/summary.json` — full summary on clean finalize
- `ls <run-dir>/tasks/` — all spawned task directories
- `cat <run-dir>/tasks/<actor-id>/events.jsonl` — Path-B `tool_denied`
  rows (the per-actor audit trail). `pitboss status` adds a `DENIED`
  column when any actor recorded a denial; the TUI's Detail view
  surfaces the last 5 rows in a `RECENT DENIALS` section so an operator
  can see what was blocked without grepping the file (#405).

The root lead's logs live at `<run-dir>/tasks/<lead-id>/stdout.log`.
Workers and sub-leads live at `<run-dir>/tasks/<task-id>/` and
`<run-dir>/tasks/<sublead-id>/` respectively. Sub-lead ids are
`sublead-<uuid>` — they don't match the manifest's `[lead].id`. Use
the UUID form from `summary.jsonl` when calling `pitboss attach`.

### Diagnosing OOM-kills + memory pressure (v0.15+)

When a `pitboss container-dispatch` run records a cluster of worker
failures with `exit_code: null`, `terminate_reason: null`, and empty
stderr, the most common root cause is the podman VM OOM-killing the
workers — pitboss's audit chain correctly reports "not me," but pre-
#553 the operator had to drop into `podman machine ssh -- sudo dmesg`
to confirm. With `[run].resource_sample_secs > 0` (default 5),
inspect the finalize roll-up directly:

```bash
jq .resource_high_water <run-dir>/summary.json
# {
#   "total_rss_bytes_max": 1844000000,
#   "cgroup_memory_max_bytes": 2040000000,
#   "peak_utilization_pct": 0.904,
#   "rss_bytes_max_by_actor": { ... },
#   "sample_count": 412,
#   "sample_cadence_secs": 5
# }
```

`peak_utilization_pct ≥ 0.9` paired with a cluster of unattributed
worker failures is the canonical OOM signature. The fix is one of:

- Reduce `[lead].max_workers` (cuts the simultaneous-claude RSS).
- Raise the podman VM ceiling: `podman machine set --memory 4096`.
- Switch the manifest's `[container.copy]` over to mounts so the
  in-VM copy doesn't push RSS while workers are also live.

For in-flight runs, the dispatcher broadcasts `resource_pressure`
envelopes on the same wire as workers / approvals — `pitboss-web`
renders these as a banner above the Workers card on the Live tab
(at warn ≥70% / error ≥90%, debounced clear at sustained <60%),
and the Resources tab shows the cgroup-headroom chart + per-actor
RSS sparklines.

### Sweeping orphaned runs (`pitboss prune`)

A run is *orphaned* when its dispatcher exited uncleanly (`SIGKILL`,
OOM, segfault, host crash) and never finalized `summary.json`. These
show up in `pitboss list` as `Stale` (no live control socket and
`summary.jsonl` mtime > 4h) or `Aborted` (no records at all).

```bash
pitboss prune                             # dry-run: report what would be swept
pitboss prune --apply                     # commit: synthesize Cancelled summary.json from partial state
pitboss prune --apply --remove            # commit: delete the run dir entirely instead
pitboss prune --apply --older-than 24h    # only sweep runs older than 24h (avoid in-flight investigation)
pitboss prune --include-aborted           # also sweep Aborted runs (off by default — might be a still-spinning-up dispatcher)
```

Defaults to dry-run so you can see what would happen first; `--apply`
commits. Default action is to synthesize a Cancelled `summary.json` that
reflects whatever partial state landed in `summary.jsonl` — preserves
audit trail. Pass `--remove` if you want the directory gone entirely.

`--older-than` accepts `60s`, `30m`, `4h`, `1d`, or a bare seconds
integer. Set this when you don't want fresh failures swept while you're
still mid-investigation.

### Terminal-state classification (v0.7+)

A task that exited because an approval returned negative is classified
distinctly from a task that succeeded:

| Status | Meaning |
|--------|---------|
| `Success` | Task completed work and exited cleanly |
| `Failed` | Task exited with non-zero status |
| `TimedOut` | Task exceeded `timeout_secs` |
| `Cancelled` | Task was explicitly cancelled by operator or cascade |
| `SpawnFailed` | Task never started (worktree prep, claude not found, etc.) |
| `ApprovalRejected` | Task's last approval returned `{approved: false}` from operator action or `[[approval_policy]]` auto_reject rule, then exited shortly after |
| `ApprovalTimedOut` | Task's last approval aged past its declared `ttl_secs` and the configured `fallback` fired (v0.8: TTL is wired end-to-end via `BridgeEntry` — covers both queued approvals and ones already drained into a connected TUI). The task's `ApprovalResponse` carries `from_ttl: true` so downstream consumers can distinguish TTL-driven from operator-driven responses. |

Before v0.7, both new statuses were reported as `Success` because the
claude subprocess exited 0. If you see `ApprovalRejected` in
`summary.json`, your manifest's `approval_policy` (or operator action)
denied the actor's request — revisit the approval configuration.

### Customizing sub-lead env and tools per spawn (v0.7+)

`spawn_sublead` accepts optional `env` and `tools` parameters:

```
spawn_sublead(
  prompt: "...",
  model: "claude-sonnet-4-6",
  budget_usd: 2.0,
  max_workers: 4,
  env: { "MY_VAR": "value" },        // merged over pitboss defaults
  tools: ["Read", "Bash"]            // adds to standard sublead toolset
)
```

Both fields are optional. Operator-supplied `env` keys override pitboss
defaults (including `CLAUDE_CODE_ENTRYPOINT` if you really want claude's
own gate back for that sub-lead). Operator-supplied `tools` are added
to the standard sublead MCP toolset; pitboss orchestration tools are
always present regardless of override.

### Offline access to this doc

```bash
pitboss agents-md                                    # from any binary
cat /usr/share/doc/pitboss/AGENTS.md                  # from a container
```

Both routes serve the same bytes — `AGENTS.md` is `include_str!`'d into
the binary at compile time and `COPY`'d into the container image.
`pitboss_version` in the frontmatter matches the binary version.

---

## Interpreting a run directory

After `pitboss dispatch` finishes, find the run via:

```bash
RUN_DIR=$(ls -td ~/.local/share/pitboss/runs/*/ | head -1)
```

Files in the run dir:

| File | Purpose |
|---|---|
| `manifest.snapshot.toml` | Exact manifest bytes used for this run. For container-dispatched runs this copy has the `[container]` section stripped (back-compat with older images that hit `deny_unknown_fields` on the key); see `manifest.source.toml` for the operator-typed original. |
| `manifest.source.toml` | **Container-dispatch only.** Written host-side by `pitboss container-dispatch` before `exec()`. Preserves the operator's original manifest including the `[container]` block + `[[container.mount]]` entries. `pitboss-web`'s Fork-manifest button prefers this over the snapshot so forked container runs carry their full host-side config back into the workspace. |
| `resolved.json` | Fully resolved manifest (defaults applied). |
| `meta.json` | `run_id`, `started_at`, `claude_version`, `pitboss_version`. |
| `summary.json` | Written on clean finalize. Full structured summary of the run. |
| `summary.jsonl` | Appended incrementally as tasks finish. Useful for live observation. |
| `events.jsonl` | **Opt-in** (v0.13+, `[run].emit_event_stream = true`). Run-wide control-event log: every `EventEnvelope` the dispatcher would have sent on the wire, persisted as it fires. Survives headless dispatches (no TUI / web bridge). See [`events.jsonl` structure](#eventsjsonl-structure-v013-opt-in). |
| `audit.jsonl` | **v0.14+** (#414). Run-wide aggregated audit log: every per-actor `TaskEvent` row (`pause` / `continue` / `reprompt` / `approval_request` / `approval_response` / `notification_failed` / `tool_denied` / `tool_auto_approved`) tee'd from the per-actor file with an explicit `actor_id` field. Always-on, created lazily on first event. Query via `pitboss audit <run-id> [--actor X] [--kind K] [--since T] [--reason-kind R]` or `GET /api/runs/<id>/audit` with the same filters as query params. |
| `tasks/<id>/stdout.log` | Raw stream-json from the task's claude subprocess. |
| `tasks/<id>/stderr.log` | Stderr. |
| `tasks/<id>/events.jsonl` | **Distinct from the run-level file above.** Per-actor audit log: `pause` / `continue` / `reprompt` / `tool_denied` / `tool_auto_approved` / `approval_request` rows. Always-on (created lazily on first append). The same rows are tee'd into the run-wide `audit.jsonl` for chronological cross-actor queries. |
| `lead-mcp-config.json` | Hierarchical only. The `--mcp-config` file pointed at `pitboss mcp-bridge <socket>`. |

### `summary.json` structure

```json
{
  "run_id": "019d9b...",
  "started_at": "2026-04-17T12:14:22Z",
  "ended_at":   "2026-04-17T12:14:55Z",
  "total_duration_ms": 32654,
  "tasks_total": 4,
  "tasks_failed": 0,
  "was_interrupted": false,
  "pitboss_version": "0.3.3",
  "claude_version":  "2.1.112 (Claude Code)",
  "tasks": [
    {
      "task_id": "triage",
      "status": "Success" | "Failed" | "TimedOut" | "Cancelled" | "SpawnFailed",
      "exit_code": 0,
      "started_at": "...",
      "ended_at":   "...",
      "duration_ms": 22649,
      "worktree_path": "/path/to/worktree-or-null",
      "log_path":      "/path/to/tasks/triage/stdout.log",
      "token_usage": {
        "input": 26,
        "output": 1373,
        "cache_read":     72647,
        "cache_creation": 37413
      },
      "claude_session_id": "...",
      "final_message_preview": "Done. Composed summary...",
      "parent_task_id": null | "lead-id"
    }
  ]
}
```

Lead records have `parent_task_id: null`. Worker records have
`parent_task_id: "<lead-id>"`. Query with `jq`.

### `events.jsonl` structure (v0.13+, opt-in)

Enabled by `[run].emit_event_stream = true`. The dispatcher writes
one `EventEnvelope` per line as the event fires on the live wire,
so the file is consistent with what a connected TUI / web bridge
would have observed. Headless dispatches (no client ever attached)
still produce a complete log — persistence rides on a dispatcher-
owned bus subscriber, not on the per-connection pump.

```json
{"actor_path":["root","sub-1"],"seq":3,"event":"sublead_spawned","sublead_id":"sub-1","budget_usd":0.5,"max_workers":2,"read_down":false}
{"actor_path":["root","sub-1"],"seq":4,"event":"worker_failed","task_id":"w-1","parent_task_id":"sub-1","reason":{"kind":"auth_failure"}}
{"actor_path":["root","sub-1","lead"],"seq":5,"event":"approval_request","request_id":"r-1","task_id":"t-1","summary":"run cargo test","kind":"action"}
```

Wire shape:

| Field | Type | Notes |
|---|---|---|
| `actor_path` | `string[]` (omitted when empty) | Tree path from root to the actor that produced the event. Run-level events (sub-lead lifecycle, root-layer transitions) have an empty path. |
| `seq` | `uint64` | Monotonic per-run sequence. Starts at 1. Pre-v0.13 envelopes default to 0 (legacy sentinel). Shared with the live wire — replaying disk up to seq=N then subscribing to SSE at seq=N+1 gives gap-free coverage. |
| `event` | snake-case discriminator | Carries the variant payload inline via `#[serde(flatten)]` — same shape as the live `/api/runs/:id/events` SSE feed. |

**What's in the log:** every envelope produced by the dispatcher's
`broadcast_control_event` path — `sublead_spawned`,
`sublead_terminated`, `worker_failed`, run-wide `approval_request`
(including ones queued before any client connected). As of v0.14
(PR-Q of #438) this also includes **op replies** — `op_acked`,
`op_failed`, `op_unknown_state`, `workers_snapshot` — which were
previously connection-scoped. The log now records every accepted
control op alongside what the dispatcher did with it (audit trail).
Also connection-scoped envelopes that ran during an attached session:
`approval_request` replay, `store_activity` ticks, `superseded`.

**What's not in the log:**

- `hello` handshakes — pure connection-bootstrap noise, filtered
  at the persist boundary.
- Connection-scoped envelopes that fired during periods when no
  client was attached (e.g. `store_activity` ticks only run while
  a TUI / web bridge is connected). The bus-routed events
  (sub-lead lifecycle, worker failures, approvals, op replies)
  persist unconditionally.

Three ways to consume the log:

```bash
# CLI: compact one-line summary, or --json for jq-friendly NDJSON
pitboss events <run-id>
pitboss events <run-id> --json | jq 'select(.event == "worker_failed")'

# HTTP: raw NDJSON over the web server (auth-gated like the rest of /api)
curl -H "Authorization: Bearer $TOKEN" \
     http://localhost:8080/api/runs/<run-id>/events-jsonl

# SPA: the "Replay" tab on the run-detail page (finalized runs only;
# in-progress runs still use the live SSE Live tab).
```

### `audit.jsonl` structure (v0.14+, #414)

Always-on. The dispatcher tees every per-actor `TaskEvent` write into
this run-wide file with an explicit `actor_id` field, giving operators
a chronological cross-actor view without walking
`tasks/*/events.jsonl` by hand.

```json
{"actor_id":"lead","event":{"kind":"approval_request","at":"2026-05-14T03:42:00Z","request_id":"r-1","summary_preview":"spawn 3 workers"}}
{"actor_id":"worker-7","event":{"kind":"tool_denied","at":"2026-05-14T03:42:05Z","tool_name":"Bash","actor_id":"worker-7","reason_kind":"denied_by_rule","reason":"shell access denied"}}
```

Wire shape: `{actor_id, event}` — `actor_id` is the run-wide
attribution key; `event` carries the existing per-actor `TaskEvent`
JSON verbatim (the `kind`-discriminated enum with payload fields). The
per-actor `tasks/<id>/events.jsonl` files remain durable; the audit
file is the run-wide companion.

Three ways to consume:

```bash
# CLI: columnar one-line-per-event with filters; --json for raw NDJSON.
pitboss audit <run-id>
pitboss audit <run-id> --actor worker-7 --kind tool_denied
pitboss audit <run-id> --since 2026-05-14T03:00:00Z --reason-kind operator_rejected
pitboss audit <run-id> --json | jq 'select(.event.kind == "approval_response")'

# HTTP: NDJSON over the web server with the same filter surface as
# query params (auth-gated).
curl -H "Authorization: Bearer $TOKEN" \
     'http://localhost:8080/api/runs/<run-id>/audit?actor=lead&kind=approval_response'
```

Write semantics: the tee is best-effort — a transient filesystem
failure on `audit.jsonl` logs a warning but doesn't break the durable
per-actor write. So a crashed dispatcher might leave the audit file
slightly stale relative to the per-actor files; the per-actor files
remain the source of truth.

---

## MCP socket & bridge auth (v0.10+)

Every hierarchical run stands up a per-run unix socket and a per-actor
auth token. Operators don't normally need to think about either — the
dispatcher mints, embeds, and tears down both — but agents reading the
source or debugging connection issues should know the contract.

### Socket

- Path: `$XDG_RUNTIME_DIR/pitboss/<run_id>.sock` (preferred), or fallback
  `<run_dir>/<run_id>/mcp.sock` if `$XDG_RUNTIME_DIR` is unset or
  non-writable. Computed by `mcp::socket_path_for_run`
  (`crates/pitboss-cli/src/mcp/server.rs`).
- `run_id` is a UUIDv7 → two concurrent runs cannot collide on a path.
- Containing directory is created with mode `0o700`; the socket file
  itself is chmod'd to `0o600` immediately after `bind` to close the
  umask race window.
- Created by `McpServer::start`; removed by `McpServer::shutdown` and
  as a best-effort `Drop`. Any stale `.sock` from a crashed prior run
  is removed pre-bind.

### Token

- Format: UUIDv7 string, minted by
  `DispatchState::mint_token(actor_id, role)` and stored in-memory in
  `DispatchState.actor_tokens: HashMap<token, ActorIdentity>`. There is
  no on-disk authoritative table; the `mcp-config.json` written for the
  actor is the only on-disk artifact carrying the token.
- Mint sites (one per actor lifecycle event):
  - **Root lead** — `dispatch/hierarchical.rs`, at dispatch start.
  - **Sublead** — `dispatch/sublead.rs`, when `spawn_sublead` is handled.
  - **Worker** — `mcp/tools/spawn.rs`, at `spawn_worker` and at the
    resume path.
- The token is appended to the bridge arg vector (`--token <hex>`)
  inside the actor's `mcp-config.json` (`build_mcp_servers_json` in
  `dispatch/hierarchical.rs`). The config file is written with mode
  `0o600` for the running user.
- The bridge (`pitboss mcp-bridge --token <hex>`) injects `_meta.token`
  on every JSON-RPC `tools/call` and forwards to the unix socket.
- The server validates `_meta.token` against
  `DispatchState.actor_tokens` in
  `PitbossHandler::authenticate_and_rebind` (`mcp/server.rs`). On hit
  it binds the connection's canonical identity from the lookup
  result (NOT the wire `_meta.actor_id`, which is forge-able) and
  strips the token before downstream handlers see it. Unknown or
  forged tokens → `invalid_request("invalid actor token …")`.
- Tokens are revoked at actor termination (closes F-SEC-1 / #523):
  - **Worker exits** — `run_worker` and `spawn_resume_worker` revoke
    their own iteration's token in the finalize tail via
    `DispatchState::revoke_token(&str)`. Per-token (not actor-id-wide)
    so a slow finalize can't race-drop a fresh token that a
    concurrent `spawn_resume_worker` call just minted for a
    reprompted subprocess.
  - **Sub-lead exits** — `reconcile_terminated_sublead` calls
    `revoke_tokens_for_actor(&sublead_id)` (actor-id-wide; sub-leads
    have one token for their lifetime, no kill+resume re-mint).
  - **Lead exits** — `run_hierarchical` calls
    `revoke_tokens_for_actor(&state.root.lead_id)` before
    `mcp.shutdown()`.
- **Known limitation (per-connection identity cache):**
  `authenticate_and_rebind` short-circuits on `connection_identity`
  before consulting `lookup_token`. Once a bridge connection has
  bound its identity (on its first `tools/call`), subsequent calls on
  the SAME connection inherit the cached identity without
  re-validating the token. A connection bound BEFORE revoke fires
  retains its access until the connection drops. In normal operation
  the bridge subprocess exits with its parent claude, closing the
  connection; the surviving threat is a malicious same-UID process
  that holds the socket fd open past actor exit.

### Invariants

- No two simultaneous runs share a socket path (`run_id` collision is
  the only way, modulo UUIDv7).
- Only the running user can `connect()` the socket (`0o600` socket +
  `0o700` containing dir).
- A `tools/call` without a valid `_meta.token` is rejected; an
  `_meta.actor_id` without a token is also rejected (no anonymous
  fallback — closes #309/#310).
- Tokens minted in run A's `DispatchState` are unknown to run B's
  `DispatchState` (in-memory tables are per-process); cross-run replay
  is rejected at `authenticate_and_rebind`. Pinned by
  `concurrent_runs_isolate_sockets_and_tokens` in
  `tests/hierarchical_flows.rs`.

### Threat model

- **In scope:** other local processes under different UIDs, accidental
  cross-run wiring during refactors.
- **Out of scope:** same-UID processes with read access to the
  run_subdir, kernel-level attackers, network attackers (the socket
  is local-only).

---

## Control socket protocol (v0.14+)

Each run also stands up a **control socket** distinct from the MCP
socket. It's a per-run Unix socket carrying line-delimited JSON ops
(`ControlOp`) and event envelopes (`EventEnvelope` wrapping
`ControlEvent`). The TUI, the web SSE bridge, `pitboss-core::stream`'s
live transport, and any future consumer that wants to read the live
event stream all attach over this socket.

### Socket

- Path: `$XDG_RUNTIME_DIR/pitboss/<run_id>.control.sock` (preferred),
  or fallback `<run_dir>/<run_id>/control.sock`. Resolved by
  `pitboss_cli::control::control_socket_path` (re-exported as
  `pitboss_core::control_protocol::resolve_control_socket`).
- Bound by `start_control_server` at dispatch start, removed when the
  `ControlServerHandle` drops.

### Handshake: `Hello { client_version, mode }`

Every client MUST send `ControlOp::Hello` as its first line. The
`mode: ClientMode` field selects the connection role:

| `mode` | Role | Behavior |
|---|---|---|
| `"writer"` (default, elided on the wire) | TUI / `pitboss-web::control_bridge::send_op` / any client that sends mutating ops | Takes the single `control_writer` slot, displacing any prior writer with a `Superseded` event. Receives the queued-approval drain + bridge replay. |
| `"subscriber"` | `pitboss-core::stream::drive_live`; any read-only mirror | Does NOT take the writer slot — coexists with one writer + any number of other subscribers. Skips approval-drain / bridge-replay (subscribers can't respond). Writer ops are rejected up-front with `OpFailed { error: "subscriber mode forbids writer ops" }`. |

Pre-v0.14 clients send `Hello` without `mode`; `#[serde(default)]`
deserializes them as `Writer`, preserving byte-identical wire and
historical semantics. Subscriber connections may send `Hello` (no-op
ack) and `Subscribe { since_seq }` (advisory subscribe ack from
PR-N); every other op returns `OpFailed`.

The dispatcher fans out broadcast envelopes (`broadcast_control_event`)
to every connected client — writer and all subscribers — via the
per-run `events_tx: broadcast::Sender<EventEnvelope>`. As of v0.14
(PR-Q of #438) this includes **op replies** (`OpAcked`, `OpFailed`,
`OpUnknownState`, `WorkersSnapshot`) — they're routed through the bus
so multi-viewer SSE bridges and unified-API consumers all see the same
reply stream, and the replies persist in `events.jsonl` when
`emit_event_stream = true`. Connection-scoped envelopes that remain
direct: server `Hello` (per-connection bootstrap), `Superseded`
(targeted at the displaced writer), `StoreActivity` ticks
(per-connection ticker), and the queued-approval drain / bridge replay
(a reconnecting TUI needs its own re-delivery, but a fresh subscriber
doesn't).

---

## The MCP tools the lead has

When running hierarchical, the lead's `--allowedTools` is automatically
populated with these. You (the operator) don't list them explicitly.

### Orchestration tools

| Tool | Args | Returns |
|---|---|---|
| `mcp__pitboss__spawn_worker` | `{prompt, directory?, branch?, tools?, timeout_secs?, model?}` | `{task_id, worktree_path}` |
| `mcp__pitboss__worker_status` | `{task_id}` | `{state, started_at, partial_usage, last_text_preview, prompt_preview}` |
| `mcp__pitboss__wait_for_worker` | `{task_id, timeout_secs?}` | full `TaskRecord` when worker settles |
| `mcp__pitboss__wait_actor` | `{actor_id, timeout_secs?}` | `ActorTerminalRecord` (`Worker(TaskRecord)` or `Sublead(SubleadTerminalRecord)`) when actor settles. Accepts worker or sub-lead ids. `wait_for_worker` is a back-compat alias. |
| `mcp__pitboss__wait_for_any` | `{task_ids: [...], timeout_secs?}` | `{task_id, record}` on first settle |
| `mcp__pitboss__list_workers` | `{}` | `{workers: [{task_id, state, prompt_preview, started_at}, ...]}` |
| `mcp__pitboss__cancel_worker` | `{task_id, reason?: string}` | `{ok: bool}` — optional `reason` delivers a synthetic `[SYSTEM]` reprompt to the killed actor's direct parent lead via kill+resume |
| `mcp__pitboss__pause_worker` | `{task_id, mode?}` — `mode` is `"cancel"` (default) or `"freeze"` | `{ok: bool}` |
| `mcp__pitboss__continue_worker` | `{task_id, prompt?}` | `{ok: bool}` |
| `mcp__pitboss__reprompt_worker` | `{task_id, prompt}` | `{ok: bool}` — mid-flight course-correct via `claude --resume` |
| `mcp__pitboss__request_approval` | `{summary, timeout_secs?, plan?: ApprovalPlan}` | `{approved, comment?, edited_summary?, reason?}` |
| `mcp__pitboss__propose_plan` | `{plan: ApprovalPlan, timeout_secs?}` | `{approved, comment?, edited_summary?, reason?}` |
| `mcp__pitboss__spawn_sublead` | `{prompt, model, budget_usd?, max_workers?, lead_timeout_secs?, initial_ref?, read_down?, env?, tools?, resume_session_id?}` | `{sublead_id}` — root lead only; requires `[lead] allow_subleads = true`. `resume_session_id` is used by `pitboss resume` to re-attach a prior sub-lead session; omit for fresh spawns. See Depth-2 section. |
| `mcp__pitboss__run_lease_acquire` | `{key, ttl_secs, wait_secs?}` | `{lease_id, version, ...}` — run-global; auto-released on actor termination |
| `mcp__pitboss__run_lease_release` | `{lease_id}` | `{ok: true}` |
| `mcp__pitboss__analyze_run` | `{run_id}` | `RunAnalysis` — header (id / manifest / mode / status / duration / versions), `cost` rollup (per-model + lead-vs-worker), `failures` grouped by classified `FailureReason::kind`, `tasks` (workers grouped under their lead), `hotspots` (longest / costliest / most-tokens). Read-only triage over a prior run; resolves `run_id` against the canonical runs base directory. |
| `mcp__pitboss__analyze_recent` | `{limit?, failed_only?}` | `RecentAnalysis { runs, aggregates }` — per-run analyses plus a cross-run aggregate (failure histogram, model usage, top-3 slowest / costliest / most-failed runs). `limit` defaults to 10 and is silently clamped to 50. `failed_only` skips runs with zero failed tasks. |
| `mcp__pitboss__message_send` | `{to, subject, body, refs?}` | `{message_id}` — opt-in via `[communication] mode = "parent_child"` (default disabled). Workers may target `"parent"` or self; subleads may target `"root"` or workers in their own sub-tree; root may target any actor. Body capped by `max_message_bytes`. |
| `mcp__pitboss__message_list` | `{scope?}` | `{messages: [...]}` — `scope` defaults to `inbox`. Other scopes: `sent`, `visible`, `all` (root only). |
| `mcp__pitboss__message_read` | `{message_id}` | `{message}` — parents can read child messages; workers can read only messages they sent or received. |
| `mcp__pitboss__message_ack` | `{message_id}` | `{ok: true}` |
| `mcp__pitboss__artifact_put` | `{name, mime_type?, content_base64}` | `{artifact_id, uri, size_bytes, sha256}` — bytes land at `<run_dir>/communication/artifacts/<id>`. Subject to `max_artifact_bytes` and `max_artifacts_per_actor`. |
| `mcp__pitboss__artifact_list` | `{scope?}` | `{artifacts: [...]}` — `scope` defaults to `visible`. Other scopes: `owned`, `granted`, `all` (root only). |
| `mcp__pitboss__artifact_read` | `{artifact_id}` | `{artifact, content_base64}` |
| `mcp__pitboss__artifact_grant` | `{artifact_id, to}` | `{ok: true}` — caller must already be allowed to read the artifact and address the recipient. Used by parents to enable sibling-worker handoffs. Read access only; ownership does not transfer. |

All tool responses returning a collection are wrapped in a record
(`{workers: [...]}`, `{entries: [...]}`, `{entry: ...}`) — MCP spec
requires `structuredContent` to be `{ [key: string]: unknown }`, so
tools don't return bare arrays or null. Unwrap one level from callers.

**Worker spawn arg rules:**
- `prompt` is the new worker's system prompt / `-p` payload. Required.
- `directory` defaults to the lead's `directory`.
- `model` defaults to the lead's model. Override per-worker when you want
  heavier workers (Sonnet) under a Haiku lead.
- `tools` defaults to the lead's tools.

### Worker shared store (v0.4.2+)

A per-run, in-memory, hub-mediated coordination surface. Workers get
a narrower `mcp-config.json` that lists only the seven tools below
(not `spawn_worker` or `spawn_sublead` — workers are terminal). Namespaces:

- `/ref/*` — lead-write, all-read. Use for shared context (plans,
  conventions, targets).
- `/peer/<actor-id>/*` — actor-write (own path only), lead-override.
  Use for per-worker outputs like `/peer/self/completed`.
- `/shared/*` — all-write. Use for loose cross-worker coordination
  like `/shared/findings/`.
- `/leases/*` — managed via `lease_acquire` / `lease_release` only.

Workers don't know their UUID actor-id; use the `/peer/self/` alias —
the dispatcher resolves it to `/peer/<caller.actor_id>/` at the tool
layer.

| Tool | Args | Returns |
|---|---|---|
| `mcp__pitboss__kv_get` | `{path}` | `{entry: Option<Entry>}` |
| `mcp__pitboss__kv_set` | `{path, value: bytes, override_flag?}` | `{version}` |
| `mcp__pitboss__kv_cas` | `{path, expected_version, new_value: bytes, override_flag?}` | `{version, swapped}` |
| `mcp__pitboss__kv_list` | `{glob}` | `{entries: [ListMetadata, ...]}` |
| `mcp__pitboss__kv_wait` | `{path, timeout_secs, min_version?}` | `Entry` when condition met |
| `mcp__pitboss__lease_acquire` | `{name, ttl_secs, wait_secs?}` | `{lease_id, version, ...}` |
| `mcp__pitboss__lease_release` | `{lease_id}` | `{ok: true}` |

### `mcp__pitboss__pause_worker`

Pause a running worker. Two modes, distinguished by `mode`:

- `mode: "cancel"` (default, v0.4.1+) — terminates the subprocess and
  snapshots `claude_session_id` so `continue_worker` can respawn via
  `claude --resume`. Zero context loss on Anthropic's side; some
  reload cost on resume.
- `mode: "freeze"` (v0.5.0+) — SIGSTOPs the subprocess in place.
  `continue_worker` SIGCONTs to resume. No state loss at all, but
  long freezes risk Anthropic dropping the HTTP session on their
  side — use for short pauses only.

Args: `{task_id: string, mode?: "cancel" | "freeze"}`. Fails if
worker is not in `Running` state with an initialized session.

### `mcp__pitboss__continue_worker`

Continue a previously-paused or frozen worker. For paused workers,
spawns `claude --resume <id>`; for frozen workers, SIGCONTs. Args:
`{task_id: string, prompt?: string}` (prompt ignored for frozen
workers — use `reprompt_worker` after continue if you want to
redirect a frozen worker).

### `mcp__pitboss__request_approval`

Gate a *single in-flight action* on operator approval. Block the lead
until the operator approves, rejects, or edits. Args:
`{summary: string, timeout_secs?: number, ttl_secs?: number, fallback?: "auto_reject"|"auto_approve"|"block", plan?: ApprovalPlan}`.
Returns `{approved: bool, comment?: string, edited_summary?: string, reason?: string}`.
When `approved = false`, `reason` carries the operator's rejection explanation if provided.
Policy-gated: see `approval_policy` below.

`ApprovalPlan` (v0.5.0+) is a typed structured schema that the TUI
renders as labeled sections:

```
{
  summary: string,              // required; appears in the modal title
  rationale?: string,           // why this action should be taken
  resources?: [string, ...],    // files / APIs / PRs that will be touched
  risks?: [string, ...],        // known failure modes; TUI highlights in warning color
  rollback?: string,            // how to undo if something goes wrong
}
```

Populate `plan` for any non-trivial approval (deletions, multi-file
edits, irreversible ops). The bare `summary` form still works for
simple approvals.

### `mcp__pitboss__propose_plan`

Gate the *entire run* on operator pre-flight approval. Distinct from
`request_approval`, which gates individual actions mid-run. Args:
`{plan: ApprovalPlan, timeout_secs?: number}`. Returns the same
shape as `request_approval`.

When `[run].require_plan_approval = true`, `spawn_worker` refuses
with `plan approval required: call propose_plan ...` until a plan
submitted via this tool has been operator-approved. The TUI modal
shows `[PRE-FLIGHT PLAN]` in its title (vs `[IN-FLIGHT ACTION]` for
`request_approval`) so operators can tell them apart at a glance. On
rejection, the gate stays closed so the lead can revise and retry.

When `require_plan_approval = false` (the default), calling
`propose_plan` is still valid but purely informational — `spawn_worker`
never checks the result.

---

## Error patterns

### `budget exceeded: $<spent> spent + $<reserved> reserved + $<estimated> estimated > $<budget> budget`

The lead tried `spawn_worker` with insufficient budget headroom. Two
reactions:
1. **Fall back gracefully.** Finish the work the lead already has, compose
   a partial report, note the budget hit in the final output.
2. **Request a larger budget from the operator.** If you're orchestrating
   pitboss from natural language, surface this error back to the human and
   ask whether to re-run with a higher cap.

Don't loop calling `spawn_worker` after budget exhaustion — each call costs
nothing structurally but adds noise to the logs.

### `worker cap reached: N active (max M)`

More than `max_workers` workers are Pending/Running. Wait for one to finish
(via `wait_for_worker` or `wait_for_any`), then retry the spawn.

### `run is draining: no new workers accepted`

The operator Ctrl-C'd or the lead's `cancel` was tripped. Finish gracefully;
don't spawn new work.

### `unknown task_id`

You're referring to a worker that was never spawned (or was typo'd).
`mcp__pitboss__list_workers` shows what's actually registered.

### `SpawnFailed`

A worker never started — usually a git worktree prep failure (dirty tree,
branch conflict, non-git directory). Check the stderr log.

---

## Operator keybindings (pitboss-tui, v0.6.0+)

Navigation / views:
- `h j k l` / arrows — navigate tiles
- `Tab` — cycle focus across sub-tree containers (v0.6+; depth-2 runs)
- `Enter` — open Detail view for focused tile (metadata pane + live
  git-diff + scrollable log); on a sub-tree container header,
  toggles expand/collapse
- `a` — focus the approval list pane (right-rail, non-modal; v0.6+)
- `o` — run picker (switch to another run)
- `?` — help overlay (full keybinding reference)
- `q` / `Ctrl-C` — quit
- `Esc` — close any overlay / modal

Mouse:
- Left-click a grid tile — focus + open Detail
- Left-click a run in the picker — open that run
- Right-click inside Detail — exit back to grid
- Scroll wheel inside Detail — scroll log 5 rows/tick

Scroll cadence inside Detail:
- `j` / `k` / arrows — 1 row
- `J` / `K` — 5 rows
- `Ctrl-D` / `Ctrl-U` / `PageDown` / `PageUp` — 10 rows
- `g` / `G` — jump to top / bottom (bottom re-enables auto-follow)

Control plane:
- `x` — confirm+cancel focused worker
- `X` — confirm+cancel entire run (cascades SIGTERM to every worker)
- `p` — pause focused worker (requires initialized session)
- `c` — continue paused worker
- `r` — open reprompt textarea (Ctrl+Enter to submit, Esc to cancel)
- During approval modal: `y` approve, `n` reject (with optional reason
  string, Ctrl+Enter to submit), `e` edit (Ctrl+Enter to submit, Esc to cancel)

Approval list pane (`'a'` to focus, v0.6+):
- `Up` / `Down` — navigate pending approvals
- `Enter` — open detail modal for the highlighted approval

## `[run].approval_policy`

Run-level scalar. Controls handling of `request_approval` calls when
no TUI is attached and no `[[approval_policy]]` rule matches.

- `"block"` (default) — queue until a TUI connects, or fail after
  `lead_timeout_secs`.
- `"auto_approve"` — immediate `{approved: true}`.
- `"auto_reject"` — immediate `{approved: false, comment: "no operator
  available"}`.

## `[[approval_policy]]` blocks (v0.6+)

Ordered list of deterministic rules evaluated in pure Rust before
approvals reach the operator queue. **NOT LLM-evaluated.** First match
wins; unmatched approvals fall through to `[run].approval_policy`.

```toml
[[approval_policy]]
match = { actor = "root", category = "tool_use", tool_name = "Bash" }
action = "auto_approve"

[[approval_policy]]
match = { category = "cost", cost_over = 1.00 }
action = "block"

[[approval_policy]]
match = { actor = "root→S1" }
action = "auto_reject"
```

Match fields (all optional; unset fields match any value):

| Field | Type | Notes |
|---|---|---|
| `actor` | string | `ActorPath` rendered as `"root"` or `"root→S1"` or `"root→S1→W3"` |
| `category` | string | snake_case enum: `"tool_use"`, `"plan"`, `"cost"`, `"other"` |
| `tool_name` | string | Exact tool name; only meaningful when `category = "tool_use"` |
| `cost_over` | float | Matches when the `cost_estimate` hint on the approval exceeds this USD value. **Advisory only** — `cost_estimate` is caller-supplied; an actor that passes `cost_estimate=0.0` bypasses any threshold. For hard cost gates use `auto_reject` on `tool_name`/`actor`, or the server-side `[run].budget_usd` cap. (F-SEC-8 / #531) |

Action values (snake_case):

| Action | Effect |
|---|---|
| `"auto_approve"` | Immediate approved response, no operator queue entry |
| `"auto_reject"` | Immediate rejected response with optional `reason` |
| `"block"` | Force into operator queue regardless of run-level policy |

For full syntax reference and defense-in-depth patterns see:
- [Approval policy reference](https://sds-mode.github.io/pitboss/operator-guide/approval-policy-reference.html)
- [Defense-in-depth](https://sds-mode.github.io/pitboss/security/defense-in-depth.html)

### Reject-with-reason (v0.6+)

When the operator rejects an approval, an optional `reason: string` is
accepted in the modal. The reason flows back through MCP to the
requesting actor's session so Claude can adapt without a separate
reprompt round-trip. Appears in the `reason` field of the approval
response alongside `approved: false`.

### Approval TTL + fallback (v0.6+)

`request_approval` accepts optional `ttl_secs` and `fallback` hints:
- `ttl_secs` — seconds after which the approval auto-resolves
- `fallback` — action to take on TTL expiry: `"auto_reject"` (default),
  `"auto_approve"`, or `"block"` (requeue)

Prevents unreachable operators from permanently stalling a run. Set a
short TTL + `fallback = "auto_reject"` on approvals that should not
block indefinitely if the operator steps away.

---

## Canonical examples

### 1. Fan out a summarization across N files

Flat mode, predeclared tasks.

```toml
[run]
max_parallel_tasks = 3

[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[[task]]
id = "summarize-a"
directory = "/path/to/repo"
prompt = "Read file-a.txt and summarize in one sentence to /tmp/summaries/a.md"

[[task]]
id = "summarize-b"
directory = "/path/to/repo"
prompt = "Read file-b.txt and summarize in one sentence to /tmp/summaries/b.md"

# ... etc
```

### 2. Lead decides fanout based on the input

Hierarchical mode, dynamic decomposition.

```toml
[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[lead]
id = "author-digest"
directory = "/path/to/repo"
prompt = """
List the last 20 commits with `git log --format='%H %an %s' -20`. Group
them by author. Spawn one worker per unique author via
mcp__pitboss__spawn_worker to summarize that author's work in
/tmp/digest/<author-slug>.md. Wait for all via mcp__pitboss__wait_for_worker.
Compose a combined /tmp/digest/SUMMARY.md. Then exit.
"""
max_workers = 6
budget_usd = 1.50
lead_timeout_secs = 1200
```

### 3. Refactor analysis of a neighboring repo

This pattern was exercised end-to-end against the
[Ketchup](https://github.com/SDS-Mode/ketchup) plugin on
2026-04-17 (branch
[`feature/pitboss-refactor-analysis`](https://github.com/SDS-Mode/ketchup/tree/feature/pitboss-refactor-analysis)).
4 Haiku workers ran in parallel auditing one angle each — SKILL.md
structure, 16 CLI parsing rules, cross-file overlap, README-vs-SKILL.md
separation — and a Haiku lead synthesized into a single `REFACTOR-PLAN.md`
with 11 prioritized changes (5 P0, 4 P1, 2 P2) and a ~12% token-footprint
reduction estimate. **Total: 5 tasks, 0 failed, 201 s wall-clock, under
$0.40 on Haiku.**

Sketch of the manifest:

```toml
[run]
worktree_cleanup = "never"

[defaults]
model = "claude-haiku-4-5"
use_worktree = false        # read-only audit — no worktree isolation needed

[lead]
id = "refactor-analyst"
directory = "/path/to/target-repo"
prompt = """
[...concrete directions for spawning 4 workers, one per audit angle,
each writing to /tmp/refactor/<angle>.md; then reading them back and
synthesizing into /tmp/refactor/REFACTOR-PLAN.md...]
"""
max_workers = 4
budget_usd = 1.50
lead_timeout_secs = 1500
```

The lead spawned workers in an explicit loop, used
`mcp__pitboss__wait_for_worker` on each before reading their outputs,
then composed the synthesis. Key patterns the lead applied:

- Each worker received a **specific file path** to write to, so the lead
  could read the results back deterministically.
- Workers received **per-angle instructions**, not the whole repo — keeps
  their context tight and their output focused.
- The lead's synthesis prompt explicitly asked for *executive summary,
  before/after file structure, prioritized list, risk section* — giving
  the output a predictable shape.

Run this pattern against any repo of similar shape by:
1. Listing the 3–5 files that matter most.
2. Choosing 4 audit angles (structural / rules / overlap / user-vs-internal
   / test-coverage / dependencies — pick what applies).
3. Writing worker prompts that each produce one focused analysis file.
4. Writing a lead-synthesis prompt that reads those files back and
   composes an actionable plan.

### 4. Tight-budget stress / graceful degradation

```toml
[defaults]
model = "claude-haiku-4-5"
use_worktree = false

[lead]
id = "partial"
directory = "/path/to/repo"
prompt = """
Attempt to spawn 6 workers with mcp__pitboss__spawn_worker, one per file in
src/. When a spawn fails with 'budget exceeded', DO NOT retry — record the
file and move on. Wait for successfully-spawned workers, then compose a
partial summary noting which files were skipped and why.
"""
max_workers = 8
budget_usd = 0.20
lead_timeout_secs = 900
```

Use this pattern when you want to explore *what you can get* within a fixed
spend envelope.

---

## Depth-2 sub-leads (v0.6+)

Pitboss supports a single optional level of nesting beyond the original
hierarchical mode. A root lead with `allow_subleads = true` can spawn
sub-leads at runtime via `spawn_sublead`. Each sub-lead is itself a
Claude session with its own workers. Workers remain terminal — they
cannot spawn anything.

### When to use sub-leads

Use sub-leads when the root lead's plan would otherwise require holding
context for orthogonal sub-tasks simultaneously (e.g., "phase 1 and
phase 2 both need their own decomposition tree, but they don't share
implementation details"). Each sub-lead gets a clean Claude session
focused on its own slice.

Use plain workers (no sub-leads) when each unit of work is a single
self-contained task. Sub-leads add coordination overhead; workers are
cheaper.

### Manifest

```toml
[lead]
id = "root"
directory = "/path/to/repo"
prompt = "..."
budget_usd = 20.00
max_workers = 12
allow_subleads = true
max_subleads = 8                # optional cap
max_sublead_budget_usd = 5.00   # optional cap on per-sub-lead envelope
max_total_workers = 20          # optional cap on total live workers (root + sub-trees)

[sublead_defaults]              # top-level (v0.9+, was [lead.sublead_defaults])
budget_usd = 2.00
max_workers = 4
lead_timeout_secs = 1800
read_down = false
```

### `spawn_sublead` MCP tool

```
spawn_sublead(
  prompt: string,
  model: string,
  budget_usd: float,           # required unless read_down=true
  max_workers: u32,            # required unless read_down=true
  lead_timeout_secs: u64?,
  initial_ref: { string: any }?,
  read_down: bool = false,
)
→ sublead_id: string
```

### Authz model

- **Strict tree by default.** Root cannot read into a sub-tree unless `read_down = true` was passed at spawn time.
- **Strict peer visibility.** At any layer, `/peer/<X>` is readable only by X itself, that layer's lead, or the operator (TUI). Workers within a sub-tree do NOT see each other's peer slots — coordinate via `/shared/*` or leases.
- **Operator (TUI) is super-user.** Read/write across all layers regardless of read-down.

### Lease scope-selection guidance

- Use `/leases/*` (per-layer KV namespace) for resources internal to the sub-tree (e.g., a worker-coordinated counter for "next chunk to process within S1").
- Use `run_lease_acquire(key, ttl)` (run-global, separate primitive) for resources that span sub-trees (e.g., a path on the operator's filesystem that any sub-tree might write to).
- When in doubt: prefer `run_lease_acquire`. Over-serializing is safer than silent cross-tree collision.

### Approval routing

- All approvals route to the operator via TUI. Root lead is not an approval authority.
- Set `[[approval_policy]]` rules in the manifest to auto-approve/auto-reject categories of approvals before they reach the operator. The matcher is deterministic — never evaluated by an LLM.

### Kill-with-reason

`cancel_worker(target, reason)` — when invoked with a reason, the killed actor's direct parent lead receives a synthetic reprompt with the reason text. Use this to correct a misbehaving sub-tree without a separate reprompt round-trip.

### Waiting on sub-leads

Use `wait_actor(sublead_id)` to block until a sub-lead settles.
Returns `ActorTerminalRecord` (a `Sublead(SubleadTerminalRecord)` variant).
`wait_for_worker` only accepts worker ids — call `wait_actor` for sub-leads.

### Cancel cascade

Cancellation is depth-first. Root cancel → sub-leads → their workers, with the existing two-phase drain at each layer.

---

## Writing manifests from natural-language requests

When a human asks you (the agent) to "run claude on each X in Y and
combine the results", the canonical translation is:

1. Can I enumerate the Xs up front? → flat mode, one `[[task]]` per X.
2. Do I need to compute the list of Xs first? → hierarchical mode, lead
   does the enumeration then spawns workers.
3. Is the user ok with a worst-case budget? → put it in `budget_usd`.
4. Does the work need git worktree isolation, or is it read-only? → set
   `use_worktree` accordingly.
5. What model? Default to Haiku unless the work is substantial (deep code
   analysis, multi-file refactor proposals) — then Sonnet.

Write the manifest, run `pitboss validate`, show the human the manifest
and the validation result, ask for confirmation, then dispatch. If you
dispatch first and have to ask follow-ups, you've probably wasted budget.

---

## Version

The current version is declared in the frontmatter at the top of this
file (`pitboss_version`). Schema may evolve; `pitboss validate` is the
source of truth. This document should stay self-contained — if something
here conflicts with the actual binary, the binary wins. File a PR.
