# Pitboss Roadmap

Capture of deferred work. Items here are scoped but unscheduled — grab
one when you're ready, or file issues to formalize priority.

**Last refresh: v0.6.0 (2026-04-19).** Everything shipped through
v0.6.0 has been removed from this file — check `CHANGELOG.md` for
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

## Deferred from v0.6.0 (targeting v0.7+)

Items that were scoped or considered during v0.6 development but
explicitly deferred. These are reasonably well-understood problems,
not blue-sky ideas.

### Per-sub-tree runners (Phase 4 ownership cleanup)

In v0.6.0, the watcher cascades cancel tokens directly across sub-tree
boundaries rather than going through a per-sub-tree runner that owns
its workers' cancellation. This inverts the ownership model the v0.6
spec noted for future cleanup. Current implementation works correctly
but the ownership inversion is a known footgun for handlers that need
to route by caller. **Status:** deferred for v0.7; tracked in codebase
with CAUTION doc block on `DispatchState`'s `Deref` impl.

### TUI runtime policy mutation

`[[approval_policy]]` rules are manifest-only in v0.6 — the TUI can
display and act on policy but cannot edit rules while a run is live.
Useful for operators who want to tighten or relax auto-approve rules
mid-run without restarting. **Status:** deferred; manifest-only is
safe and sufficient for the v0.6 use cases.

### Sub-lead resume support

`pitboss resume <run-id>` only resumes the root lead. Sub-leads that
were live when the run was interrupted are not individually resumed;
the root lead must re-spawn them. Full sub-lead resume would require
persisting sub-lead `sublead_id` → `session_id` mappings and threading
`--resume` through `spawn_sublead_session`. **Status:** deferred; the
current fallback (root lead re-spawns) is workable for the typical
interruption+retry case.

### Native-arm container runners

The current multi-arch container build uses QEMU emulation for
`linux/arm64`, which is slow. Splitting the matrix to use native arm64
runners (GitHub's `ubuntu-24.04-arm` or self-hosted) would cut build
time significantly. **Status:** deferred; blocked on runner availability
and cost in the public CI budget; the QEMU build is correct if slow.

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
