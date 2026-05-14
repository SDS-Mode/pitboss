# Changelog

All notable changes to Pitboss are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project uses [Semantic Versioning](https://semver.org/).

CHANGELOG entries from v0.9.2 onward are generated from commit messages
by `git-cliff` at release time. Hand-editing this file in feature PRs
is no longer required (or recommended — it causes merge conflicts).
See #242 for the adoption notes.

## [0.14.0] — 2026-05-14

v0.14.0 closes out the **unified envelope API** (#438): a single
`RunStreamItem` stream in `pitboss-core::stream` is now the canonical
read path for live + historical run state across CLI, TUI, and web.
The dispatcher accepts subscriber-mode client handshakes so multiple
read-only viewers (web SPA in two tabs, TUI mirror, a future `pitboss
tail`) coexist on one run without displacing each other or the writer.
Op replies moved to the broadcast bus, which unblocks the web SSE
cutover and lands op replies in `events.jsonl` for post-run audit.

The `[communication]` plane now journals every mailbox / artifact
mutation to `<run-dir>/communication/messages.jsonl`; `pitboss resume`
rehydrates the pre-crash mailbox + artifact metadata + per-actor
activity counters so the resumed lead doesn't come back blind (#282).
A new run-wide audit log lands at `<run-dir>/audit.jsonl` (#414) —
every per-actor `TaskEvent` row tee'd into one chronological file with
`actor_id` attribution. Query via `pitboss audit <run-id>` (filters:
`--actor`, `--kind`, `--since`, `--reason-kind`) or `GET
/api/runs/:id/audit` with the same surface as query params.

Highlights:

- **Unified envelope API (#438)** — `open_run_stream(StreamMode::ReplayThenLive)` collapses the per-surface readers in `pitboss-cli`, `pitboss-tui`, and `pitboss-web`. PR-A through PR-Q close the arc.
- **Subscriber-mode handshakes (#438, PR-P)** — `ControlOp::Hello { mode: ClientMode }` lets multiple read-only consumers coexist with one writer on a run's control socket.
- **Communication-plane resume (#282)** — `messages.jsonl` journal + replay in `CommunicationStore::new` so `pitboss resume` doesn't lose pre-crash mailbox state.
- **Aggregated audit log (#414)** — `<run-dir>/audit.jsonl` + `pitboss audit` CLI + `GET /api/runs/:id/audit` web endpoint with `--actor` / `--kind` / `--since` / `--reason-kind` filters.

### Added

- Per-run aggregated audit log + pitboss audit CLI + /api/runs/:id/audit ([#470](https://github.com/SDS-Mode/pitboss/pull/470))
- Persist mailbox + activity to messages.jsonl for resume ([#469](https://github.com/SDS-Mode/pitboss/pull/469))
- Web SSE bridge cutover to unified envelope API (#438, PR-Q) ([#467](https://github.com/SDS-Mode/pitboss/pull/467))
- Subscriber-mode client handshake for multi-consumer streams (#438, PR-P) ([#466](https://github.com/SDS-Mode/pitboss/pull/466))
- Live-socket transport for unified consumer API (#438, PR-N) ([#464](https://github.com/SDS-Mode/pitboss/pull/464))
- SPA Replay tab for persisted events.jsonl (#259, PR-K) ([#454](https://github.com/SDS-Mode/pitboss/pull/454))
- Pitboss events <run-id> prints persisted control-event stream (#259, PR-J) ([#461](https://github.com/SDS-Mode/pitboss/pull/461))
- GET /api/runs/:id/events-jsonl serves persisted envelopes (#259, PR-I) ([#452](https://github.com/SDS-Mode/pitboss/pull/452))
- Persistent events.jsonl bus subscriber (#259, PR-H) ([#451](https://github.com/SDS-Mode/pitboss/pull/451))
- Events.jsonl writer + run-scoped seq counter (#259, PR-G) ([#460](https://github.com/SDS-Mode/pitboss/pull/460))
- Tail_run_stream — continuous disk-tail mode (PR-E of #438) ([#458](https://github.com/SDS-Mode/pitboss/pull/458))
- Live-side cutover to unified consumer (PR-D of #438) ([#457](https://github.com/SDS-Mode/pitboss/pull/457))
- Live + historical unified consumer (PR-C of #438) ([#456](https://github.com/SDS-Mode/pitboss/pull/456))
- Dispatcher-assigned seq on EventEnvelope (PR-B of #438) ([#445](https://github.com/SDS-Mode/pitboss/pull/445))
- Unified run-stream foundation (PR-A of #438) ([#444](https://github.com/SDS-Mode/pitboss/pull/444))
- Notify-driven watcher with periodic safety net ([#441](https://github.com/SDS-Mode/pitboss/pull/441))
- Incremental tail reader for summary.json[l] ([#440](https://github.com/SDS-Mode/pitboss/pull/440))
- Canonical summary.json[l] reader for TUI/web/CLI ([#439](https://github.com/SDS-Mode/pitboss/pull/439))
- Track lead+sublead token spend against budget_usd ([#434](https://github.com/SDS-Mode/pitboss/pull/434))


### Changed

- Route status + diff through unified replay stream (#438, PR-M) ([#463](https://github.com/SDS-Mode/pitboss/pull/463))
- Rewire watcher onto tail_run_stream (PR-F of #438) ([#459](https://github.com/SDS-Mode/pitboss/pull/459))


### Dependencies

- Bump the rust-minor-and-patch group across 1 directory with 4 updates ([#442](https://github.com/SDS-Mode/pitboss/pull/442))
- Bump sha2 from 0.10.9 to 0.11.0 ([#278](https://github.com/SDS-Mode/pitboss/pull/278))


### Fixed

- Show in-progress children in graph inspector ([#468](https://github.com/SDS-Mode/pitboss/pull/468))
- Build pitboss-web SPA before dist build + cargo test ([#436](https://github.com/SDS-Mode/pitboss/pull/436))
- Close out sub-lead's own worker-row on termination ([#432](https://github.com/SDS-Mode/pitboss/pull/432))
- Exclude host user-scope claude settings inside containers ([#430](https://github.com/SDS-Mode/pitboss/pull/430))
- Dedupe summary.jsonl rows by task_id (run-view tabs crash) ([#429](https://github.com/SDS-Mode/pitboss/pull/429))
- Wrap connect timeout in async block (panic on launch) ([#428](https://github.com/SDS-Mode/pitboss/pull/428))
- Pr-gate stops requiring test job on docs-only PRs ([#424](https://github.com/SDS-Mode/pitboss/pull/424))


## [0.13.0] — 2026-05-08

### Added

- Pretty/Raw toggle for stream-json task logs ([#421](https://github.com/SDS-Mode/pitboss/pull/421))
- Tail active task logs inline in graph inspector ([#420](https://github.com/SDS-Mode/pitboss/pull/420))
- Rework Graph tab into post-run actor inspector ([#419](https://github.com/SDS-Mode/pitboss/pull/419))
- Pitboss validate --container skips host-side dir checks ([#410](https://github.com/SDS-Mode/pitboss/pull/410))
- Surface recent tool denials in TUI Detail and pitboss status ([#407](https://github.com/SDS-Mode/pitboss/pull/407))
- Surface per-server tool allowlists in capability matrix UI ([#402](https://github.com/SDS-Mode/pitboss/pull/402))
- Runtime [[mcp_server]].tools allowlist enforcement ([#401](https://github.com/SDS-Mode/pitboss/pull/401))
- Per-server tools allowlist on [[mcp_server]] (#397 slice) ([#400](https://github.com/SDS-Mode/pitboss/pull/400))
- Surface capability matrix on manifest detail page (#391 slice) ([#398](https://github.com/SDS-Mode/pitboss/pull/398))
- Surface capability matrix in Detail view (#391 slice) ([#396](https://github.com/SDS-Mode/pitboss/pull/396))


### Dependencies

- Bump dirs from 5.0.1 to 6.0.0 ([#277](https://github.com/SDS-Mode/pitboss/pull/277))


### Fixed

- Serialize env-touching tests crate-wide via serial_test ([#418](https://github.com/SDS-Mode/pitboss/pull/418))
- Cancelled runs render distinctly in overview and detail ([#409](https://github.com/SDS-Mode/pitboss/pull/409))
- Route breaking-marker commits to dedicated section ([#408](https://github.com/SDS-Mode/pitboss/pull/408))


## [0.12.0] — 2026-05-07

### Breaking changes

- Flip permission_routing default to path_b ([#393](https://github.com/SDS-Mode/pitboss/pull/393))


### Added

- Synthetic-default profile for un-typed Path-B callers ([#392](https://github.com/SDS-Mode/pitboss/pull/392))
- Emit (actor-type → MCP servers) capability matrix ([#390](https://github.com/SDS-Mode/pitboss/pull/390))
- Nudge Path-B operators toward typed profiles ([#389](https://github.com/SDS-Mode/pitboss/pull/389))
- Profile-driven Path-B short-circuit ([#388](https://github.com/SDS-Mode/pitboss/pull/388))
- Surface Path-B tool-denial counter on each actor tile ([#387](https://github.com/SDS-Mode/pitboss/pull/387))
- Surface aggregate approvals counters in run footer ([#386](https://github.com/SDS-Mode/pitboss/pull/386))
- Per-actor MCP scoping + actor_type persistence (#252 Phase 1.5) ([#384](https://github.com/SDS-Mode/pitboss/pull/384))
- Typed worker/sublead profiles with dispatcher caps ([#382](https://github.com/SDS-Mode/pitboss/pull/382))


### Fixed

- Keep substituted webhook tokens out of resolved.json ([#383](https://github.com/SDS-Mode/pitboss/pull/383))


## [0.11.0] — 2026-05-07

### Added

- Add denial_termination_policy with Adapt as default ([#379](https://github.com/SDS-Mode/pitboss/pull/379))
- Re-stabilize Path B permission routing ([#372](https://github.com/SDS-Mode/pitboss/pull/372))
- Reject enum_select without enum_values at macro expansion ([#359](https://github.com/SDS-Mode/pitboss/pull/359))
- Validate sub-lead caps reject 0 and negatives at parse time ([#358](https://github.com/SDS-Mode/pitboss/pull/358))


### Fixed

- Downgrade Path B worker spawn to Path A when mcp_config is None ([#381](https://github.com/SDS-Mode/pitboss/pull/381))
- Attribute default-policy auto-rejection to DeniedByPolicy, not OperatorRejected ([#378](https://github.com/SDS-Mode/pitboss/pull/378))
- Align permission_prompt with Claude Code's PermissionResult wire shape ([#376](https://github.com/SDS-Mode/pitboss/pull/376))
- Write approval audit rows to caller actor's tasks dir ([#375](https://github.com/SDS-Mode/pitboss/pull/375))
- Track approval state on every handle_permission_prompt path ([#374](https://github.com/SDS-Mode/pitboss/pull/374))
- Surface focus-lost notice when active tile auto-falls-back ([#362](https://github.com/SDS-Mode/pitboss/pull/362))
- Surface notify_failures in summary.json and pitboss status ([#361](https://github.com/SDS-Mode/pitboss/pull/361))
- Track total_bytes by delta, not full O(n) recompute ([#357](https://github.com/SDS-Mode/pitboss/pull/357))
- Align diff cost computation with analyze ([#356](https://github.com/SDS-Mode/pitboss/pull/356))
- Narrow artifact_put lock around fs I/O ([#355](https://github.com/SDS-Mode/pitboss/pull/355))
- Make max_parallel_tasks Option<u32> in resolved manifest ([#354](https://github.com/SDS-Mode/pitboss/pull/354))
- Stream task_log instead of slurping the entire file ([#353](https://github.com/SDS-Mode/pitboss/pull/353))
- Clamp approval-TTL age before unsigned cast ([#352](https://github.com/SDS-Mode/pitboss/pull/352))
- Inject PITBOSS_RUN_ID per-spawn instead of mutating parent env ([#350](https://github.com/SDS-Mode/pitboss/pull/350))
- Subscribe-before-try in lease_acquire ([#348](https://github.com/SDS-Mode/pitboss/pull/348))
- Prune worker_layer_index on SpawnFailed and resume paths ([#345](https://github.com/SDS-Mode/pitboss/pull/345))
- Close auth bypass cluster (#309 #310 #311) ([#343](https://github.com/SDS-Mode/pitboss/pull/343))
- Abort kill+resume bridge task at iteration end ([#342](https://github.com/SDS-Mode/pitboss/pull/342))


## [0.10.0] — 2026-05-06

### Added

- Phase C — surface comm activity in TUI + web consoles ([#285](https://github.com/SDS-Mode/pitboss/pull/285))
- Add parent-child mailbox + artifact MCP tools ([#281](https://github.com/SDS-Mode/pitboss/pull/281))
- Add analyze triage command and MCP tools ([#280](https://github.com/SDS-Mode/pitboss/pull/280))


### Dependencies

- Bump the rust-minor-and-patch group across 1 directory with 3 updates ([#288](https://github.com/SDS-Mode/pitboss/pull/288))
- Bump lru from 0.16.4 to 0.18.0 ([#279](https://github.com/SDS-Mode/pitboss/pull/279))


### Fixed

- Make worker/lead/sublead --allowedTools mode-aware for [communication] ([#289](https://github.com/SDS-Mode/pitboss/pull/289))


## [0.9.2] — 2026-04-30

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
