# Pitboss Roadmap

Capture of deferred work. Items here are scoped but unscheduled — grab
one when you're ready, or file issues to formalize priority.

**Last refresh: v0.8.0 (2026-04-24).** Everything shipped through
v0.8.0 has been removed from this file — check `CHANGELOG.md` for
per-version history. If you're about to add an item, slot it into one
of the tiered sections below (biggest effort first).

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
mitigation (post-register cascade check at worker-registration time)
lands separately as #99.

### TUI approval replay on run-switch (#95)

When the TUI is launched without a run-id and the operator picks a run
from the selector, pending approval_requested events already in the
queue are not re-displayed. Launching with a run-id prefix works.

**Status:** diagnosed as stale `approval_list` state not being cleared
in the `SwitchRun` handler.

### Slack notification sink escaping

v0.7 hardened the Discord sink (escape markdown + `allowed_mentions: []`)
but the Slack sink wasn't audited for the same class of injection.
If Slack sink formats untrusted fields into `mrkdwn` blocks, same
treatment applies. **Status:** deferred; audit + fix in one small PR
when prioritized.

### Low-severity nits from v0.8 ultrareview

- **#96:** `pitboss status` table overflows the task-ID column for
  sub-lead IDs (hard-coded `{:<30}` pad, no truncation).
- **#97:** Duration formatter has no hour rollover — 2h run shows as
  `"120m00s"`. Four duplicated copies (`status.rs`, `tui_table.rs`,
  `diff.rs`, `tui.rs`) — centralize into one helper.
- **#98:** Sync `std::fs` write inside a `tokio::spawn` async task in
  `sublead.rs` — switch to `tokio::fs` or `spawn_blocking`.

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
