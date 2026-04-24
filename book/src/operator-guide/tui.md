# TUI

`pitboss-tui` is the live floor view: a tile grid of all workers in the current run, with live log tailing, budget and token counters, and a full control plane for cancellation, pause, reprompt, and approval management.

## Opening the TUI

```bash
pitboss-tui                # open the most recent run
pitboss-tui 019d99         # open a run by UUID prefix
pitboss-tui list           # print a table of runs to stdout
```

The TUI polls the run directory at 250ms intervals. It can open a run while dispatch is in progress (most useful) or after the fact for post-mortem review.

## Grid view

The main view is a tile grid. Each tile represents one actor (lead, sub-lead, or worker). Tiles show:

- Actor role and ID (with `★` for leads, `▸` for workers)
- Current state: `Running`, `Done`, `Failed`, `Paused`, `Frozen`, `Cancelled`, etc.
- Model family color swatch (opus = magenta, sonnet = blue, haiku = green)
- Partial token count and cost estimate
- KV/lease activity counters when non-zero (`kv:N lease:M`)

In depth-2 runs, sub-trees render as collapsible containers with a header showing the sub-lead ID, budget bar, worker count, and approval badge.

## Detail view

Press `Enter` on a tile to open the Detail view. It's a split pane:

- **Left** — identity, lifecycle, token totals + cost, activity counters (tool calls / results / top tools), and a one-shot `git diff --shortstat` summary.
- **Right** — scrollable log with semantic color coding (assistant text = white, tool use = cyan, tool results = green, rate limits = yellow, result events = magenta, system = gray).

Scroll the log:

| Keys | Scroll |
|------|--------|
| `j` / `k` / arrows | 1 row |
| `J` / `K` | 5 rows |
| `Ctrl-D` / `Ctrl-U` / `PageDown` / `PageUp` | 10 rows |
| `g` / `G` | Jump to top / bottom (G re-enables auto-follow) |
| Scroll wheel | 5 rows/tick |

## Navigation keybindings

| Key | Action |
|-----|--------|
| `h j k l` / arrows | Navigate tiles in grid view |
| `Enter` | Open Detail view for focused tile |
| `o` | Run picker (switch to another run) |
| `?` | Help overlay (full keybinding reference) |
| `q` / `Ctrl-C` | Quit |
| `Esc` | Close any overlay or modal |
| `Tab` | Cycle focus across sub-tree containers (depth-2 runs) |

## Control plane keybindings

| Key | Action |
|-----|--------|
| `x` | Cancel focused worker (with confirm modal) |
| `X` | Cancel entire run (cascades to all workers) |
| `p` | Pause focused worker (requires initialized session) |
| `c` | Continue paused worker |
| `r` | Open reprompt textarea (Ctrl+Enter to submit, Esc to cancel) |

## Approval pane

Press `'a'` to focus the approval list pane (right-rail, 30% width). Pending approvals queue here as they arrive.

| Key | Action |
|-----|--------|
| `Up` / `Down` | Navigate the approval queue |
| `Enter` | Open detail modal for selected approval |

In the approval modal:

| Key | Action |
|-----|--------|
| `y` | Approve |
| `n` | Reject (optionally add a reason comment) |
| `e` | Edit the summary (Ctrl+Enter to submit, Esc to cancel) |

## Mouse support

| Action | Effect |
|--------|--------|
| Left-click tile | Focus + open Detail |
| Left-click run in picker | Open that run |
| Right-click inside Detail | Exit back to grid |
| Scroll wheel inside Detail | Scroll log 5 rows/tick |

## Live policy editor (v0.8+)

Press `P` in Normal mode to open the policy editor overlay. It shows the current `[[approval_policy]]` rules with their actions.

| Key | Action |
|-----|--------|
| `j` / `k` / arrows | Navigate the rule list |
| `Space` / `Enter` | Cycle the action (`auto_approve` → `auto_reject` → `block`) |
| `n` | Add a new rule |
| `d` | Delete the selected rule |
| `s` / `F2` | Save and apply live (sends `UpdatePolicy` to the dispatcher) |
| `Esc` | Cancel without saving |

Changes take effect immediately without restarting dispatch.

## `pitboss status` — headless run snapshot (v0.8+)

For a quick table of task records without the full TUI:

```bash
pitboss status <run-id>          # formatted table
pitboss status <run-id> --json   # JSON output for scripting
```

Run ID supports prefix matching identical to `attach`. Works on in-flight runs (reads `summary.jsonl`) and finalized runs (reads `summary.json`).

## `pitboss attach` — single-worker follow mode

For a terminal-only follow view on one worker without the full TUI:

```bash
pitboss attach <run-id> <task-id>
pitboss attach <run-id> <task-id> --raw       # stream raw stream-JSON jsonl
pitboss attach <run-id> <task-id> --lines 200 # larger backfill
```

Run-id is resolved by prefix (first 8 chars are enough when unique). Exits on Ctrl-C or when the worker emits its terminal result.
