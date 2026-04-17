# Pitboss Roadmap

Capture of deferred work. Items here are scoped but unscheduled — grab
one when you're ready, or file issues to formalize priority.

## Near-term (focused, shippable in ≤1 day)

### TUI kill: cancel a worker or a whole run from `pitboss-tui`

**Status:** designed, not built. Deferred out of v0.3.4.

The TUI is read-only today. Add two keybindings:
- `x` on a focused tile → cancel that worker (hierarchical mode only).
- `X` (or `Ctrl-K`) → cancel the whole run (equivalent to Ctrl-C in the
  dispatch terminal).

Both gated behind a confirmation modal.

**Mechanism — per-run control socket.** The dispatcher binds a second unix
socket at `$XDG_RUNTIME_DIR/pitboss/<run-id>.control.sock` (alongside the
existing MCP socket for hierarchical runs). TUI connects and sends a
line-based protocol:

```
cancel-run
cancel-worker <task_id>
```

Dispatcher receives → looks up the appropriate `CancelToken` → terminates.
Reuses the same `CancelToken` plumbing we already have for Ctrl-C,
lead-exit cleanup, and `mcp__pitboss__cancel_worker`.

**Scope estimate:** ~380 lines across the dispatcher control server, TUI
client, keybindings + modal, a `fake-control-client` test-support crate,
and one integration test. Half a day.

**Gotchas:**
- Socket lifecycle tied to run lifecycle — if the TUI opens a completed
  run, `x`/`X` should show "run finished, nothing to cancel" rather
  than error.
- `x` on the lead tile in hierarchical mode is semantically equivalent
  to `X` — probably forbid `x` on the lead and require `X` for that
  action.
- `⊘ Cancelling...` intermediate state in the TUI while the worker
  drains (up to `TERMINATE_GRACE` = 10 s).

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
