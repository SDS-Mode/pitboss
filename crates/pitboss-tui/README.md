# Pitboss TUI

Terminal floor-view for Pitboss runs. Tile grid + live log tail + live
control plane (cancel / pause / continue / reprompt / approve).

## Quick start

```
cargo build --release -p pitboss-tui
./target/release/pitboss-tui          # open most recent run
./target/release/pitboss-tui list     # print table of runs, exit
./target/release/pitboss-tui 019d99   # open specific run by id prefix
./target/release/pitboss-tui --help
```

## Layout

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
│ [hjkl] nav  [Enter] snap  [L] log  [x/X] kill wrk/run  …             │
└───────────────────────────────────────────────────────────────────────┘
```

Focus-log lines are colored by event type: white for assistant text
(`> `), cyan for tool use (`* `), green for tool results (`< `),
magenta for result events (`v `), yellow for rate limits (`! `), gray
for unparseable / system / unknown.

## Keybindings

| Key                | Action                                               |
|--------------------|------------------------------------------------------|
| `h j k l` / arrows | Navigate tiles (wraps horizontally)                  |
| `Enter`            | Snap-in to focused tile (full-screen log view)       |
| `L`                | Toggle full-log overlay of focused tile              |
| `o`                | Run picker — switch to another run by id             |
| `x`                | Cancel focused worker (confirm modal)                |
| `X`                | Cancel entire run (confirm modal)                    |
| `p`                | Pause focused worker (preserves `claude_session_id`) |
| `c`                | Continue paused worker (`claude --resume`)           |
| `r`                | Reprompt focused worker (textarea, Ctrl+Enter send)  |
| `?`                | Help overlay                                         |
| `q` / `Ctrl-C`     | Quit                                                 |
| `Esc`              | Close any overlay / modal                            |

Approval modal (triggered by `mcp__pitboss__request_approval`): `y`
approve, `n` reject (with comment), `e` edit summary (Ctrl+Enter to
submit, Esc to cancel).

## How it works

Every 250 ms a background thread re-reads:

- `<run-dir>/resolved.json` — full list of tasks
- `<run-dir>/summary.jsonl` — completed task records
- `<run-dir>/summary.json` — on clean finalize, replaces jsonl records
- `<run-dir>/tasks/<focused-id>/stdout.log` — tailed for the focus pane

Control keys speak to the dispatcher through a per-run
`<run-dir>/control.sock` unix socket; push events (approval requests,
worker state transitions) come back the same way.

### Tile status

- **Pending** — in `resolved.json`, no `stdout.log` yet
- **Running** — `stdout.log` exists and was touched recently
- **Done(Success|Failed|TimedOut|Cancelled|SpawnFailed)** — recorded in
  `summary.jsonl` / `summary.json`

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
