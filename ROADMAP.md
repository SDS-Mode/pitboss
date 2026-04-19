# Pitboss Roadmap

Capture of deferred work. Items here are scoped but unscheduled — grab
one when you're ready, or file issues to formalize priority.

**Last refresh: v0.4.5 (2026-04-19).** Everything shipped through
v0.4.5 has been removed from this file — check `CHANGELOG.md` for
per-version history. If you're about to add an item, slot it into one
of the tiered sections below (biggest effort first).

---

## Flagship feature bucket (targeting v0.5.0)

_Empty — all v0.5.0 flagship items have shipped. v0.5.0 release can
cut whenever we're ready. Post-v0.5.0 items live in the tiered
sections below._

---

## Ops / infra polish

- **`cargo-dist` migration.** Current release workflow is a manual
  cross-compile matrix. `cargo-dist` adds installers (`curl | sh`),
  auto-updater binaries, and Homebrew formula publishing. Worth the
  switch once we add more target triples.
- **Dockerfile / container image.** Deployment convenience + a
  canonical runtime environment for CI reproduction.
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

### Depth > 1 hierarchies

A worker can itself be a lead. Needs recursion guardrails, a nested
socket addressing scheme, and a real product decision: *should this
exist at all, or is "I want a deeper tree" a code smell for a flatter
decomposition?* **Status:** parked pending need.

### Peer messaging

Workers pass results to siblings without the lead in the middle.
Considered and implemented differently in v0.4.2 — the worker shared
store (`/shared/*` namespace) covers the most common case via a hub
model. Lateral MCP calls between workers remain a non-goal (see
below). **Status:** partially subsumed by shared store; MCP-channel
form explicitly retired.

---

## Non-goals (don't build these)

### Worker → worker MCP channel

Specifically: don't let workers spawn sub-workers or send tool calls
laterally. Depth = 1 is a design invariant, not an accidental limit.
Breaking it re-introduces the contention + ordering complexity that
pitboss was built to avoid. If you need workers to share data, use
the shared store (`/shared/*` or `/peer/<id>/*` namespaces — see
`AGENTS.md`).

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
