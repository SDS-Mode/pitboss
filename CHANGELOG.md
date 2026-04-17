# Changelog

All notable changes to Pitboss are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/SDS-Mode/pitboss/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/SDS-Mode/pitboss/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/SDS-Mode/pitboss/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/SDS-Mode/pitboss/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/SDS-Mode/pitboss/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/SDS-Mode/pitboss/releases/tag/v0.1.0
