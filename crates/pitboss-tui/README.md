# Pitboss TUI

Terminal floor-view for Pitboss runs. Tile grid + live log tail + live
control plane (cancel / freeze / continue / reprompt / approve / reject)
+ a non-modal approval list pane.

**Current version:** v0.6.0. For depth-2 sub-lead runs, tiles are
grouped into expandable sub-tree containers.

## Quick start

```
cargo build --release -p pitboss-tui
./target/release/pitboss-tui          # open most recent run
./target/release/pitboss-tui list     # print table of runs, exit
./target/release/pitboss-tui 019d99   # open specific run by id prefix
./target/release/pitboss-tui --help
```

Non-interactive follow of a single worker (no TUI):

```
pitboss attach <run-id> <task-id>         # formatted tail
pitboss attach <run-id> <task-id> --raw   # raw stream-json
```

## Layout

### Flat and v0.4.x hierarchical runs

```
┌─ Pitboss — run 019d9946-4a98 ───────────────── 3 tasks, 1 failed ────┐
│                                                                       │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐              │
│  │ auth     │  │ lint     │  │ test     │  │ build    │              │
│  │ ✓ Done   │  │ ● Run    │  │ … Pend   │  │ ✗ Fail   │              │
│  │ 02m15s   │  │ 00m44s   │  │ —        │  │ 00m12s   │              │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘              │
│                                                                       │
│  ── Focus: lint (running) ──────────────────────────────────────────  │
│  * Bash cargo clippy --workspace -- -D warnings                      │
│  < Checking crates/pitboss-core/src/parser/mod.rs                    │
│  > Linting workspace...                                              │
│                                                                       │
│ [hjkl] nav  [Enter] detail  [a] approvals  [x/X] kill wrk/run  …     │
└───────────────────────────────────────────────────────────────────────┘
```

### Depth-2 sub-lead runs (v0.6+)

Workers are grouped under their sub-lead's container; the root lead and
any direct workers get their own container. `Tab` cycles focus across
containers; a container header with `▾` expands, `▸` collapses.

```
┌─ Pitboss — run 019daea8-a2f8 ─── 2 subleads · 5 workers · 1 pending ─┐
│ ▾ root  (planner, running)                                            │
│   ┌──────────┐                                                        │
│   │ root     │                                                        │
│   │ ● Run    │                                                        │
│   └──────────┘                                                        │
│ ▾ root→S1  (phase-1, running, $0.42 / $2.00)                          │
│   ┌──────────┐  ┌──────────┐                                          │
│   │ w1       │  │ w2       │                                          │
│   │ ✓ Done   │  │ ● Run    │                                          │
│   └──────────┘  └──────────┘                                          │
│ ▸ root→S2  (phase-2, running, 2 workers — collapsed)                  │
│ ─ Focus: root→S1→w2 ───────────────────────────────────────────────── │
│   ...                                                                 │
└───────────────────────────────────────────────────────────────────────┘
```

Focus-log lines are colored by event type: white for assistant text
(`> `), cyan for tool use (`* `), green for tool results (`< `),
magenta for result events (`v `), yellow for rate limits (`! `), gray
for unparseable / system / unknown.

## Keybindings

### Navigation & views

| Key                | Action                                               |
|--------------------|------------------------------------------------------|
| `h j k l` / arrows | Navigate tiles                                       |
| `Tab`              | Cycle focus across sub-tree containers (v0.6+)       |
| `Enter`            | Open Detail view (metadata pane + live git-diff + scrollable log); on a container header, expand/collapse |
| `L`                | Toggle full-log overlay of focused tile              |
| `a`                | Focus the approval list pane (right-rail; v0.6+)     |
| `o`                | Run picker — switch to another run by id             |
| `?`                | Help overlay (full keybinding reference)             |
| `q` / `Ctrl-C`     | Quit                                                 |
| `Esc`              | Close any overlay / modal                            |

### Mouse

| Gesture                         | Action                               |
|---------------------------------|--------------------------------------|
| Left-click a grid tile          | Focus + open Detail                  |
| Left-click a run in the picker  | Open that run                        |
| Right-click inside Detail       | Exit back to grid                    |
| Scroll wheel inside Detail      | Scroll log 5 rows/tick               |

### Scroll cadence inside Detail

| Key                            | Rows/step                              |
|--------------------------------|----------------------------------------|
| `j` / `k` / arrows             | 1                                      |
| `J` / `K`                      | 5                                      |
| `Ctrl-D` / `Ctrl-U` / PgDn/PgUp| 10                                     |
| `g` / `G`                      | top / bottom (bottom re-enables follow)|

### Control plane

| Key | Action                                                          |
|-----|-----------------------------------------------------------------|
| `x` | Confirm + cancel focused worker                                 |
| `X` | Confirm + cancel entire run (cascades SIGTERM to every worker)  |
| `p` | Pause focused worker (cancel-mode, preserves `claude_session_id`) |
| `c` | Continue paused/frozen worker (`claude --resume` or SIGCONT)    |
| `r` | Reprompt focused worker (textarea, Ctrl+Enter send, Esc cancel) |

Pause mode defaults to `"cancel"` (tears the subprocess down + snapshots
the session for `--resume`). `mode = "freeze"` (SIGSTOP) is exposed to
the lead via the MCP tool but not bound to a TUI key — long freezes
risk Anthropic dropping the HTTP session.

### Approval modal

Two flavors, distinguished by a title prefix:

- `[IN-FLIGHT ACTION]` — from `mcp__pitboss__request_approval`. Gates
  one mid-run action.
- `[PRE-FLIGHT PLAN]` — from `mcp__pitboss__propose_plan`. Gates the
  entire run when `[run].require_plan_approval = true`.

| Key             | Action                                                       |
|-----------------|--------------------------------------------------------------|
| `y`             | Approve                                                      |
| `n`             | Reject — opens an optional reason textarea (Ctrl+Enter submit) |
| `e`             | Edit summary — textarea (Ctrl+Enter submit, Esc cancel)      |
| `Esc`           | Close modal without deciding (approval stays queued)         |

Rejection `reason` (v0.6+) flows back through the MCP response so the
requesting actor can adapt without a separate reprompt round-trip.

### Approval list pane (v0.6+)

The approval list is a right-rail, **non-modal** pane that shows every
pending approval across the run tree. Unlike the modal, it doesn't
steal focus — the rest of the TUI stays interactive. Focus it with `a`.

| Key         | Action                                          |
|-------------|-------------------------------------------------|
| `Up` / `Down` | Navigate pending approvals                   |
| `Enter`     | Open the full approval modal for the selected entry |

Each list entry shows: requesting actor path (e.g. `root→S1`), category
(`tool_use` / `plan` / `cost` / `other`), TTL countdown if set, a
one-line summary. Policy-matched approvals never appear here — the
deterministic `[[approval_policy]]` matcher handles those in pure Rust
before they reach the queue.

## How it works

Every 250 ms a background thread re-reads:

- `<run-dir>/resolved.json` — full list of tasks
- `<run-dir>/summary.jsonl` — completed task records
- `<run-dir>/summary.json` — on clean finalize, replaces jsonl records
- `<run-dir>/tasks/<focused-id>/stdout.log` — tailed for the focus pane
- `<run-dir>/shared-store.json` — hub-visible KV + lease state (depth-2 runs)

Control keys speak to the dispatcher through a per-run
`<run-dir>/control.sock` unix socket; push events (approval requests,
worker state transitions, sub-lead lifecycle transitions) come back the
same way.

### Tile status

- **Pending** — in `resolved.json`, no `stdout.log` yet
- **Running** — `stdout.log` exists and was touched recently
- **Paused** — `pause_worker` was invoked; resumable via `continue_worker`
- **Frozen** — `pause_worker mode="freeze"` held the process in SIGSTOP
- **Done(Success|Failed|TimedOut|Cancelled|SpawnFailed)** — recorded in
  `summary.jsonl` / `summary.json`

### Sub-tree container status (v0.6+)

A container header summarizes the sub-lead's state: `running`,
`done`, `cancelled`, `budget-exceeded`, or `timed-out`, plus a live
spend counter (`$X.XX / $budget`) and worker count.

### Run status (in the picker + `list`)

- **complete** — `summary.json` parsed cleanly
- **running** — task records in `summary.jsonl`, no `summary.json` yet
- **aborted** — dispatcher wrote `manifest.snapshot.toml` +
  `resolved.json` but no task records (orphaned / killed-at-setup
  invocation)

Focus survives across snapshots by task id, so tiles don't jump when
new tasks complete.

## Troubleshooting

**"No runs directory found"** — You haven't run `pitboss dispatch` yet.
The runs directory is `~/.local/share/pitboss/runs/` unless overridden
by a manifest's `[run] run_dir` or the `--run-dir` flag.

**TUI renders garbage or hangs on input** — Your terminal may not
support the alternate screen buffer or raw mode. Try a different
terminal emulator.

**Occasional character leak on transitions** — Ratatui's diff is clean
at the buffer level (covered by a regression test); some emulators
don't reliably apply every cell update crossterm emits. A `dirty` flag
in the event loop forces `terminal.clear()` on focus change, mode
transition, resize, and SwitchRun to wipe stale cells. Residual cases
remain a known follow-up — see the CHANGELOG.

**"Error: No such device or address"** — You launched `pitboss-tui`
without a real terminal attached (e.g., under `bash -c` without a TTY).
`pitboss-tui list` works in non-TTY contexts; the interactive TUI
requires a real terminal.

**Approval list shows no entries even though I expect pending
approvals** — A `[[approval_policy]]` rule probably matched and
auto-approved/rejected them before they reached the queue. Check the
manifest's policy blocks against the approval's `actor` / `category`
/ `tool_name`. Policy matches are logged in the run dir.

**Frozen worker unfreezes itself after a long pause** — Anthropic's
HTTP session has an idle timeout; `pause_worker mode="freeze"` only
SIGSTOPs the local process. For pauses longer than a few minutes, use
`mode="cancel"` (the default) which tears down the HTTP session cleanly
and resumes via `claude --resume`.

## See also

- [`AGENTS.md`](../../AGENTS.md) — authoritative MCP tool reference
- [Operator guide — TUI](https://sds-mode.github.io/pitboss/operator-guide/tui.html) — full keybinding reference online
- [Approval policy reference](https://sds-mode.github.io/pitboss/operator-guide/approval-policy-reference.html)
