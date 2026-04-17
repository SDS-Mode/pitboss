# Pitboss Roadmap

Capture of deferred work. Items here are scoped but unscheduled — grab
one when you're ready, or file issues to formalize priority.

## In progress

### v0.4.0 — Live control plane

**Status:** design spec written, plan pending, build pending.

Expanded from the original "TUI kill" roadmap item into a broader L4
(human-in-the-loop) surface covering kill, pause, continue, reprompt,
and approve/edit. Mechanism is a per-run unix control socket. See
`docs/superpowers/specs/2026-04-17-pitboss-v0.4-live-control-design.md`
for the full spec.

---

## Deferred out of v0.4.0 (promoted from near-term to explicit deferrals)

### `pitboss attach <run-id> <task-id>` — live TTY relay escape hatch

**Status:** designed out of v0.4.0 core; layered on top as v0.4.1.

Uses `portable-pty` crate to allocate a PTY for the worker subprocess;
`pitboss attach` connects the operator's terminal stdio to that PTY for
real-time keystroke passthrough. The only L3 feature that requires more
than the `-p` + `--resume` model can give. Target: v0.4.1 once v0.4.0
is stable and operator feedback indicates concrete need.

### "Freeze" pause — true SIGSTOP/SIGCONT process freeze

**Status:** future. Concept captured 2026-04-17 during v0.4.0 brainstorm.

v0.4.0 ships pause via cancel-with-resume semantics: subprocess ends,
session id preserved, continue spawns `claude --resume <id>`. The
alternative "freeze" semantic — `SIGSTOP` the running process, keep
it memory-resident, `SIGCONT` to thaw — is precise and fast (no
subprocess re-spawn cost) but adds failure modes around:

- Anthropic's HTTP stream timing out during the freeze.
- Orphan stopped processes if pitboss itself dies while a worker is
  frozen.
- The question of whether `lead_timeout_secs` should also pause (it
  currently doesn't, per the v0.4 design decision).

Revisit when a concrete use case surfaces (probably: "pause for hours"
or "pause for operator meeting" kind of scenarios).

### Out-of-TUI notifications

**Status:** future. Pitboss shouldn't require a TUI to be attached for
operator awareness. Approval requests, run completion, budget-exceeded
events, worker failures — all candidates for outbound channels:

- **Webhooks** (HTTP POST to a configurable URL)
- **Slack / Discord** integration
- **Email** (SMTP / SES)
- **Desktop notifications** (notify-send / osascript)

Natural pairing with the `approval_policy` manifest field: a `notify`
policy that pushes the request to a notification channel and waits
for an asynchronous response. Requires designing a response mechanism
(reply link? CLI tool that posts back?).

### Structured plan editing for approval

**Status:** YAGNI unless free-form editing proves insufficient.

v0.4.0's `request_approval` takes a free-form string summary;
operator `e`-edits it back as a string. If the lead wants structured
plans (typed fields: workers_to_spawn, budget, rationale) with
typed in-TUI editing, that's a bigger design: schema, typed UI,
validation. Add if concrete pain shows up.

### Reprompt via MCP tool

**Status:** operator-only in v0.4.0. Lead can achieve similar via
`cancel_worker` + `spawn_worker` with `--resume`. Promote to a
first-class lead-facing `mcp__pitboss__reprompt_worker(task_id,
prompt)` if friction emerges.

---

## Near-term (small, shippable)

The original "TUI kill" near-term item has been absorbed into the v0.4.0
scope; this section is currently empty. Add items here as they're
identified.

---

## Medium-term — deferred from earlier versions

### Broadcast mode (`pitboss b "<prompt>"`)

Send the same prompt to every running tile. From the v0.2.1 roadmap.
Depends on interactive snap-in (deferred indefinitely — see below).
**Status:** parked. No design doc.

### Depth > 1 hierarchies

A worker can itself be a lead. Needs recursion guardrails, a nested
socket addressing scheme, and a product decision: *should this exist at
all, or is "I want a deeper tree" a code smell for a flatter
decomposition?* **Status:** parked pending need.

### Peer messaging

Workers pass results to siblings without the lead in the middle. Would
need a new MCP surface or a shared blackboard. Risk: contention /
ordering complexity for marginal benefit. **Status:** parked.

### Plan approval flow

Lead proposes a plan, human operator approves in the TUI (or via
`pitboss plan`) before workers actually dispatch. UX question:
modal-in-TUI vs. separate subcommand. **Status:** parked.

### Full end-to-end `fake-claude` integration test

Task 26 of the v0.3 plan left a placeholder. Requires `fake-claude` to
speak real MCP. Medium lift; pays off once more hierarchical features
land. **Status:** would take 1–2 days.

---

## Long-term / explicitly retired

### Interactive snap-in (keystroke passthrough)

Forwarding keystrokes from a focused TUI tile into a running claude
subprocess. Analyzed in v0.3.4 as part of "AGENTS.md week" and
**retired** — hierarchical mode is the correct abstraction for the
"operator can't watch 16 workers" problem. Keeping view-only snap-in;
not building interactive.

---

## Code-quality follow-ups (Tier 3/4 from earlier reviews)

- `TokenUsageSchema` compile-time sync check with `TokenUsage`
  (e.g., `static_assertions::assert_fields!`). Currently two structs
  can diverge silently.
- Resume + worktree cleanup interaction — `pitboss resume` loses the
  claude session if the original worktree was cleaned. Either default
  hierarchical to `worktree_cleanup = "never"` or reuse the original
  worktree path on resume.
- More Claude models in `prices.rs` as they ship.
- `cargo-dist` migration — current release workflow is manual cross-
  compile. `cargo-dist` adds installers (`curl|sh`), updater binaries,
  Homebrew formula publishing. Worth it once we add more targets.
- Dockerfile / container image.
- MCP protocol extensions (we only implement tools; resources + prompts
  would be cheap adds).
- Opt-in telemetry (aggregate run counts, token totals).
- systemd unit for running as a long-lived dispatcher.
- `cargo publish` once we want crates.io distribution.

---

## Non-goals (don't build these)

### Worker → worker MCP channel

Specifically: don't let workers spawn sub-workers or send tool calls
laterally. Depth = 1 is a design invariant, not an accidental limit.
Breaking it re-introduces contention + ordering complexity that
pitboss was built to avoid.

### Windows-native builds

Pitboss relies on unix sockets + SIGTERM for cancellation. Porting to
Windows would mean a named-pipe abstraction + different process
signaling. The current target audience runs on Linux + macOS; supporting
Windows would materially expand the surface area for zero incremental
use cases we know about. Happy to revisit if a concrete need emerges.

### Heavy dependency-manager features

Pitboss is a dispatcher, not a workflow engine. Don't add DAG
dependencies between tasks, conditional branches, retry policies, etc.
If you want those, you want a proper workflow system (Airflow,
Prefect). Pitboss's value is that it stays simple.
