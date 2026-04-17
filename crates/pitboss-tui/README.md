# Pitboss TUI

Terminal observer for Pitboss runs. v0.2-alpha — read-only tile grid.

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
│  > Checking crates/pitboss-core/src/parser/mod.rs                    │
│  > Linting workspace...                                              │
│  > ...                                                                │
│                                                                       │
│ [h/j/k/l] nav  [L] log  [r] refresh  [?] help  [q] quit              │
└───────────────────────────────────────────────────────────────────────┘
```

## Keybindings

| Key                | Action                                |
|--------------------|---------------------------------------|
| `h j k l` / arrows | Navigate tiles (wraps horizontally)   |
| `L`                | Toggle full-log overlay of focused tile |
| `r`                | Force redraw (watcher still polls 500ms) |
| `?`                | Help overlay                          |
| `q` / `Ctrl-C`     | Quit                                  |
| `Esc`              | Close any overlay                     |

## How it works

Pitboss TUI does not launch or manage sessions in v0.2-alpha — it only **observes** runs
created by `pitboss`. Every 500 ms a background thread re-reads:

- `<run-dir>/resolved.json` to get the full list of tasks
- `<run-dir>/summary.jsonl` for completed task records
- `<run-dir>/tasks/<focused-id>/stdout.log` to tail the focus pane

Task status rules:

- **Pending** — in `resolved.json`, no `stdout.log` yet
- **Running** — `stdout.log` exists and was touched within the last 5 seconds
- **Done(Success|Failed|TimedOut|Cancelled|SpawnFailed)** — recorded in `summary.jsonl`

If you launch `pitboss-tui` while a `pitboss dispatch` is still running, you'll see tasks
flip Pending → Running → Done live. Focus survives across snapshots by task id, so
the focused tile doesn't jump when new tasks complete.

## Deferred to v0.2.1+

- Snap-in / Enter (keystroke passthrough to a running session)
- `n` to launch a new session from within the TUI
- `x` to kill a running session
- `r` to resume (`claude --resume <session-id>`)
- SQLite-backed cross-restart session state
- Run picker (currently: latest run or specified id)

## Troubleshooting

**"No runs directory found"** — You haven't run `pitboss dispatch` yet. The runs
directory is `~/.local/share/pitboss/runs/` unless overridden by a manifest's
`[run] run_dir` or the `--run-dir` flag.

**TUI renders garbage or hangs on input** — Your terminal may not support the
alternate screen buffer or raw mode. Try a different terminal emulator.

**"Error: No such device or address"** — You launched `pitboss-tui` without a real
terminal attached (e.g., under `bash -c` without a TTY). `pitboss-tui list` works in
non-TTY contexts; the interactive TUI requires a real terminal.
