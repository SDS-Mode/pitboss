# Changelog

All notable changes to Pitboss are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.4.3] — 2026-04-18

### Added
- **`TaskRecord.model` persistence.** Resolved model string is now
  captured on the `TaskRecord` at spawn time (lead + workers, both
  happy and spawn-fail paths) and round-tripped through JSON + SQLite.
  Backward-compatible: `#[serde(default)]` on the new field means
  pre-v0.4.3 records parse as `None`. SQLite store runs an idempotent
  `migrate_task_model` migration on open. The TUI watcher prefers the
  persisted model; log-scan fallback stays only for pre-v0.4.3 records.
  Eliminates the ~100 MB/s of redundant disk reads the old fallback did
  on every snapshot tick.
- **Per-actor shared-store activity counters.** Each grid tile shows a
  dim `kv:N lease:M` row when non-zero. Counters bump at tool-handler
  entry — before authz — so failed attempts show up too (useful for
  spotting workers spinning on bad paths). Surfaced via a new
  `ControlEvent::StoreActivity { counters: Vec<ActorActivityEntry> }`
  broadcast by the control server once per second per attached TUI.
- **Mouse affordances in the TUI.**
  - Left-click a grid tile — focus + open Detail view (equivalent to
    `hjkl` + `Enter`).
  - Left-click a run in the picker overlay — open that run (equivalent
    to highlighting + `Enter`).
  - Right-click inside Detail — exit back to the grid (symmetric with
    `Esc`).
  - Hit-test via per-frame cached tile/row rects in `AppState`, so
    clicks stay accurate across resizes.

## [0.4.2] — 2026-04-18

### Added
- **Worker shared store.** Per-run, in-memory, hub-mediated coordination
  surface for the lead and workers. Seven new MCP tools
  (`mcp__pitboss__kv_get`, `kv_set`, `kv_cas`, `kv_list`, `kv_wait`,
  `lease_acquire`, `lease_release`) exposed on the existing dispatcher
  MCP server. Workers now get their own narrow `mcp-config.json`
  (shared-store tools only — not spawn/cancel). Four namespaces:
  `/ref/*` (lead write), `/peer/<actor-id>/*` (actor write + lead
  override), `/shared/*` (all write), `/leases/*` (managed). Identity
  injection via extended `pitboss mcp-bridge --actor-id` / `--actor-role`
  flags stamping `_meta` into each forwarded MCP tool call. Ephemeral
  per run; optional finalize-time dump to `<run-dir>/shared-store.json`
  via `[run] dump_shared_store = true`. See
  `docs/superpowers/specs/2026-04-18-worker-shared-store-design.md`.
- **Unified TUI Detail view** (press `Enter` on a tile). Replaces the
  legacy `L` log-overlay + Snap-in modes with a single split-pane view:
  left pane shows identity, lifecycle, token totals + cost, activity
  counters (tool calls / results / top tools), and a one-shot
  `git diff --shortstat` summary. Right pane shows the scrollable log.
- **`/peer/self/` path alias for shared-store writes.** Workers don't
  have a natural way to discover their own actor_id (UUIDs are assigned
  at spawn for dynamically-spawned workers). Paths starting with
  `/peer/self/` are auto-resolved against the caller's actor_id, so
  task prompts can say "write to `/peer/self/findings.md`" without
  needing to template an id. Applies to `kv_get`/`kv_set`/`kv_cas`/
  `kv_list`/`kv_wait`.
- **In-flight git diff.** Dispatcher writes
  `<run-dir>/tasks/<task-id>/worktree.path` at worker/lead spawn time,
  so the Detail pane's GIT DIFF section shows live files-changed /
  +lines / -lines before the TaskRecord lands on finalize.
- **In-flight token + model stats.** Watcher scans each task's
  `stdout.log` for per-turn `message.usage` and `message.model` so
  token totals + model family populate from the first assistant turn,
  not only after finalize. Dynamic workers (not in `resolved.json`)
  finally get their model surfaced.
- **Tile signifiers.** Every grid tile now shows a
  model-family color swatch (opus = magenta, sonnet = blue, haiku =
  green) and a role glyph in its title (★ lead, ▸ worker), replacing
  the old `[LEAD]` text prefix.
- **Log pane QoL.** Per-event caps raised 3-5× with a `… +N chars`
  truncation marker; scroll-back buffer 500 → 2000 lines; new `J`/`K`
  keybinds scroll 5 rows (fills the gap between `j`/`k` = 1 and
  Ctrl-D/U = 10); mouse wheel bumped from 3 → 5 rows/tick.
- **Reset script for recurring dogfood.** `scripts/reset-ketchup-p0-dogfood.sh`
  tears down stale pitboss worktrees + `demo/ketchup-p0-*` branches
  and resets the ketchup checkout to the `dogfood/p0-baseline` tag so
  the 5-worker P0 refactor test is reproducible.

### Fixed
- **Kill commands now actually kill.** Two compounding bugs unified
  under one fix: (a) TUI's control-socket `send_op` ran on a fresh
  tokio runtime per call while the `ControlClient` writer was
  registered with the TUI startup runtime — cross-runtime async I/O
  silently hung. Switched to a stashed `Handle::spawn`. (b) `CancelRun`
  only terminated the lead-only `state.cancel` token; worker cancel
  tokens in `state.worker_cancels` were not cascaded. Both the TUI-driven
  `CancelRun` op and `dispatch/hierarchical.rs` finalize now iterate
  and terminate every worker token, so workers actually stop.
- **TUI log-pane bleed.** Detail-view log lines containing non-ASCII
  graphemes (e.g. `√`, `—`) could land cells past the pane's right
  edge into the metadata pane. ratatui 0.29's `Paragraph::render_text`
  has no `x < area.width` guard; we now paint via `Buffer::set_stringn`
  with an explicit `max_width` for guaranteed containment.
- **TUI scroll units.** Detail log scroll is now tracked in visual
  rows (post-wrap) rather than log-line indices, so long wrapped lines
  don't eat scroll budget and `jump-to-bottom` actually shows the end
  of the log regardless of wrap.

### Changed
- **MCP `structuredContent` is now always a record.** The shared-store
  tools `kv_get`, `kv_list`, and `list_workers` previously returned
  bare `null` / arrays, which Claude Code's MCP client rejected with
  `{"code":"invalid_type","message":"expected record, received ..."}`.
  Return shapes are now `{ entry: ... }`, `{ entries: [...] }`, and
  `{ workers: [...] }` respectively. **Breaking for callers** that
  expected bare arrays/nulls — unwrap one level.
- **Shared-store `Forbidden` errors now include caller's actor_id and
  a remediation hint.** Previously, "workers may write only their own
  `/peer/<self>/*`" left workers guessing what `<self>` resolves to.
  The message now names both the target peer and the caller's actual
  `actor_id`, and points at `/peer/self/...` as the always-correct
  path.
- **Run id abbreviation in TUI title bar** switched from the
  leading 8 chars (`019da1b8…`) to the last UUID segment
  (`…146e21f77dd8`). UUIDv7 time-prefixes collide across sibling runs
  from the same minute; the random tail actually differentiates.

## [0.4.1] — 2026-04-18

### Added
- **`fake-claude` MCP-client mode.** When `PITBOSS_FAKE_MCP_SOCKET` is
  set, the test-support `fake-claude` binary connects to a pitboss MCP
  socket and can issue real tool calls from a new `mcp_call` script
  action. Supports named bindings + whole-string template substitution
  (`"$w1.task_id"`) for chaining tool calls — covers realistic lead
  patterns in integration tests.
- **`crates/pitboss-cli/tests/e2e_flows.rs`** — four new end-to-end
  tests driving a real `fake-claude` subprocess through `SessionHandle`
  + `TokioSpawner`: spawn+wait happy path, 3-worker fan-out with
  wait_for_any, mid-flight cancel, and a full approval round-trip
  exercising the MCP→bridge→control→control→bridge→MCP loop with
  `fake-control-client`. Unlocks v0.4.1 feature development without
  Anthropic API calls.
- **`mcp__pitboss__reprompt_worker` MCP tool.** Lead-facing counterpart
  to the v0.4.0 operator-only `RepromptWorker` control-socket op. Lets
  a lead correct a wandering worker mid-flight with a new prompt while
  preserving the worker's claude session via `--resume`. Matches the
  control-socket op's state machine, event writes, and counter semantics
  exactly; prompt is required (not optional like `ContinueWorkerArgs`).
- **Notifications plugin system.** Trait-based `NotificationSink`
  abstraction in `pitboss-cli::notify` with four concrete sinks:
  `LogSink`, `WebhookSink`, `SlackSink`, `DiscordSink`. Routed via
  `NotificationRouter` with per-sink event filters (`events =
  [...]`) and `severity_min`. Typed `PitbossEvent` enum with three
  variants (`approval_request`, `run_finished`, `budget_exceeded`).
  Config via new `[[notification]]` manifest section; env-var
  substitution (`${FOO}`) for URLs. Fire-and-forget per sink via
  `tokio::spawn`, 3-attempt exponential backoff (100ms → 300ms →
  900ms), LRU dedup cache (size 64) prevents retry storms. New
  `TaskEvent::NotificationFailed` variant records delivery failures
  in `events.jsonl`.
- **Semantic log-line coloring.** TUI's focus-log pane and full-screen
  snap-in view now color each stream-json line by event type: white
  for assistant text, cyan for tool use, green for tool results, gray
  for system/unknown, magenta for result events, yellow for rate
  limits. Driven by the new `pitboss_tui::theme::log_line_style`
  helper. Unparseable lines fall back to gray.
- **Aborted run status.** `RunStatus` enum in `pitboss-tui::runs`
  distinguishes `Complete` (summary.json finalized), `Running`
  (task records in summary.jsonl, no summary.json yet), and `Aborted`
  (dispatcher wrote manifest + resolved but never produced any task
  records). Run picker overlay and `pitboss-tui list` both use the
  new label — orphaned run dirs no longer masquerade as running.
- **Refreshed TUI legends.** Statusbar hint now lists the full v0.4
  keybinding ladder (`hjkl`, `Enter`, `L`, `x/X`, `p/c`, `r`, `o`,
  `?`, `q`). Help overlay reorganized into Navigation / Views /
  Control / System sections and sized up to 70% × 80% to fit the
  v0.4 additions. Stale "OBSERVE mode" + "r = force refresh"
  footer lines removed.

### Fixed
- Control-socket `approve` op now writes the `approval_response` event
  to `events.jsonl` and increments `worker_counters.approvals_{approved,
  rejected}` — matching `ApprovalBridge::respond`'s audit trail. The
  v0.4.0 queue-drain path bypassed `respond`, so approvals drained on
  TUI connect produced no response event and didn't bump counters.
  Surfaced by the new e2e approval round-trip test.
- **TUI tile grid no longer retains prior-frame content in empty cells.**
  `render_tile_grid` now calls `frame.render_widget(Clear, area)`
  before drawing tiles, so partial-final-row dead space stays clean.
  Observed as character leakage during Stage 1 dogfood run (#129).
- **TUI summary/count responsiveness improved.** Watcher poll interval
  lowered from 500ms to 250ms; on-disk `summary.jsonl` writes now
  surface in the TUI within 250ms instead of up to 500ms (#128).
- **TUI text word-wraps at window width in log/tile/overlay bodies.**
  Added `Wrap { trim: false }` to Paragraphs in `render_tile`,
  `render_focus_log`, `render_snap_in`, `render_run_picker_overlay`,
  and `render_approval_modal`. Title bar and status bar intentionally
  stay single-line (truncation is desired there) (#130).
- **TUI color usage consolidated.** New `pitboss_tui::theme` module
  holds palette constants + style helpers. Status colors and UI
  accent colors flow through `theme::*` instead of inline `Color::*`
  literals scattered across `tui.rs`. No user-visible change in most
  views; fixes accidental drift where the same semantic state
  rendered in different colors in different contexts (#131).
- **TUI event loop handles `Event::Resize` + tracks layout-changing
  transitions.** Previously the loop matched only on `Event::Key`
  and silently dropped Resize events, so ratatui's autoresize
  shuffled buffer content when width changed and the diff left stale
  physical cells. A new `dirty` flag triggers `terminal.clear()`
  on resize, focus change, mode transition, and `SwitchRun` — one
  frame of flicker per transition in exchange for reliably clean
  redraws across terminal emulators. Common-case leakage fixed;
  occasional emulator-specific cells remain a known follow-up.

## [0.4.0] — 2026-04-17

### Added
- **Live control plane.** Per-run `control.sock` unix socket carrying
  line-JSON operations from `pitboss-tui` to the dispatcher and push
  events back. New TUI keybindings: `x` cancel focused worker (with
  confirm modal), `X` cancel entire run, `p` pause, `c` continue, `r`
  reprompt (textarea-driven).
- **Three new MCP tools.** `mcp__pitboss__pause_worker`,
  `mcp__pitboss__continue_worker`, `mcp__pitboss__request_approval` —
  the last blocks the lead until the operator approves, rejects, or
  edits, LangGraph-`interrupt()`-style.
- **`approval_policy` manifest field** under `[run]`. Values: `block`
  (default), `auto_approve`, `auto_reject`.
- **Pause = cancel-with-resume.** Pause terminates the worker
  subprocess but preserves `claude_session_id`; `continue_worker`
  spawns `claude --resume <id>`.
- **Per-task `events.jsonl`** audit file: pause, continue, reprompt,
  approval_request, approval_response events.
- **5 new `TaskRecord` counters.** `pause_count`, `reprompt_count`,
  `approvals_requested`, `approvals_approved`, `approvals_rejected`.
  Backfilled on disk via `#[serde(default)]` and in SQLite via the
  idempotent `migrate_v04_event_counters` migration.
- **`examples/v0.4-approval-demo.toml`** — minimal hierarchical
  manifest that exercises `approval_policy = "block"` + a
  `request_approval` interrupt + three tiny workers, for manual
  smoke-testing of the new keybindings.

### Changed
- `WorkerState::Running` now carries an `Option<String> session_id`;
  `WorkerState::Paused` is a new variant.
- `DispatchState::new` gained an `ApprovalPolicy` parameter.
- The lead's allowed MCP tool list now includes the three new tools.

### Backward compatibility
- v0.3.x manifests run unchanged; `approval_policy` defaults to
  `block`.
- v0.3.x runs on disk deserialize with counter fields defaulting to 0.
- SQLite DBs auto-migrate on next open.
- TUI pointed at a v0.3.x completed run enters observe-only mode when
  `control.sock` is absent.

## [0.3.4] — 2026-04-17

### Added
- **`AGENTS.md`** — agent-facing entry point with decision tree, full
  manifest schema reference, invocation patterns, run-directory
  interpretation guide, the 6 MCP tools, error patterns, and 4
  canonical examples (including a ketchup refactor case study).
- **`ROADMAP.md`** — deferred-work capture: near-term TUI kill
  design, medium-term features (broadcast, depth > 1, peer messaging,
  plan approval, full fake-claude E2E), explicitly retired items
  (interactive snap-in), and non-goals.
- **`examples/ketchup-refactor.toml`** — literal manifest used for the
  canonical AGENTS.md case study. 4-worker hierarchical audit on
  Haiku.
- **`x86_64-apple-darwin` target retired** from release workflow matrix
  (Intel Macs are EOL for new builds; Apple Silicon and Linux x86_64
  remain).

### Changed
- `scripts/smoke-part1.sh` passes `--run-dir "$SCRATCH/runs"` to every
  `pitboss dispatch` call so the script's trap cleanup sweeps test
  run artifacts. Previously a few cases left orphan dirs under
  `~/.local/share/pitboss/runs/`.
- `README.md` gains a one-line pointer to AGENTS.md at the top.

## [0.3.3] — 2026-04-17

### Added
- `aarch64-apple-darwin` and `x86_64-apple-darwin` release targets alongside
  the existing `x86_64-unknown-linux-gnu`. `pitboss-core` now enables
  `git2`'s `vendored-libgit2` feature so cross-compilation on macOS
  runners doesn't depend on a system libgit2.
- `server_drops_cleanly_even_with_active_connection` regression test
  asserting `McpServer::drop` completes within 500 ms even when a unix
  socket session is still open.

### Changed
- **MCP session cleanup.** `McpServer` now tracks per-connection tasks
  via `tokio_util::task::TaskTracker` and a `CancellationToken`. On
  drop, the token is cancelled so in-flight sessions exit their select
  arms immediately instead of running until their internal session
  timeout (up to 3600 s). Closes the reviewer-flagged orphan-task
  concern.
- **SQLite row projection.** `fetch_task_records` and `fetch_run_row`
  used to return 15- and 8-element positional tuples from
  `rusqlite::Row::query_map`, coupling field order to SQL column order.
  Replaced with `TaskRow` and `RunRow` structs that read by column name
  via `row.get("column_name")`. Adding a new column no longer silently
  re-maps existing fields. Two `#[allow(clippy::type_complexity)]`
  suppressions removed.

### Removed
- Module-wide `#![allow(dead_code)]` suppressions from
  `pitboss-cli/src/mcp/server.rs`, `mcp/tools.rs`, and
  `dispatch/state.rs`. The suppressions were vestigial from early
  development; every item is now wired up, so clippy runs clean
  without them.
- Unused `_store: Arc<dyn SessionStore>` parameter from
  `dispatch/runner.rs::execute_task`. Was a "planned but never wired"
  forward-reference.

## [0.3.2] — 2026-04-17

### Fixed
- TUI worker-switch latency. `pitboss-tui`'s watcher now wakes immediately
  on a focus change (`focus_rx.recv_timeout` replaces a 500 ms
  `thread::sleep`), dropping perceived switch latency from up-to-half-a-
  second to just the rebuild cost.
- `tail_log` seeks to the last 256 KiB of the log file instead of reading
  and parsing the entire file every poll. Per-poll work is now
  O(constant) regardless of log size; previously a 2 MB log meant
  10–30 ms of redundant parse work twice per second.

### Added
- README: `Install` section leads with pre-built tarball install; `Shell
  completions` subsection; `Continuous integration` + `Cutting a release`
  docs under Development.
- Three `tail_log` regression tests covering small files, >256 KiB files
  with mid-seek partial-line drop, and missing files.

## [0.3.1] — 2026-04-17

### Added
- GitHub Actions CI on every push/PR to `main`: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --features pitboss-core/test-support`, and
  `scripts/smoke-part1.sh`. Cached with `Swatinem/rust-cache@v2`.
- GitHub Actions release workflow triggered by `v*` tag push: builds
  cross-platform binaries and attaches `pitboss-<version>-<target>.tar.gz`
  archives to the GitHub release.
- `pitboss completions <shell>` and `pitboss-tui completions <shell>`
  subcommands generating shell completions for bash, zsh, fish, elvish,
  and powershell via `clap_complete`.
- `CHANGELOG.md` covering v0.1.0 through v0.3.1.

### Fixed
- Two clippy lints introduced in rustc 1.95 that the local 1.94 missed:
  `unnecessary_sort_by` in `pitboss-tui/src/runs.rs` (switched to
  `sort_by_key` + `cmp::Reverse`) and `map_unwrap_or` in
  `pitboss-tui/src/watcher.rs` (switched to `Result::is_ok_and`).

## [0.3.0] — 2026-04-17

### Added
- Hierarchical dispatch mode: a lead `claude` subprocess dynamically spawns
  workers via MCP tool calls over a unix socket. Six tools:
  `spawn_worker`, `worker_status`, `wait_for_worker`, `wait_for_any`,
  `list_workers`, `cancel_worker`.
- House rules for hierarchical runs: `max_workers` (<=16), `budget_usd` with
  reservation accounting + per-worker model tracking, `lead_timeout_secs`.
- `pitboss mcp-bridge <socket>` subcommand — stdio<->unix-socket proxy that
  bridges Claude's MCP client to the pitboss MCP server. Auto-launched by
  the lead's generated `--mcp-config`.
- TUI annotations: `[LEAD] <id>` prefix on lead tiles, `<- <lead-id>` on
  worker tiles, `— N workers spawned` status-bar counter, dynamic-worker
  discovery from summary.jsonl + filesystem.
- `parent_task_id: Option<String>` on `TaskRecord` with backward-compatible
  JSON deserialization.
- SQLite migrations: `parent_task_id` column + `shire_version` ->
  `pitboss_version` column rename, both idempotent via `pragma_table_info`.
- `pitboss resume` now works for hierarchical runs — re-dispatches the lead
  with `--resume <session-id>`; workers are not individually resumed.
- `pitboss validate` prints a hierarchical summary when a `[[lead]]` section
  is present.

### Changed
- **Rebrand**: crates renamed (`mosaic-core` -> `pitboss-core`, `shire-cli`
  -> `pitboss-cli`, `mosaic-tui` -> `pitboss-tui`); binaries renamed
  (`shire` -> `pitboss`, `mosaic` -> `pitboss-tui`); default runs dir now
  `~/.local/share/pitboss/runs/`; env vars under `PITBOSS_*`; MCP server
  name and tool prefix are `pitboss` / `mcp__pitboss__*`; README rewritten
  with a pit-boss-at-a-casino aesthetic.
- `truncate_preview` in `pitboss-core/src/session/handle.rs` now iterates
  by character instead of byte-slicing, so multi-byte UTF-8 (emoji) in
  claude output no longer panics the parser.

### Fixed
- Budget-guard TOCTOU: burst spawning used to bypass `budget_usd` because
  the guard only checked `spent_usd`. Now reserves the estimated cost at
  spawn time and releases it on completion.
- Budget estimator used to hardcode Haiku rates when pricing historical
  workers, undercounting Sonnet/Opus runs. Each worker's actual model is
  now tracked and used for both historical and fallback estimates.

## [0.2.2] — 2026-04-17

### Added
- `pitboss diff <run-a> <run-b>` subcommand: side-by-side run comparison
  with totals, per-task metrics, only-in-A / only-in-B sections, and
  `--json` output for scripting. Useful as a before/after tool with
  `pitboss resume`.

### Changed
- Moved `prices` module from the TUI crate to core so both binaries
  (pitboss + pitboss-tui) can compute cost from `TokenUsage`.

### Fixed
- Non-TTY progress table now emits exactly one line per completed task
  instead of repeating the last-registered task's row on every state
  change. Three regression tests lock the fix.

## [0.2.1] — 2026-04-17

### Added
- Run picker in the TUI (`o` to switch between runs without relaunching).
- SQLite session store alongside the existing JSON file store
  (`rusqlite` with bundled sqlite; `runs` + `task_records` tables with
  full init/append/finalize/load round-trip).
- `pitboss resume <run-id>` for flat-mode runs (reuses
  `claude_session_id` via `--resume <id>` in spawn args).
- View-only snap-in mode: press Enter on a running tile to enter a
  full-screen log view with scrolling.
- Cost estimation per tile (price-table lookup per model).
- Wall-clock run timer in the status bar.

## [0.2.0] — 2026-04-17

### Added
- TUI (`pitboss-tui`, then `mosaic`): tile grid of task state, live log
  tailing (stream-json parsed), 500 ms polling, read-only observation.
  Keybindings `h/j/k/l`, `L`, `r`, `?`, `q`; non-interactive `list` and
  `screenshot` subcommands.
- Stats bar with tokens + duration; per-task stderr routed to
  `stderr.log`.

### Fixed
- Dispatcher now calls `store.append_record` on each task completion
  (spec §5.3 invariant was silently violated in v0.1). Regression test
  added.
- `--verbose` wired into claude spawn args so stream-json output flows
  correctly.
- `summary.jsonl` persistence restored.

## [0.1.0] — 2026-04-16

### Added
- Headless dispatcher (`pitboss`, then `shire`) reading a TOML manifest
  and fanning out `claude` subprocesses under a concurrency cap.
- Per-task git worktree isolation.
- Stream-JSON parser for Claude output with token/cost accounting.
- Structured run artifacts: manifest snapshot, resolved config,
  summary.json, summary.jsonl, per-task logs.
- Graceful Ctrl-C handling: single SIGINT drains running tasks; second
  SIGINT terminates.
- Part 1 offline smoke test harness (`scripts/smoke-part1.sh`, 10 tests).

[Unreleased]: https://github.com/SDS-Mode/pitboss/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/SDS-Mode/pitboss/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/SDS-Mode/pitboss/compare/v0.3.4...v0.4.0
[0.3.4]: https://github.com/SDS-Mode/pitboss/compare/v0.3.3...v0.3.4
[0.3.3]: https://github.com/SDS-Mode/pitboss/compare/v0.3.2...v0.3.3
[0.3.2]: https://github.com/SDS-Mode/pitboss/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/SDS-Mode/pitboss/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/SDS-Mode/pitboss/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/SDS-Mode/pitboss/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/SDS-Mode/pitboss/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/SDS-Mode/pitboss/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/SDS-Mode/pitboss/releases/tag/v0.1.0
