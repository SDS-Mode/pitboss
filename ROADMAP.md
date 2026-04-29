# Pitboss Roadmap

A directory into ongoing work. Each entry is a **3-line abstract +
status + tracking link** — long-form design lives in the linked
GitHub issue, not here.

## How to use this file

- **Closed items live in `CHANGELOG.md`, not here.** When a roadmap
  item ships, delete it from this file. Don't strikethrough, don't
  leave a "Closed —" marker. Git history and the changelog carry that
  load.
- **One entry per item.** If it fits on a screen, keep it inline; if
  the design needs more than ~10 lines, file a tracking issue
  (label: `roadmap`) and leave a 3-line abstract here pointing at it.
- **Status vocabulary** (pick one): `scoped` / `in-progress` /
  `blocked: <what>` / `parked: <why>` / `non-goal`.
- **Tracking line** (always present): `**Tracking:** #NNN` or
  `**Tracking:** none yet — file before grabbing`.
- **No version targeting.** Use GitHub Milestones for release-train
  scheduling. Strategic horizons can stay inline as `parked:` notes.

Last refreshed: v0.9.1 (2026-04-29).

---

## Major initiatives

Multi-PR features in active shaping. Each one is large enough to
warrant its own tracking issue with the design discussion.

### Nested-dispatch worker type

A worker marked `type = "dispatcher"` invokes a child `pitboss
dispatch` against its own manifest as a self-contained unit of work,
with its own budget envelope, KV namespace, and `summary.json`.
Pairs with a cost oracle so a planner lead can pre-flight-estimate
total spend from real historical data.

**Status:** scoped — depends on sandbox relaxation + `completion_marker` plumbing.
**Tracking:** #248.

### Web Insights Slice B — manifest detail + per-task drill

Per-manifest landing page (`/manifests/<name>`) with run history,
KPI strip, trend sparklines, plus a side-panel drill from any task
row to that task ID's history across the manifest's runs. Reuses
the Slice A aggregator with no new backend storage.

**Status:** scoped.
**Tracking:** #249.

### Web Insights Slice C — run comparison + standalone Gantt

Side-by-side compare of two runs of the same manifest (Gantt diff,
per-task delta, manifest TOML diff). Extracts the per-run task-tree
drawing into a reusable `gantt.svelte` component consumed by both
the existing single-run view and the new compare route.

**Status:** scoped.
**Tracking:** #250.

### Typed worker/sublead profiles

Operator-declared `[[worker_type]]` / `[[sublead_type]]` profiles
with tool allowlists, model allowlists, and timeout/budget caps the
dispatcher enforces at spawn-arg validation time — a belt-and-
suspenders against prompt drift. Folds in per-actor MCP server
scoping as a sub-task.

**Status:** scoped — high-impact safety primitive.
**Tracking:** #252.

### Lead+sublead budget accounting

Today `[run].budget_usd` accounts only for `spawn_worker` spend; the
lead's own tokens are unbounded. Reconcile lead and sublead spend
into the same `spent_usd` mutex, add an optional `lead_budget_usd`
separate cap, and surface running lead-spend live in
`worker_status`. Backstop for runaway-lead failure modes.

**Status:** scoped — sibling to #252.
**Tracking:** #253.

### Non-Anthropic model support (v1.0 destination)

Make pitboss model-agnostic: provider-tagged spawner trait, pricing
registry indexed by `(provider, model)`, per-provider failure
classifiers, tool-use translation. Strategic — explicitly out of
scope for v0.x but flagged so v0.x design choices don't foreclose
the option.

**Status:** parked: v1.0 horizon, no concrete blocking demand today.
**Tracking:** #251.

---

## Scoped & ready

One- or two-PR items, well-understood enough that the inline
abstract is the spec.

- **`pitboss schema --format=n8n-form` export.** Emit the manifest
  field set as an n8n-style form-field JSON descriptor (`fieldLabel`,
  `fieldType`, `requiredField`, `defaultValue`, `fieldOptions.values`)
  so a form-builder UI can drive operator manifest entry without
  re-implementing field knowledge. The CLI hint already exists in
  `pitboss schema --help`. **Status:** scoped. **Tracking:** none yet.

- **MCP protocol extensions: `resources` + `prompts`.** We only
  implement `tools` today. Adding the other two surfaces makes
  pitboss a more complete MCP citizen — useful when exposing it to
  non-claude MCP clients. **Status:** scoped. **Tracking:** none yet.

- **`pitboss scaffold` / `pitboss init` template generator.** Emit a
  valid TOML skeleton with commented placeholders. Two tiers:
  `simple` (coordinator + flat worker pool) and `full` (coordinator
  + sub-leads + workers + seam worker + KV refs + budget fields).
  Agents are reliably better at filling in than constructing from
  scratch. **Status:** scoped. **Tracking:** none yet.

- **TUI run-list staleness detection.** Add a `Stale` state when the
  last `summary.jsonl` line is more than N hours old (default 4h)
  AND the socket either won't accept connections or the bound
  process no longer exists. Pairs with `pitboss prune` (already
  shipped) — `Stale` is what `prune` matches on by default.
  **Status:** scoped. **Tracking:** none yet.

- **systemd unit for long-lived dispatcher mode.** Run `pitboss
  dispatch` as a service rather than a one-shot, with restart
  semantics and journaled logs. **Status:** scoped. **Tracking:**
  none yet.

- **Opt-in telemetry.** Aggregate run counts + token totals (no
  prompt content). Default off; explicit config key to enable.
  **Status:** scoped. **Tracking:** none yet.

- **`cargo publish` to crates.io.** Once we want third-party library
  consumers of `pitboss-core` / `pitboss-schema`. **Status:** parked:
  no third-party consumer demand today. **Tracking:** none yet.

---

## Deferred

Items with an explicit blocker — usually waiting on something
upstream.

- **Path B `permission_routing` stabilization.** The implementation
  PRs (#92, #93, #94) merged, but `validate.rs` still rejects
  `permission_routing = "path_b"` as "not yet stable". Soak in
  staging, then remove the validate gate. **Status:** blocked: needs
  real-world soak before gate removal. **Tracking:** none yet (file
  when ready to remove the gate).

- **Broadcast mode (`pitboss b "<prompt>"`).** Send the same prompt
  to every running tile. Parked because the original UX (interactive
  snap-in) was retired in v0.3.4 — needs a different surface now.
  **Status:** parked: needs UX redesign post-snap-in retirement.
  **Tracking:** none yet.

- **Depth > 2 hierarchies.** Sub-leads spawning their own sub-leads.
  Depth=2 cap is enforced at both the MCP handler and the sub-lead's
  `--allowedTools` list. **Status:** parked: no concrete need that
  can't be served by a wider flat fan-out. **Tracking:** none yet.

- **Full TUI approval-replay on run-switch (#95).** Drain of stale
  events on `SwitchRun` shipped (#104); replay of pending approvals
  on re-connect remains. **Status:** scoped. **Tracking:** #95.

---

## Non-goals

Explicit "don't build these" — design constraints, not accidental
limits.

- **Worker → worker MCP channel.** Workers are terminal nodes. No
  spawning, no lateral calls. Breaking this re-introduces the
  contention + ordering complexity pitboss was built to avoid. Use
  the shared store (`/shared/*`, `/peer/<id>/*`) for worker-to-worker
  data. Depth-2 via *sub-leads* is a distinct concept — a sub-lead
  is a full orchestrator, not a worker calling peers.

- **Interactive snap-in (keystroke passthrough).** Forwarding
  keystrokes from a focused TUI tile into a running claude
  subprocess. Analyzed in v0.3.4 and retired — hierarchical mode is
  the correct abstraction for the "operator can't watch 16 workers"
  problem. View-only Detail view stays.

- **Windows-native builds.** Pitboss relies on unix sockets +
  SIGTERM. Porting would mean a named-pipe abstraction + different
  process signaling for zero incremental use case we know about.
  Revisit if a concrete need emerges.

- **Heavy workflow-engine features.** No DAG dependencies between
  tasks, no conditional branches, no retry policies. If you want
  those, you want a proper workflow system (Airflow, Prefect).
  Pitboss's value is that it stays simple.

- **Peer-messaging via lateral MCP calls.** Considered in v0.4.2 and
  resolved differently — the worker shared store covers the common
  case via a hub model. The MCP-channel form is explicitly retired.
