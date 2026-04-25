# Pitboss Roadmap

Capture of deferred work. Items here are scoped but unscheduled — grab
one when you're ready, or file issues to formalize priority.

**Last refresh: 2026-04-25 (post-v0.8.0 session).** Everything shipped through
v0.8.0 has been removed from this file — check `CHANGELOG.md` for
per-version history. If you're about to add an item, slot it into one
of the tiered sections below (biggest effort first).

---

## Major features

### Dispatcher worker pattern (nested dispatches)

A worker (or sub-lead) marked `type = "dispatcher"` invokes a child
`pitboss dispatch` against its own manifest as a self-contained unit
of work. Different from the existing depth-2 sub-lead model: the child
run has its own budget envelope, KV namespace, run lifecycle, and
`summary.json` — the parent doesn't manage internals, only the
outcome.

Proposed manifest fragment:

```toml
[[task]]
id                 = "build-db-tier"
type               = "dispatcher"        # relaxes sandbox, adds pitboss to PATH
manifest           = "builds/db-tier.toml"
timeout_secs       = 3600
completion_marker  = "db-tier.done"      # file carries child run ID
```

Required pitboss changes:

1. `type = "dispatcher"` worker role that relaxes the run-dir sandbox
   specifically for designated dispatcher actors and adds the pitboss
   binary to PATH.
2. `completion_marker` task field — dispatcher waits on the file
   (which carries the child run ID, not a boolean) before settling
   the parent task. Absence at `timeout_secs` → failure surfaced up
   the tree.
3. Helper plumbing for the parent to locate and parse the child's
   `summary.json` so failures bubble rather than time out blindly.

Child spend is **not** auto-rolled into the parent budget. Instead
each child manifest's `summary.json` accumulates as a data point for
a cost oracle the operator can query before dispatch:

```
builds/windows-server-2019/base.toml      → $1.42 avg
builds/postgres-16/standard.toml          → $0.93 avg
```

A planner lead consults historical summaries to give pre-flight cost
estimates from real data, not guesses.

**Why it's a distinct concept from sub-leads:** sub-leads exist within
the root lead's budget and KV namespace — useful for breaking up one
logical job. A nested dispatch is a named, versioned, reusable unit
of work that any actor with dispatcher privileges can fire. Primary
use case is infra buildouts with well-defined sub-problems (db tier,
app tier, reverse proxy) where each sub-problem benefits from being
independently auditable.

**Status:** not feasible today — workers are sandbox-locked to the
run directory and cannot exec the pitboss binary. Requires the
`dispatcher` role + completion-marker plumbing as a coordinated
feature add.

### Web operational console

The TUI is fine for a single local run but does not compose: it
cannot be embedded in a dashboard, queried programmatically, shared
across operators, or run headlessly. It is also the first thing that
breaks in backgrounded dispatch. Run artifacts are already structured
JSON (`summary.json`, `summary.jsonl`, per-task logs) — a read-only
server watching `~/.local/share/pitboss/runs/` covers most of the
operational need with no database. The filesystem is the database
initially.

Build order by value:

1. **Post-run archaeology view.** Buildable today against existing
   artifacts, zero pitboss changes. Renders timeline, spend breakdown,
   task tree, long-pole identification from a root run ID.
2. **Pre-flight tree** — the same `pitboss tree` rendered in the
   browser, with manifest tree visualization, budget envelopes, and
   estimated cost.
3. **Live run graph** — real-time tree with node states and live
   spend. Requires polling or a websocket feed from the dispatcher;
   only becomes essential once nested dispatches are a real feature.

The TUI stays for quick local watches; the web console is the
operational picture at scale. Pairs naturally with the dispatcher-
worker pattern above — once nested dispatches are real, the live
graph is how an operator follows a multi-tier infra build without
camping a terminal.

**Status:** scoped, unstarted. Step 1 is the highest-leverage
single-PR slice and could ship independent of any other roadmap
item.

---

## Ops / infra polish

- **MCP protocol extensions.** We only implement `tools`. Adding
  `resources` + `prompts` would be cheap and makes pitboss a more
  complete MCP citizen — useful if we ever expose it to non-claude
  MCP clients.
- **Opt-in telemetry.** Aggregate run counts + token totals (no
  prompt content). Default off; explicit config key to enable.
- **systemd unit.** For long-lived dispatcher mode (`pitboss
  dispatch` as a service rather than a one-shot).
- **`cargo publish` to crates.io.** Once we want third-party
  library consumers.
- **`pitboss prune` subcommand.** Scan the runs dir for orphans —
  entries with no `summary.json` AND no responsive control socket —
  and either mark them `Cancelled` (synthesize a minimal summary.json
  reflecting the partial state in `summary.jsonl`) or remove them
  outright with `--remove`. Today, any dispatch killed before clean
  finalize (`kill -KILL`, OOM, segfault) leaves an orphan that
  `pitboss-tui list` misreports as `running` indefinitely. Manual
  cleanup is `rm -rf ~/.local/share/pitboss/runs/<id>` plus
  `rm /run/user/$(id -u)/pitboss/<id>-*.sock` — fine for an operator
  who knows the layout, but a built-in `prune` makes the lifecycle
  legible. Defaults to dry-run; `--apply` for the destructive path;
  `--older-than 24h` for time-windowed sweeps.
- **TUI run-list staleness detection.** The run-list status column
  derives "running" from "summary.json missing + control socket
  liveness probe doesn't fail." A stale unix socket file can stick
  around without a live binder, so dead runs occasionally show as
  `running` until cleaned up by hand. Add a third state — `Stale` —
  when (a) the last `summary.jsonl` line is more than N hours old
  (default 4h), AND (b) the socket either won't accept connections or
  the bound process no longer exists. Pairs naturally with `pitboss
  prune` above: `Stale` is what `prune` matches on by default.
- **`pitboss tree <manifest>` pre-flight subcommand.** Static TOML
  walk that prints the dispatch tree with per-actor budget envelopes
  and an aggregated total. `--check <USD>` mode acts as a dispatch
  gate — refuses to proceed if aggregate budget exceeds the
  threshold or a referenced child manifest is missing. Useful as a
  CI gate and a cost preview before firing $20+ runs. Buildable
  today against the existing manifest parser; precursor to the live
  run graph in the web console section above.
- **`pitboss scaffold` / `pitboss init` template generator.** Emit
  a valid TOML skeleton with commented placeholders to stdout or a
  named file. Two tiers: `simple` (coordinator + flat worker pool,
  2 levels) and `full` (coordinator + sub-leads + workers + seam
  worker + KV refs + budget fields, optional sections commented).
  Agents are reliably better at filling in than constructing from
  scratch — a structurally correct skeleton removes most
  first-attempt parse failures and lets the agent focus on prompt
  content. Defaults to `simple`; agents reach for `full` only when
  a task genuinely requires a seam worker or nested sub-leads.
- **`pitboss validate` — detect promptless lead.** A `[lead]` /
  `[[lead]]` block whose `prompt` key sits *after* a subtable
  declaration (e.g. `[lead.sublead_defaults]`) parses as
  syntactically valid TOML but assigns `prompt` outside the lead's
  scope. The lead spawns, gets no `-p` argument, and exits with
  code 1 in ~700ms. `pitboss validate` reports `OK` because the
  schema is satisfied — only `prompt` is silently empty. Fix:
  validate should fail when a resolved lead/sub-lead config has a
  missing or empty `prompt`, since a promptless actor cannot run.
  Cheap, single-day change.
- **AGENTS.md `[[lead]]` vs `[lead]` consistency.** The manifest
  schema section uses `[[lead]]` while the depth-2 example uses
  `[lead]`. The depth-2 fields (`allow_subleads`, `max_subleads`,
  `max_workers_across_tree`, `[lead.sublead_defaults]`) reject under
  `[[lead]]` with `unknown field`. Pick one syntax for hierarchical
  manifests and propagate; flag the other as deprecated or as
  flat-mode-only.

---

## Safety / defense in depth

### Typed worker/sub-lead profiles with dispatcher-enforced tool caps

Today the lead's prompt is the sole guard against an off-policy spawn.
When the lead calls `spawn_worker(tools=[...], model=...)`, pitboss
honours whatever it sends. A drift in the lead's prompt template, a
copy-pasted worker snippet, or a hostile actor earlier in the chain
can silently widen the worker's capability surface without the
operator's knowledge.

Add declarative actor profiles that the dispatcher enforces at
spawn-arg validation time — a belt-and-suspenders allowlist the lead
cannot override:

```toml
[[worker_type]]
id            = "extraction"
tools         = ["Read", "Glob", "Grep"]            # allowlist
allowed_models = ["claude-haiku-4-5", "claude-sonnet-4-6"]
max_timeout_secs = 900

[[worker_type]]
id    = "writer"
tools = ["Read", "Glob", "Grep", "Write"]

[[sublead_type]]
id            = "planner"
tools         = ["Read", "Glob", "Grep"]
allowed_models = ["claude-opus-4-7"]
max_budget_usd = 2.00
```

`spawn_worker` / `spawn_sublead` gain an optional `type: "<id>"` arg.
When supplied (or required by the manifest — see below), the
dispatcher:

1. Looks up the type's profile. Unknown id → reject the spawn.
2. Validates the lead's `tools` arg is a **subset** of the profile's
   allowlist. Any tool not in the allowlist → reject.
3. Validates `model` is in `allowed_models` (when set). Unlisted → reject.
4. Clamps `timeout_secs` down to `max_timeout_secs` (when set);
   doesn't round up if the lead's value is smaller.
5. (Sub-leads) Clamps `budget_usd` down to `max_budget_usd`.

If `tools` is omitted by the lead, the spawn gets the profile's full
allowlist — not the lead's tools. A manifest-level `require_actor_type`
flag (default false for back-compat) forces every `spawn_worker` /
`spawn_sublead` to name a type, rejecting legacy type-less spawns.

**Why this belongs as a manifest concept, not a prompt constraint:**

- The operator (not the lead) sets the capability surface. Prompts are
  tuned iteratively; manifests are code-review artifacts.
- Works with `[lead.sublead_defaults]` — profiles are a sibling to
  defaults, not a replacement.
- Auditable: `pitboss validate` can report the full capability matrix
  of a manifest without running it. `pitboss status` could show which
  type each live worker was spawned under.
- Composable: a team could ship a "safe" base manifest with restrictive
  types and let downstream manifests redeclare or extend.

**Scope for initial implementation:**

- [ ] `[[worker_type]]` and `[[sublead_type]]` parsing + validation
- [ ] `type` arg on `spawn_worker` / `spawn_sublead` MCP tools
- [ ] Dispatcher-side clipping + rejection with clear error messages
- [ ] `pitboss validate` surfaces unknown types referenced anywhere
- [ ] Record resolved type on `TaskRecord` so `summary.json` shows it
- [ ] Docs: AGENTS.md section + pitboss.example.toml commented block

**Deferred to follow-up:**

- Env var allowlists per type (e.g. "type `db-writer` may read
  `DATABASE_URL` but not `AWS_*`")
- Per-type working-directory constraints
- `[[task_type]]` for flat mode (the same capability surface, but flat
  mode's use cases are narrower — defer until a concrete ask)
- TUI support: surface the type label in tile headers + approvals pane

**Status:** scoped, not started. Good candidate for v0.9.

### Lead-side budget accounting and cap

Today `[run].budget_usd` reserves and accounts only for `spawn_worker`
estimates — it does **not** count the lead's own token spend. A lead
that does heavy synthesis or thrashes through orchestration retries can
burn arbitrary amounts of compute without ever tripping the budget cap.

Concrete failure mode (observed in the docs-vault-update v1 dispatch
run, 2026-04-24): an Opus lead orchestrating 16 workers across 54
minutes consumed ~$15 in lead tokens alone, against a `budget_usd =
5.00` declaration. Workers reserved + spent within budget; the lead's
own spend was untracked and unbounded.

Two changes to make budget actually predictive of total cost:

1. **Track lead token consumption against `spent_usd`.** Pitboss
   already prices each model. The lead's own `Event::Result` reports
   its token usage on every turn — feed that into the same
   `spent_usd.lock().await` mutex that workers do. The lead's parent
   spawn site (`run_dispatch`) already has the model rate; add a
   reconciliation pass per assistant turn or at lead termination.
2. **Optional `lead_budget_usd` separate cap.** When set, the lead's
   spend is capped independently — useful for runs that want generous
   worker budgets but a tight orchestration budget (the inverse case
   shows up too — large per-worker budgets for code generation, small
   lead budget for a fixed-shape orchestration plan).

Sub-leads pose a third wrinkle: their own token spend should count
toward the run's total. Same mechanism — sub-lead reconciliation at
termination.

**Why this matters beyond cost predictability:** the budget cap is
also a kill switch for runaway lead behavior. If a lead enters a
retry loop or pathological orchestration pattern, today there is no
financial backstop other than `lead_timeout_secs`. A budget cap that
includes lead spend would terminate the run when total cost exceeds
the declared envelope — a more ergonomic safety than time-based.

**Scope:** track lead spend by default (+ sub-lead spend); add
optional `lead_budget_usd` for the separate-cap case. Surface running
lead-spend total via `worker_status` / `pitboss status` output so
operators can observe the eat-rate live.

**Status:** scoped, not started. Sibling to typed worker profiles
above; both are operator-declared bounds the dispatcher enforces.

### External MCP server injection in manifests

Pitboss generates per-actor `--mcp-config` files containing only the
pitboss bridge server. Claude Code's `--mcp-config` flag is not
additive with project-level `.mcp.json` auto-discovery, so any
project-local MCP server placed in the working directory is
silently absent inside lead, sub-lead, and worker actors. The
manifest schema has no field to declare additional MCP servers
either.

Confirmed via live test on 2026-04-24 (run 019dc06a): context7
MCP tools (`resolve-library-id`, `query-docs`) were absent at every
actor level despite a valid `.mcp.json` in the working directory.
Workers are additionally sandbox-locked to the run directory, so a
helper script wrapping the MCP call is not reachable either.

Add a manifest field that declares MCP servers to merge into the
generated `--mcp-config`:

```toml
[[mcp_server]]
id      = "context7"
command = "npx"
args    = ["-y", "@upstash/context7-mcp"]
# optional: scope = "lead" | "sublead" | "worker" | "all"
```

Open scope decisions:

- **Run-wide vs per-actor.** Initial cut: run-wide with optional
  per-actor override (`scope = "lead"` to limit context7 to the
  lead, for example). Per-actor is more secure but more verbose;
  run-wide is the right default for the common "give all actors
  context7 docs access" case.
- **Allowlist interaction with typed profiles.** A `worker_type`
  profile should be able to declare which MCP servers its workers
  see — keeps the safety belt even when context7 is in scope
  run-wide.

Today's workaround is documented in `Pitboss.md` (Larry notes,
2026-04-24): the lead pre-fetches docs via a bash helper running
outside the worker sandbox, writes results to `/ref/context7/<lib>`
in KV, and workers read from KV. Functional but operator-heavy.
Native injection makes context7-style augmentation a one-line
manifest declaration.

**Status:** scoped, not started. Pairs with typed worker profiles
above for the per-type allowlist piece.

---

## Medium-term — deferred, lower priority

### Broadcast mode (`pitboss b "<prompt>"`)

Send the same prompt to every running tile. From the v0.2.1 roadmap.
Depends on interactive snap-in (retired — see below). **Status:**
parked; probably needs a different UX surface now.

### Depth > 2 hierarchies

Depth-2 (root lead → sub-leads → workers) shipped in v0.6.0. Depth-3+
(sub-leads spawning their own sub-leads) remains a non-goal for now —
the same "flatter decomposition" concern applies with more force. The
depth=2 cap is enforced at both the MCP handler and the sub-lead's
`--allowedTools` list. **Status:** depth=2 SHIPPED (v0.6.0); depth>2
parked pending a concrete need that can't be served by a wider flat
fan-out.

### Peer messaging

Workers pass results to siblings without the lead in the middle.
Considered and implemented differently in v0.4.2 — the worker shared
store (`/shared/*` namespace) covers the most common case via a hub
model. Lateral MCP calls between workers remain a non-goal (see
below). **Status:** partially subsumed by shared store; MCP-channel
form explicitly retired.

---

## Deferred from v0.8.0 (targeting v0.9+)

Items that were scoped or considered during v0.8 development but
explicitly deferred. These are reasonably well-understood problems,
not blue-sky ideas.

### Path B stabilization (#92, #93, #94)

v0.8 added the `permission_routing = "path_b"` manifest field but
gates it with a validation error. Three tracked bugs block stabilization:

- **#92:** `--permission-prompt-tool` flag isn't threaded to all
  spawn-args call sites (root lead vs. sub-lead vs. worker).
- **#93:** The `PermissionPromptResponse` wire format doesn't match
  what claude's SDK expects.
- **#94:** `CLAUDE_CODE_ENTRYPOINT=sdk-ts` must be evicted from the
  environment when Path B is active (otherwise Path A takes over).

**Status:** work-in-progress on `feat/path-b-permission-routing`.
Landing all three unblocks removal of the validate-time gate.

### Phase 4 per-sub-tree runners (#100)

The watcher cascades cancel tokens directly across sub-tree boundaries
rather than going through a per-sub-tree runner that owns its workers'
cancellation. Watchers are fire-once, so workers registered after the
cascade fires can be orphaned. Also: `terminate()` does not cascade to
sub-tree workers (only `drain()` does).

**Status:** tracked in #100 with a detailed architecture memo. Tactical
mitigation (post-register cascade check at worker-registration time) was
already applied (#99, closed).

### TUI approval replay on run-switch (#95)

When the TUI is launched without a run-id and the operator picks a run
from the selector, pending approval_requested events already in the
queue are not re-displayed. Launching with a run-id prefix works.

**Status:** partially mitigated — `ctrl_events_rx` is now drained on
`SwitchRun` so stale events from the prior run no longer leak (#104,
closed). Full replay of pending approvals on re-connect remains open.

### Slack notification sink escaping

v0.7 hardened the Discord sink (escape markdown + `allowed_mentions: []`)
but the Slack sink wasn't audited for the same class of injection.
If Slack sink formats untrusted fields into `mrkdwn` blocks, same
treatment applies. **Status:** deferred; audit + fix in one small PR
when prioritized.

### Low-severity nits from v0.8 ultrareview

- ~~**#96:** `pitboss status` table overflows the task-ID column.~~ **Closed** — `truncate_ellipsis` added to `pitboss-core::fmt`, applied in `status.rs` and `tui_table.rs`.
- ~~**#97:** Duration formatter has no hour rollover; four duplicate copies.~~ **Closed** — centralized into `pitboss_core::fmt::format_duration_ms` with hour rollover; duplicates removed.
- ~~**#98:** Sync `std::fs` write inside `tokio::spawn` in `sublead.rs`.~~ **Closed** — switched to `tokio::fs::OpenOptions` + `AsyncWriteExt`.

---

## Non-goals (don't build these)

### Worker → worker MCP channel

Specifically: don't let workers spawn sub-workers or send tool calls
laterally. Workers are terminal nodes — no spawning, no lateral calls.
This is a design constraint, not an accidental limit. Breaking it
re-introduces the contention + ordering complexity that pitboss was
built to avoid. (Note: v0.6.0 added depth-2 via *sub-leads*, which are
a distinct concept — a sub-lead is a full orchestrator, not a worker
calling peers.) If you need workers to share data, use the shared store
(`/shared/*` or `/peer/<id>/*` namespaces — see `AGENTS.md`).

### Interactive snap-in (keystroke passthrough)

Forwarding keystrokes from a focused TUI tile into a running claude
subprocess. Analyzed in v0.3.4 and **retired** — hierarchical mode is
the correct abstraction for the "operator can't watch 16 workers"
problem. The `pitboss attach` escape hatch (above) covers the narrow
cases where view-only isn't enough. View-only Detail view stays.

### Windows-native builds

Pitboss relies on unix sockets + SIGTERM for cancellation. Porting to
Windows would mean a named-pipe abstraction + different process
signaling. The current target audience runs on Linux + macOS;
supporting Windows would materially expand the surface area for zero
incremental use cases we know about. Revisit if a concrete need
emerges.

### Heavy dependency-manager features

Pitboss is a dispatcher, not a workflow engine. Don't add DAG
dependencies between tasks, conditional branches, retry policies, etc.
If you want those, you want a proper workflow system (Airflow,
Prefect). Pitboss's value is that it stays simple.
