# Changelog

All notable changes to Pitboss are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/).

CHANGELOG entries from v0.9.2 onward are generated from commit messages
by `git-cliff` at release time. Hand-editing this file in feature PRs
is no longer required (or recommended — it causes merge conflicts).
See #242 for the adoption notes.

## [0.9.2] — 2026-04-30

The container subsystem grows up. Operators iterating against
`pitboss container-dispatch` get a real toolchain story:
declare apt packages and host files in the manifest, bake them
into a derived image once with `pitboss container-build`, and let
`container-dispatch` pick up the cached tag automatically. Stale
derived images sweep with `pitboss container-prune`. The published
`pitboss-with-claude` image now rolls forward on every main merge
(rolling `:main` and `:main-<sha>` tags) instead of stagnating
between releases. Plus the usual smaller polish — auto-refresh on
the runs list and run-detail pages, persisted per-task cost on
`TaskRecord`, an SSE event-stream filter on the run-detail page,
and a graceful shutdown contract for `pitboss-web stop`.

Highlights:

- **`[container].extra_apt` + `[[container.copy]]`** — declare
  apt packages and host-file copies in the manifest. Two paths:
  install at dispatch start (slow) or `pitboss container-build` to
  bake into a derived image and amortize across runs.
- **`pitboss container-build` + `pitboss container-prune`** —
  deterministic SHA-tagged derived images with idempotent re-runs,
  and a sweeper for the stale tags that accumulate as manifests
  evolve.
- **Image-cadence fix (#258)** — `:main`, `:main-<sha>`, and
  `:latest` now roll forward on every main merge, ending the
  "0.9.1 is two days old, my fix is in main, why isn't it in the
  image" mode that bit operators twice in the post-0.9.1 window.
- **Cost telemetry** — per-task `cost_usd` now persists on
  `TaskRecord` at finalize time and the run-detail page renders
  per-task and run-total cost estimates client-side.

### Added

- Pitboss container-prune for stale derived images ([#270](https://github.com/SDS-Mode/pitboss/pull/270))
- Warn when extra_apt-only manifest's derived image is missing ([#269](https://github.com/SDS-Mode/pitboss/pull/269))
- Pitboss container-build subcommand + [[container.copy]] ([#264](https://github.com/SDS-Mode/pitboss/pull/264))
- Bootstrap apt packages via [container].extra_apt ([#263](https://github.com/SDS-Mode/pitboss/pull/263))
- Persist per-task cost_usd on TaskRecord at finalize time ([#245](https://github.com/SDS-Mode/pitboss/pull/245))
- Per-task and run-total cost estimates on run-detail page ([#240](https://github.com/SDS-Mode/pitboss/pull/240))
- SSE event-stream filter UI on run-detail page ([#236](https://github.com/SDS-Mode/pitboss/pull/236))
- Pitboss-web stop subcommand + graceful shutdown ([#235](https://github.com/SDS-Mode/pitboss/pull/235))
- GET /api/runs/{id}/tasks/{task_id} task-metadata endpoint ([#233](https://github.com/SDS-Mode/pitboss/pull/233))
- Async McpServer::shutdown with deterministic per-connection cleanup (#151 M2) ([#216](https://github.com/SDS-Mode/pitboss/pull/216))
- SessionStore::iter_runs metadata-only enumeration (#149 L8) ([#213](https://github.com/SDS-Mode/pitboss/pull/213))
- SessionHandle builder overrides + resume-failure hint (#149, #184) ([#202](https://github.com/SDS-Mode/pitboss/pull/202))


### Changed

- **Image-cadence fix (#258, [#265](https://github.com/SDS-Mode/pitboss/pull/265))** — `:main`, `:main-<sha>`, and `:latest` on `ghcr.io/sds-mode/pitboss-with-claude` now roll forward on every main merge, not only on release-tag pushes. **Behavior change**: `:latest` strictly tracks main HEAD now, not the most recent release tag. To pin a release, use a version tag (`:0.9.1`, `:0.9`, `:0`); for fixes-since-release use `:main` or `:latest`. Reproducibility: `:main-<short-sha>` is the SHA-pinned alternative.
- Schema-first matching against API error envelopes (#185 medium) ([#219](https://github.com/SDS-Mode/pitboss/pull/219))
- Split tools.rs into per-feature submodules (#151 L6) ([#218](https://github.com/SDS-Mode/pitboss/pull/218))
- SqliteStore migration version table (#149 L11) ([#212](https://github.com/SDS-Mode/pitboss/pull/212))
- O(1) sub-tree-worker routing in cancel_actor_with_reason ([#205](https://github.com/SDS-Mode/pitboss/pull/205))
- Consolidate entry-point boilerplate (#150 M9) ([#204](https://github.com/SDS-Mode/pitboss/pull/204))
- Decompose runner::execute into per-phase functions ([#203](https://github.com/SDS-Mode/pitboss/pull/203))
- Extract kill+resume loop helper ([#201](https://github.com/SDS-Mode/pitboss/pull/201))
- Collapse cancel-cascade watcher tasks; close #100 (PR 100.3) ([#200](https://github.com/SDS-Mode/pitboss/pull/200))
- Centralize cancel-cascade through CancelToken::cascade_to (#100, PR 100.2) ([#199](https://github.com/SDS-Mode/pitboss/pull/199))


### Dependencies

- Bump rmcp from 0.8.5 to 1.5.0 ([#176](https://github.com/SDS-Mode/pitboss/pull/176))
- Bump rusqlite from 0.32.1 to 0.39.0 ([#177](https://github.com/SDS-Mode/pitboss/pull/177))
- Bump lru from 0.12.5 to 0.16.4 ([#178](https://github.com/SDS-Mode/pitboss/pull/178))
- Bump ratatui from 0.29.0 to 0.30.0 ([#180](https://github.com/SDS-Mode/pitboss/pull/180))
- Bump the rust-minor-and-patch group across 1 directory with 2 updates ([#173](https://github.com/SDS-Mode/pitboss/pull/173))


### Fixed

- Auto-refresh runs list, re-fetch run record in detail tick ([#247](https://github.com/SDS-Mode/pitboss/pull/247))
- Manifest_name fallback + dim aborted rows (#227, #229) ([#234](https://github.com/SDS-Mode/pitboss/pull/234))
- Line- and JSON-aware excerpt for failures dashboard (#222, #223) ([#232](https://github.com/SDS-Mode/pitboss/pull/232))
- Correct parent_task_id + retain terminated subleads in WorkersSnapshot ([#239](https://github.com/SDS-Mode/pitboss/pull/239))
- Tokens / workers / runtime cards on run-detail page ([#238](https://github.com/SDS-Mode/pitboss/pull/238))
- Poll summary.jsonl + list_workers while run is in-progress ([#237](https://github.com/SDS-Mode/pitboss/pull/237))
- Bump approval counters from every resolution path ([#231](https://github.com/SDS-Mode/pitboss/pull/231))
- Summary.json now includes every actor across all layers ([#230](https://github.com/SDS-Mode/pitboss/pull/230))
- Default_approval_policy now unconditional, regardless of TUI ([#220](https://github.com/SDS-Mode/pitboss/pull/220))
- Cost_over rules now fire for propose_plan + permission_prompt (#151 M5) ([#217](https://github.com/SDS-Mode/pitboss/pull/217))
- Half-close socket write half on c2s EOF (#151 L1) ([#214](https://github.com/SDS-Mode/pitboss/pull/214))
- Return retained Worktree from cleanup (#149 L9) ([#211](https://github.com/SDS-Mode/pitboss/pull/211))
- Async git-diff summary on detail-view open (#154 M3) ([#210](https://github.com/SDS-Mode/pitboss/pull/210))
- Join workers with timeout instead of fixed sleep on hierarchical drain (#150 M6+M7) ([#209](https://github.com/SDS-Mode/pitboss/pull/209))
- Drop dead viewport constant + cancel old bridge-forwarder on SwitchRun ([#207](https://github.com/SDS-Mode/pitboss/pull/207))
- Don't strand approvals when no control client is attached (#154 L2) ([#208](https://github.com/SDS-Mode/pitboss/pull/208))
- Write journal line before bumping failed_emits_total ([#198](https://github.com/SDS-Mode/pitboss/pull/198))
- Emit warn! when rate-limit reset_at parse fails ([#194](https://github.com/SDS-Mode/pitboss/pull/194))
- Consolidate depth-2 invariant enforcement + web socket-path resolution ([#192](https://github.com/SDS-Mode/pitboss/pull/192))
- Typed schema-version + alias regression test for resolved.json snapshots ([#191](https://github.com/SDS-Mode/pitboss/pull/191))
- Crash-safe atomic writes for summary.json and meta.json (#184, #188) ([#190](https://github.com/SDS-Mode/pitboss/pull/190))


## [0.9.1] — 2026-04-27

### Dependencies

- Bump git2 from 0.19.0 to 0.20.4 ([#181](https://github.com/SDS-Mode/pitboss/pull/181))
- Bump toml from 0.8.23 to 1.1.2+spec-1.1.0 ([#174](https://github.com/SDS-Mode/pitboss/pull/174))
- Bump crossterm from 0.28.1 to 0.29.0 ([#179](https://github.com/SDS-Mode/pitboss/pull/179))
- Bump thiserror from 1.0.69 to 2.0.18 ([#175](https://github.com/SDS-Mode/pitboss/pull/175))


## [0.9.0] — 2026-04-27

### Added

- Pitboss-web operational console — Phases 1–5 + tile grid + flow graph + Slice A insights + manifests wizard ([#169](https://github.com/SDS-Mode/pitboss/pull/169))
- Pitboss dispatch --background for non-blocking dispatch (#133-C) ([#138](https://github.com/SDS-Mode/pitboss/pull/138))
- [lifecycle] section + survive_parent (#133-A) ([#137](https://github.com/SDS-Mode/pitboss/pull/137))
- Pitboss list [--active] [--json] (#133-B) ([#136](https://github.com/SDS-Mode/pitboss/pull/136))
- Parent-orchestrator notify hook ([#135](https://github.com/SDS-Mode/pitboss/pull/135))
- Expose untruncated final assistant message ([#134](https://github.com/SDS-Mode/pitboss/pull/134))
- Pitboss tree pre-flight subcommand + cost gate ([#132](https://github.com/SDS-Mode/pitboss/pull/132))
- Pitboss prune — sweep orphaned run directories ([#131](https://github.com/SDS-Mode/pitboss/pull/131))
- Stale state + connect-based liveness probe ([#130](https://github.com/SDS-Mode/pitboss/pull/130))
- Pitboss init [output] [-t simple|full] [--force] ([#129](https://github.com/SDS-Mode/pitboss/pull/129))
- Pitboss schema --format=example + docs/manifest-reference.toml ([#128](https://github.com/SDS-Mode/pitboss/pull/128))
- Pitboss schema + auto-generated docs/manifest-map.md ([#127](https://github.com/SDS-Mode/pitboss/pull/127))
- FieldMetadata derive — per-field labels, help, form_type, enum_values ([#126](https://github.com/SDS-Mode/pitboss/pull/126))
- V0.9 schema redesign — single canonical [lead], renames, validate guidance ([#123](https://github.com/SDS-Mode/pitboss/pull/123))
- External MCP injection + validate promptless lead + AGENTS.md ([#122](https://github.com/SDS-Mode/pitboss/pull/122))
- Completed page + compact tiles + nit fixes ([#117](https://github.com/SDS-Mode/pitboss/pull/117))


### Fixed

- Close all 12 #153 audit items (3 medium + 9 low) ([#168](https://github.com/SDS-Mode/pitboss/pull/168))
- Close out #152 audit tracker (7 items) ([#167](https://github.com/SDS-Mode/pitboss/pull/167))
- Close out #155 and #156 audit trackers ([#166](https://github.com/SDS-Mode/pitboss/pull/166))
- Pitboss-schema + pitboss-schema-derive audit follow-ups ([#165](https://github.com/SDS-Mode/pitboss/pull/165))
- Runs.rs socket-path double-nesting + #157 audit follow-ups ([#164](https://github.com/SDS-Mode/pitboss/pull/164))
- Unblock container-dispatch hierarchical manifests + audit cleanup ([#163](https://github.com/SDS-Mode/pitboss/pull/163))
- Redact webhook secrets in errors + tighten SSRF blocklist ([#162](https://github.com/SDS-Mode/pitboss/pull/162))
- Process-group signaling + immediate PID slot clear (#147 #148) ([#161](https://github.com/SDS-Mode/pitboss/pull/161))
- Authz hardening — token-bound identity + cross-layer writes (#144, #145, #146) ([#160](https://github.com/SDS-Mode/pitboss/pull/160))
- Completed page UX fixes + log pane text leakage ([#120](https://github.com/SDS-Mode/pitboss/pull/120))
- Address bugs #104 #105 #106 #107 ([#119](https://github.com/SDS-Mode/pitboss/pull/119))
- Remove scroll-up-at-top exits-Detail gesture (#114 follow-up) ([#115](https://github.com/SDS-Mode/pitboss/pull/115))
- Scroll-to-zoom into Detail + bottom-anchor focus log preview ([#114](https://github.com/SDS-Mode/pitboss/pull/114))
- Replay bridge-held approvals on TUI reconnect ([#103](https://github.com/SDS-Mode/pitboss/pull/103))


## [0.8.0] — 2026-04-24

### Added

- Pitboss container-dispatch subcommand ([#90](https://github.com/SDS-Mode/pitboss/pull/90))


### Changed

- Remove DispatchState Deref; fix worktree test helper ([#75](https://github.com/SDS-Mode/pitboss/pull/75))


### Fixed

- Resolve 13 medium-severity bugs in control/dispatch/store/notify paths ([#89](https://github.com/SDS-Mode/pitboss/pull/89))
- Evict stale queue entry when request_approval TTL fires ([#73](https://github.com/SDS-Mode/pitboss/pull/73))
- Resolve 13 medium-severity bugs in dispatch/TUI/storage/notify paths ([#74](https://github.com/SDS-Mode/pitboss/pull/74))
- Resolve 5 high-severity bugs in approval/session/TUI paths ([#71](https://github.com/SDS-Mode/pitboss/pull/71))
- Isolate pitboss-spawned claude from operator ~/.claude/ plugins ([#48](https://github.com/SDS-Mode/pitboss/pull/48))
- Cancel_run cascades to sub-lead layers + their workers ([#47](https://github.com/SDS-Mode/pitboss/pull/47))
- Bubble classified API failures to parent + gate spawns ([#49](https://github.com/SDS-Mode/pitboss/pull/49))
- Defaults.env plumbing + orchestration allowlist gaps ([#45](https://github.com/SDS-Mode/pitboss/pull/45))
- Catch allow_subleads=true with no sublead_defaults fallback ([#44](https://github.com/SDS-Mode/pitboss/pull/44))


### V0.8

- Permission routing, approval TTL, policy editor, and status command ([#91](https://github.com/SDS-Mode/pitboss/pull/91))


## [0.7.0] — 2026-04-20

### Added

- Headless-mode hardening (lessons-learned fixes + Path A permission default) ([#40](https://github.com/SDS-Mode/pitboss/pull/40))
- Bundle AGENTS.md into binary + container image ([#39](https://github.com/SDS-Mode/pitboss/pull/39))
- Add pitboss-with-claude variant ([#37](https://github.com/SDS-Mode/pitboss/pull/37))


### Fixed

- Note_actor on run_lease_acquire/release + rmcp-driven cleanup test
- Correct ApprovalCategory enum values to snake_case in TOML examples
- Cookbook link in intro went to README.html (404)
- Drop invalid multilingual field from book.toml
- Re-track smoke scripts; narrow ignore to ketchup only
- Close reconcile/lease race in run_global_lease_serializes_two_subleads


## [0.6.0] — 2026-04-20

### Added

- Extend kill-with-reason delivery to root-lead targets
- Wire send_synthetic_reprompt to real kill+resume delivery
- Implement spawn_sublead_session — real sub-lead subprocess lifecycle
- Add sublead_spawn_args helper for v0.6 sub-lead spawning
- Allow_subleads + caps + sublead_defaults
- Approval_pending notification category
- Non-modal approval list pane + reject-with-reason input
- Grouped grid with collapsible sub-tree containers
- Add EventEnvelope + sub-lead lifecycle events (Task 4.6)
- Kill-with-reason cascades to parent lead
- TTL watcher for pending approvals
- Reject-with-reason on approval response
- TOML approval policy matcher
- Add rich fields to approval record (Task 4.1)
- Auto-release run-global leases on actor termination
- Add run_lease_acquire and run_lease_release tools
- Add run-global LeaseRegistry
- Per-layer KvStore + strict peer visibility
- Reconcile sub-lead budget on termination
- Cascade cancel from root to sub-trees
- Enforce depth-2 cap on spawn_sublead
- Implement spawn_sublead end-to-end
- Add spawn_sublead tool stub
- Accept sublead actor_role in bridge _meta injection
- Add wait_actor as generalized wait_worker
- Add ActorRole, ActorPath, ActorId types


### Changed

- Address review feedback on LayerState extraction
- Extract LayerState from DispatchState
- Address review feedback on actor types


### Fixed

- Route spawn_worker into caller's layer based on _meta.actor_role
- Wait_actor now works on sub-lead actor ids
- Preserve cancel_worker task_id parameter for wire back-compat
- Correct actor_path for sub-lead approval requests
- Close kv_wait peer-visibility hole + remove try_read silent fallthrough
- Wire original_reservation_usd through LayerState end-to-end
- Cascade-cancel covers sub-leads spawned during drain
- Allow explicit _meta in call_tool to override connection default
- Root budget guard + reservation rollback in spawn_sublead
- Inject_meta writes to params.arguments._meta to match wire path


## [0.5.3] — 2026-04-19

### Fixed

- Deflake freeze_then_resume_flips_proc_state ([#28](https://github.com/SDS-Mode/pitboss/pull/28))


### Infra

- Migrate to cargo-dist + add GHCR container image ([#29](https://github.com/SDS-Mode/pitboss/pull/29))


## [0.5.0] — 2026-04-19

## [0.4.4] — 2026-04-18

## [0.4.3] — 2026-04-18

## [0.4.2] — 2026-04-18

### Fixed

- Assert shape of `pitboss version` instead of pinning 0.1.0


## [0.4.1] — 2026-04-18

### Added

- Emit BudgetExceeded envelope at budget-guard error site
- Build notification router in runner::execute; emit RunFinished (flat)
- Emit ApprovalRequest envelope in ApprovalBridge::request
- Add notification_router field (None default) to DispatchState
- Add TaskEvent::NotificationFailed variant
- Reject malformed [[notification]] configs at parse time
- Resolve [[notification]] with env-var substitution; update literals
- Add [[notification]] section to Manifest
- Add DiscordSink (embed format, color-by-severity) + 3 wiremock tests
- Add WebhookSink + 3 wiremock tests (success/4xx/5xx)
- Add SlackSink (Block Kit formatting) + 3 wiremock tests
- Add LogSink with tracing_test unit test
- Add NotificationRouter with LRU dedup + retry + 3 integration tests
- Add NotificationConfig + env-var substitution with 8 unit tests
- Add NotificationSink trait (async_trait)
- Add NotificationEnvelope with auto-derived dedup_key
- Add PitbossEvent enum with 3 variants + kind() helper
- Add Severity enum with ordered filtering support


### Dependencies

- Add reqwest/async-trait/lru/wiremock/tracing-test for notifications


## [0.4.0] — 2026-04-17

### ROADMAP

- Promote TUI kill into v0.4 scope; capture new deferred items


### Scripts

- Pass --run-dir to smoke-part1 dispatches ([#5](https://github.com/SDS-Mode/pitboss/pull/5))


### V0.3.4

- AGENTS.md + canonical ketchup example ([#4](https://github.com/SDS-Mode/pitboss/pull/4))


### V0.4.0

- Live control plane + approval interrupts ([#6](https://github.com/SDS-Mode/pitboss/pull/6))


## [0.3.3] — 2026-04-17

## [0.3.2] — 2026-04-17

### README

- Document install-from-release, completions, CI, release process


### Pitboss-tui

- Interruptible watcher + seek-based log tail ([#2](https://github.com/SDS-Mode/pitboss/pull/2))


## [0.3.1] — 2026-04-17

## [0.3.0] — 2026-04-17

### README

- Drop "dealer" — use worker/lead throughout
- Casino aesthetic — dealers, house rules, the pit
- Tighten + reflect v0.3 state
- Document v0.3 hierarchical mode with example


### SqliteStore

- Add parent_task_id column with idempotent migration


### Dispatch

- Hierarchical mode detection + run_hierarchical scaffold
- DispatchState shared between runner and MCP server


### Fake-mcp-client

- Real rmcp client connect + call_tool


### Hierarchical

- Auto-allow shire MCP tools in lead's --allowedTools
- Shire mcp-bridge subcommand for stdio↔socket proxy
- Fake-claude tool_use emission + deferred e2e placeholder
- Integration tests for cap/budget/drain guards
- Integration test for spawn + list round-trip
- Cancel in-flight workers on lead exit + persist records
- Spawn lead with --mcp-config and persist its record


### Manifest

- Hierarchical-mode validation (mutex, ranges, lead checks)
- Resolve [[lead]] into ResolvedLead with defaults inheritance
- Add [[lead]] section and hierarchical [run] fields


### Mcp

- Budget reservation + per-worker model tracking
- Wire real worker subprocess spawn + cost accounting + prompt preview
- Per-worker CancelToken in DispatchState; cancel_worker targets one
- Defensive re-scan in wait_for_worker + wait_for_any
- Wire six tools into rmcp ServerHandler on UnixListener
- Wait_for_any — race waiter across multiple task_ids
- Wait_for_worker with broadcast channel + timeout path
- Worker_status + cancel_worker handlers (minimal, refined in Task 22)
- Spawn_worker guards (cap, budget with median estimate, drain)
- Handle_spawn_worker happy path (no guards, no real spawn yet)
- Handle_list_workers tool handler with filtering and state mapping
- Server start/stop lifecycle with unix-socket listener
- Module scaffolding + socket path helper


### Mosaic-tui

- Watcher discovers lead + dynamic workers for hierarchical runs
- Status bar shows '<N> workers spawned' counter
- Worker tiles show ← <parent-id> annotation
- [LEAD] prefix + bold border on lead tile
- Thread parent_task_id through TileState from TaskRecord


### Pitboss-tui

- Rebrand missed strings in title bar and help overlay


### Rebrand

- Shire → pitboss across paths, env vars, MCP names, SQLite schema


### Session

- Fix truncate_preview panic on multi-byte boundaries


### Shire

- Validate shows hierarchical manifest summary


### Sqlite

- Cover re-open idempotency + refresh schema evolution note


### V0.3.1

- WorkerStatus JsonSchema, lead tile Cyan border, README resume note


## [0.2.2] — 2026-04-17

## [0.2.1] — 2026-04-17

## [0.2.0] — 2026-04-17

<!-- generated by git-cliff -->
