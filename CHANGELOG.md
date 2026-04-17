# Changelog

All notable changes to Pitboss are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

### Fixed
- Control-socket `approve` op now writes the `approval_response` event
  to `events.jsonl` and increments `worker_counters.approvals_{approved,
  rejected}` — matching `ApprovalBridge::respond`'s audit trail. The
  v0.4.0 queue-drain path bypassed `respond`, so approvals drained on
  TUI connect produced no response event and didn't bump counters.
  Surfaced by the new e2e approval round-trip test.

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

[Unreleased]: https://github.com/SDS-Mode/pitboss/compare/v0.4.0...HEAD
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
