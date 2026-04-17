# Agent Shire v0.1 — Design

**Status:** Approved design, pre-implementation
**Date:** 2026-04-16
**Authors:** TheBigScript, Andy (Opus 4.7, max effort)
**Supersedes:** informal sketch in `angrylarry/Handoff.md` (2026-04-16), elaborates on
`discord_angrylarry/mosaic_spec.md`

---

## 1. Overview

Agent Shire is a Rust CLI that dispatches many concurrent Claude Code agent sessions
("Hobbits") from a single declarative manifest. v0.1 is **headless**: no TUI, no
interactive snap-in, no cross-run persistence. It exists to prove the core runtime —
subprocess lifecycle, stream-json parsing, git-worktree isolation, and summary
reporting — on a substrate that the eventual Mosaic TUI will share.

The binary is `pitboss`. It reads `shire.toml`, validates, fans out N tasks under a
concurrency cap, and writes structured artifacts to `~/.local/share/shire/runs/<run-id>/`.
It is a drop-in tool for scripts, CI, cron, and local experimentation.

## 2. Goals and Non-Goals

### Goals
- Single compiled binary with no runtime dependencies beyond the `claude` CLI.
- Parallel dispatch of N Claude Code sessions from one manifest, with per-task isolation.
- Deterministic, machine-readable output artifacts (`summary.json`, per-task logs).
- Two-phase Ctrl-C: drain cleanly, then escalate on second signal.
- A library (`pitboss-core`) whose public API is designed today to support a future TUI
  consumer without refactoring.

### Non-goals (v0.1)
- **No TUI grid.** Mosaic's visual runner is a later milestone.
- **No session resume across restarts.** SQLite store deferred with the TUI.
- **No hierarchical / lead-worker orchestration.** Needs its own design pass.
- **No task dependency DAG.** `depends_on` is future work; tasks are independent.
- **No prompt files, secrets files, or on_failure hooks.**
- **No pre-run token estimation.** We only report what `claude` tells us post-hoc.
- **No broadcast mode** (same input → multiple sessions).
- **No `shire gc`** for orphaned worktrees. The user owns cleanup when
  `worktree_cleanup = "never"`.

## 3. Workspace Topology

Cargo workspace at repo root `agentshire/`:

```
agentshire/
├── Cargo.toml                    # workspace manifest
├── Cargo.lock
├── rust-toolchain.toml           # pin stable channel (e.g. 1.82)
├── crates/
│   ├── pitboss-core/              # library — shared runtime machinery
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── session/          # SessionHandle, lifecycle state machine
│   │       │   ├── mod.rs
│   │       │   ├── handle.rs
│   │       │   ├── state.rs
│   │       │   └── cancel.rs     # CancelToken (drain / terminate)
│   │       ├── process/          # ProcessSpawner trait + tokio impl
│   │       │   ├── mod.rs
│   │       │   ├── spawner.rs
│   │       │   └── tokio_impl.rs
│   │       ├── parser/           # stream-json line parser
│   │       │   ├── mod.rs
│   │       │   └── events.rs
│   │       ├── worktree/         # git2 worktree lifecycle
│   │       │   ├── mod.rs
│   │       │   └── manager.rs
│   │       ├── store/            # SessionStore trait + JsonFileStore impl
│   │       │   ├── mod.rs
│   │       │   ├── record.rs     # wire types: TaskRecord, RunSummary
│   │       │   └── json_file.rs
│   │       ├── ringbuf/          # bounded output buffer (used by future TUI)
│   │       │   └── mod.rs
│   │       └── error.rs          # thiserror-based error types
│   └── pitboss-cli/                # binary — dispatch orchestrator
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs           # arg parsing, runtime init, signal handling
│           ├── cli.rs            # clap command definitions
│           ├── manifest/         # shire.toml load, validate, resolve
│           │   ├── mod.rs
│           │   ├── schema.rs     # serde types
│           │   ├── resolve.rs    # template/var/defaults merge
│           │   └── validate.rs
│           ├── dispatch/         # runtime orchestration
│           │   ├── mod.rs
│           │   ├── runner.rs     # semaphore, task spawning
│           │   ├── summary.rs    # summary.jsonl → summary.json writer
│           │   └── signals.rs    # two-phase Ctrl-C handler
│           └── tui_table.rs      # stdout progress table (non-interactive)
├── tests-support/
│   └── fake-claude/              # scripted fake `claude` for integration tests
│       ├── Cargo.toml
│       └── src/main.rs
├── tests/                        # workspace-level integration tests
│   └── dispatch_flows.rs
├── docs/
│   └── superpowers/specs/
└── README.md
```

No stubbed `pitboss-tui` crate — it is added when TUI work begins. The workspace
structure above is the only organizational commitment made now.

## 4. Manifest Schema (`shire.toml`)

### 4.1 Full example

```toml
[run]
# Optional; all have defaults listed below.
max_parallel      = 4                            # manifest wins; see precedence in §4.3
halt_on_failure   = false
run_dir           = "~/.local/share/shire/runs"
worktree_cleanup  = "on_success"                 # always | on_success | never
emit_event_stream = false                        # if true, write events.jsonl per task

[defaults]
# Applied to every task unless overridden.
model        = "claude-sonnet-4-6"
effort       = "high"                            # low | medium | high | xhigh | max
tools        = ["Read","Write","Edit","Bash","Glob","Grep"]
timeout_secs = 3600
use_worktree = true
env          = { }                               # merged into subprocess env

[[template]]
id     = "dep-sweep"
prompt = "Audit {package_manager} dependencies in {directory}. Patch + minor only."

[[task]]
id        = "auth-refactor"                      # required, unique within file
directory = "~/projects/myapp"                   # required; expanded per §4.2
prompt    = "Refactor auth to JWT..."            # required unless template set
branch    = "feat/auth-jwt"                      # only honored when use_worktree = true
# Optional per-task overrides:
model        = "claude-opus-4-7"
effort       = "max"
tools        = ["Read","Edit","Bash"]            # replaces defaults.tools (no merge)
timeout_secs = 7200
env          = { NODE_ENV = "test" }             # merged over defaults.env

[[task]]
id        = "npm-sweep"
template  = "dep-sweep"
directory = "~/projects/frontend"
vars      = { package_manager = "npm", directory = "~/projects/frontend" }
branch    = "chore/npm-sweep"
```

### 4.2 Field semantics

| Key | Scope | Default | Notes |
|-----|-------|---------|-------|
| `run.max_parallel` | run | 4 | Semaphore on concurrent tasks. See §4.3 precedence. |
| `run.halt_on_failure` | run | `false` | On first task failure, dispatcher stops granting new permits and broadcasts `CancelToken::drain()`. |
| `run.run_dir` | run | `~/.local/share/shire/runs` | Artifact root; created if missing. |
| `run.worktree_cleanup` | run | `"on_success"` | See §7.3. |
| `run.emit_event_stream` | run | `false` | If `true`, writes parsed `events.jsonl` per task in addition to raw `stdout.log`. |
| `defaults.*` | run | (see above) | Overridden by per-task fields. |
| `task.id` | task | — | **Required.** Drives log path, summary key, worktree name. Unique per manifest. `[a-zA-Z0-9_-]+`. |
| `task.directory` | task | — | **Required.** Path expansion: leading `~` → `$HOME`; `$VAR` and `${VAR}` expanded from shire's env. Must exist and be a directory at validation time. |
| `task.prompt` | task | — | Required unless `task.template` set and template supplies a prompt. |
| `task.template` | task | — | References `[[template]].id`. Template prompt is the final prompt after `{var}` substitution. |
| `task.vars` | task | `{}` | Values substituted into template `{var}` placeholders. Undeclared vars → validation error. |
| `task.branch` | task | — | Honored only when effective `use_worktree = true`. See §7.3. |
| `task.tools` | task | inherits | Override **replaces** `defaults.tools` (not merged). |
| `task.env` | task | merges over defaults | Keys in task `env` win over `defaults.env`. |

### 4.3 Concurrency precedence

Effective `max_parallel = manifest.run.max_parallel ?? ANTHROPIC_MAX_CONCURRENT_env ?? 4`.

Rationale: CPU count is not a proxy for API concurrency. Ceiling is the Anthropic
rate limit for the active API key. Env var lets operators set a machine-wide cap
(shared dev boxes, CI) without editing every manifest. Manifest wins so specific
runs can tune up or down.

### 4.4 Validation

Run at `pitboss validate <file>` and implicitly at `pitboss dispatch <file>` startup.
Fail-fast on any of:

- Missing required fields (`id`, `directory`, `prompt`-or-`template`).
- Duplicate `task.id` values.
- Referenced `template` id not defined.
- Template contains `{var}` not supplied by `task.vars`.
- `directory` does not exist or is not a directory.
- Effective `use_worktree = true` on a directory that is not inside a git work-tree.
- Two tasks both setting `use_worktree = true` with the same `directory` + `branch` pair
  (would race for the same worktree; fail before any spawn).
- Unknown keys at any level — achieved by `#[serde(deny_unknown_fields)]` on every
  manifest struct. Prevents silent typos.
- `effort` not in the enum.
- `timeout_secs <= 0` or `max_parallel <= 0`.

No partial runs — validation is a gate.

### 4.5 Template substitution

Syntax: `{name}` where `name` matches `[a-zA-Z_][a-zA-Z0-9_]*`. Literal `{` is
`\{`, literal `}` is `\}`. No conditionals, no loops, no nested expressions.
Keep the grammar small enough that the parser is obviously correct.

## 5. Runtime Architecture

```
                   pitboss dispatch manifest.toml
                              │
                              ▼
                    ┌─────────────────────┐
                    │ Manifest Loader     │  toml parse → ResolvedManifest
                    │  (manifest/)        │  template resolve, var subst, validate
                    └──────────┬──────────┘
                               │
                               ▼
                    ┌─────────────────────┐
                    │ Dispatcher          │  tokio multi-thread runtime
                    │  (dispatch/runner)  │  Semaphore(max_parallel)
                    │                     │  subscribes to signals::ctrl_c_watcher
                    │                     │  owns DispatchState (Arc<Mutex<_>>)
                    └──────────┬──────────┘
                               │ for each task (bounded by semaphore):
                               ▼
                    ┌──────────────────────────────────────────────┐
                    │ TaskExecutor (one tokio task per Hobbit)     │
                    │                                              │
                    │  1. WorktreeManager::prepare()               │
                    │     git2: worktree add with branch policy    │
                    │                                              │
                    │  2. SessionHandle::spawn()                   │
                    │     via ProcessSpawner trait →               │
                    │     claude --output-format stream-json \     │
                    │            --allowedTools <csv>              │
                    │            --model <id> --effort <level>     │
                    │                                              │
                    │  3. Parser reads stdout line-by-line         │
                    │     serde_json → Event                       │
                    │     state transitions, token accumulator     │
                    │     raw line appended to stdout.log          │
                    │     parsed event optionally → events.jsonl   │
                    │                                              │
                    │  4. On exit / timeout / cancel:              │
                    │     - build TaskRecord                       │
                    │     - SessionStore::append_record (fsync)    │
                    │     - WorktreeManager::cleanup per policy    │
                    │     - release semaphore permit               │
                    └──────────────────────────────────────────────┘
                               │
                               ▼
                    ┌─────────────────────┐
                    │ Summary Writer      │  on clean shutdown:
                    │  (dispatch/summary) │  fold summary.jsonl → summary.json
                    │                     │  on interrupted start-up:
                    │                     │  detect orphan run_dir, assemble
                    │                     │  partial summary with was_interrupted
                    └─────────────────────┘
```

### 5.1 Core types (`pitboss-core`)

Non-exhaustive — signatures are illustrative, subject to normal API hygiene during
implementation.

```rust
pub trait ProcessSpawner: Send + Sync + 'static {
    async fn spawn(&self, cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError>;
}

pub struct SpawnCmd {
    pub program: PathBuf,              // path to `claude` binary
    pub args:    Vec<String>,
    pub cwd:     PathBuf,
    pub env:     HashMap<String, String>,
}

pub trait ChildProcess: Send {
    fn stdout(&mut self) -> Pin<Box<dyn AsyncRead + Send>>;
    fn stderr(&mut self) -> Pin<Box<dyn AsyncRead + Send>>;
    async fn wait(&mut self) -> io::Result<ExitStatus>;
    fn terminate(&mut self);           // SIGTERM
    fn kill(&mut self);                // SIGKILL
    fn pid(&self) -> Option<u32>;
}

pub struct SessionHandle { /* owns child + parser state */ }

impl SessionHandle {
    pub async fn new(
        spawner: Arc<dyn ProcessSpawner>,
        cmd:     SpawnCmd,
        store:   Arc<dyn SessionStore>,
        task_id: TaskId,
    ) -> Result<Self, SessionError>;

    pub async fn run_to_completion(
        &mut self,
        cancel: CancelToken,
        timeout: Duration,
    ) -> SessionOutcome;
}

pub enum SessionState {
    Initializing,
    Running { since: DateTime<Utc> },
    Completed,
    Failed       { message: String },
    TimedOut,
    Cancelled,
    SpawnFailed  { message: String },
}

pub struct SessionOutcome {
    pub final_state:      SessionState,
    pub exit_code:        Option<i32>,
    pub token_usage:      TokenUsage,
    pub claude_session_id: Option<String>,
    pub final_message_preview: Option<String>,
    pub started_at:       DateTime<Utc>,
    pub ended_at:         DateTime<Utc>,
}

pub trait SessionStore: Send + Sync + 'static {
    async fn init_run(&self, run: &RunMeta) -> Result<(), StoreError>;
    async fn append_record(&self, run_id: Uuid, record: TaskRecord) -> Result<(), StoreError>;
    async fn finalize_run(&self, run_id: Uuid, summary: RunSummary) -> Result<(), StoreError>;
    async fn load_run(&self, run_id: Uuid) -> Result<Run, StoreError>;
}

pub struct JsonFileStore { /* writes manifest.snapshot.toml, resolved.json,
                             summary.jsonl (append + fsync), summary.json */ }

pub struct CancelToken {
    drain_tx:     watch::Sender<bool>,
    terminate_tx: watch::Sender<bool>,
}

impl CancelToken {
    pub fn drain(&self);
    pub fn terminate(&self);
    pub fn is_draining(&self) -> bool;
    pub fn is_terminated(&self) -> bool;
    pub async fn await_drain(&mut self);
    pub async fn await_terminate(&mut self);
}
```

### 5.2 Stream-JSON parsing

Claude Code emits newline-delimited JSON on stdout under
`--output-format stream-json`. Canonical event shapes we consume:

```
{"type":"system","subtype":"init",...}
{"type":"assistant","message":{"content":[{"type":"text","text":"..."},
                                          {"type":"tool_use","name":"Write",...}]}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"..."}]}}
{"type":"result","subtype":"success","result":"...","session_id":"sess_abc",
                 "usage":{"input_tokens":...,"output_tokens":...,
                          "cache_read_input_tokens":...,
                          "cache_creation_input_tokens":...}}
```

Parser contract: pure function `parse_line(&[u8]) -> Result<Event, ParseError>`,
no I/O, fully fixture-testable. Tolerance policy, in layers:

1. **Unknown top-level `type`** → emit `Event::Unknown { raw }`, do not fail. Lets
   Claude Code add event kinds without breaking shire.
2. **Unknown fields on known event types** → ignored (serde default, no
   `deny_unknown_fields` on parser wire structs — opposite of the manifest structs).
3. **Structurally invalid known event** (e.g., `{"type":"result"}` with no
   `session_id` or no `usage`) → `Err(ParseError::Malformed)`. Logged, line is
   appended to `stdout.log` verbatim, session is not marked failed on this alone
   (a single malformed line is survivable; lack of any `result` event at exit is not).

Per session we extract:
- `claude_session_id` from the first `result` event.
- `token_usage` accumulator (summed from each `result` event; only one is expected).
- `final_message_preview` = last `text` content from any `assistant` event before `result`.

### 5.3 Cancellation flow

Two signal phases, mapped to a `CancelToken` owned by each `SessionHandle`:

1. **First SIGINT** (or `halt_on_failure` cascade):
   - Dispatcher stops granting new semaphore permits.
   - `CancelToken::drain()` broadcast to running tasks.
   - Tasks keep streaming until subprocess exits naturally or hits `timeout_secs`.
   - Stdout table header changes to `draining…` so the operator knows.

2. **Second SIGINT within 5 s of the first**:
   - `CancelToken::terminate()` broadcast.
   - Each active `SessionHandle` sends SIGTERM to its child.
   - Grace window of `TERMINATE_GRACE = 10 s` (compile-time constant in
     `pitboss-core::session`; not configurable in v0.1), then SIGKILL any still running.
   - Tasks terminated this way record `status = "Cancelled"` with `exit_code = None`.

3. **`halt_on_failure = true`**: first task to end with `status != Success` triggers
   `CancelToken::drain()`. No automatic escalation — the operator can Ctrl-C to
   accelerate.

Summary writing is best-effort: every completed task fsync's its record to
`summary.jsonl` *before* cleanup, so a `kill -9` on `pitboss` itself still leaves
a partial transcript on disk.

## 6. Data Flow and Artifacts

### 6.1 Run directory layout

```
~/.local/share/shire/runs/<run-id>/            # run-id = UUIDv7, sortable by time
├── manifest.snapshot.toml                     # literal copy of input manifest
├── resolved.json                              # post-template/defaults resolved manifest
├── summary.jsonl                              # append-only TaskRecord entries (fsync)
├── summary.json                               # assembled on clean exit; absent → interrupted
└── tasks/
    └── <task-id>/
        ├── stdout.log                         # raw stream-json lines
        ├── stderr.log                         # subprocess stderr (diagnostics only)
        └── events.jsonl                       # optional (run.emit_event_stream = true)
```

### 6.2 Record schemas

**`TaskRecord`** — appended to `summary.jsonl` exactly once per task, at task end:

```json
{
  "task_id": "auth-refactor",
  "status": "Success",
  "exit_code": 0,
  "started_at": "2026-04-16T18:40:00Z",
  "ended_at":   "2026-04-16T18:52:31Z",
  "duration_ms": 751000,
  "worktree_path": "/home/dan/projects/myapp-shire-auth-refactor-01HX...",
  "log_path": "~/.local/share/shire/runs/01HX.../tasks/auth-refactor/stdout.log",
  "token_usage": {
    "input": 4321,
    "output": 8765,
    "cache_read": 12000,
    "cache_creation": 500
  },
  "claude_session_id": "sess_abc123",
  "final_message_preview": "Refactor complete. 14 tests pass, 0 fail..."
}
```

`status` enum: `"Success" | "Failed" | "TimedOut" | "Cancelled" | "SpawnFailed"`.

**`RunSummary`** — written to `summary.json` on clean shutdown (or assembled from
`summary.jsonl` on next invocation if we detect an orphaned, un-finalized run dir):

```json
{
  "run_id": "01HX...",
  "manifest_path": "/abs/path/to/shire.toml",
  "shire_version": "0.1.0",
  "claude_version": "1.0.123",
  "started_at": "2026-04-16T18:40:00Z",
  "ended_at":   "2026-04-16T19:02:11Z",
  "total_duration_ms": 1331000,
  "tasks_total": 5,
  "tasks_failed": 1,
  "was_interrupted": false,
  "tasks": [ /* TaskRecord[] in completion order */ ]
}
```

`claude_version` is a best-effort probe at dispatch start (`claude --version`)
cached into the summary. **Failure modes:**
- Binary not on `PATH` / not executable → dispatch aborts with exit 2 (treated as
  a configuration error; no tasks spawn).
- Binary exits non-zero or produces unparseable output → log a warning, set
  `claude_version = null` in the summary, continue. The binary clearly exists;
  the probe's format is not load-bearing for shire itself.

### 6.3 Stdout UX

A single-line table rendered as tasks complete:

```
TASK                 STATUS      TIME     TOKENS (in/out/cache)   EXIT
auth-refactor        ✓ Success   12m31s   4321 / 8765 / 12k       0
npm-sweep            ● Running   03m12s   —                       —
lint-pass            … Pending   —        —                       —
doc-rewrite          ✗ Failed    00m42s   120 / 88 / 0            2
```

Redraws in place on a TTY; emits append-only lines otherwise (detected via
`atty`). No spinners — they are noise in logs.

## 7. Isolation, Worktrees, and Git Semantics

### 7.1 Worktree physical location

Git-native: the worktree's metadata lives at `<repo>/.git/worktrees/shire-<task-id>-<run-id>/`,
and the working copy sits in a sibling directory named
`<repo-dir>-shire-<task-id>-<run-id>/`. This is exactly what `git worktree add`
does by default and ensures tools that scan `.git/worktrees/` (editors, IDE
plugins) see shire's worktrees as first-class citizens.

### 7.2 Branch policy

- If `task.branch` is set:
  - Branch does not exist → created from `HEAD` of the repo at dispatch start.
    (If the repo's `HEAD` moves while tasks are running, this is fine — each task
    captured its base at worktree-creation time.)
  - Branch exists → worktree checks it out. If the branch is already checked out
    in another worktree (including another concurrent shire task), validation
    fails before any task spawns.
- If `task.branch` is not set: worktree is created in detached-HEAD state from
  `HEAD`. The Hobbit operates freely without a named branch.
- We **never** force-update an existing branch.

### 7.3 Cleanup policy

Controlled by `run.worktree_cleanup`:

| Value | On task success | On task failure |
|-------|-----------------|-----------------|
| `"always"` | Remove worktree (`git worktree remove --force`) | Remove worktree |
| `"on_success"` *(default)* | Remove worktree | Leave worktree for forensics |
| `"never"` | Leave worktree | Leave worktree |

Cleanup runs from the `TaskExecutor`'s final step, after the `TaskRecord` has
been appended. If cleanup itself fails, we log a warning but do not flip the
task's `status` — the Hobbit did its job.

### 7.4 Non-git directories

If a task's effective `use_worktree = true` and the directory is not inside a
git work-tree, **validation fails before any task spawns**. Silent auto-degrade
would violate the isolation contract — two Hobbits running in the same non-git
dir would trample each other. Users who want in-place, un-isolated execution
set `use_worktree = false` explicitly at task or defaults level.

## 8. Error Handling

- **`pitboss-core`** uses `thiserror` for explicit, exhaustive error enums on every
  public boundary (`SessionError`, `SpawnError`, `ParseError`, `WorktreeError`,
  `StoreError`). No `anyhow` in the library.
- **`pitboss-cli`** uses `anyhow` at the CLI entry points; converts library errors
  with `.context(...)` annotations that name the task id and the operation.
- **Exit codes for `pitboss dispatch`**:
  - `0` — all tasks `Success`, no interruption.
  - `1` — one or more tasks ended with `Failed`, `TimedOut`, or `SpawnFailed`.
  - `2` — validation failure (no tasks ran).
  - `130` — interrupted (Ctrl-C, SIGTERM to shire itself).
- Stderr carries human-readable diagnostics; stdout carries the progress table
  and, when `-q/--quiet` is set, nothing.
- `tracing` with `EnvFilter`: default `RUST_LOG=shire=info,pitboss_core=info`,
  pretty output to stderr. `-v` / `-vv` bump verbosity.

## 9. Testing Strategy

Two layers, per Q8 of the brainstorm:

### 9.1 Unit tests (fast, deterministic)

- `parser/` — fixture-driven, `parse_line(bytes) -> Event` over curated samples.
  Fixtures live at `crates/pitboss-core/tests/fixtures/stream_json/*.jsonl`.
- `session/state.rs` — state-transition correctness given synthesized events.
- `manifest/resolve.rs` — template substitution, defaults merge, var validation.
- `manifest/validate.rs` — rejects every documented failure mode.
- `store/json_file.rs` — append/read round-trip, fsync on append, orphan
  detection.
- `session/` with injected `FakeSpawner` implementing `ProcessSpawner` —
  deterministic tests for timeout, drain, terminate, token accumulation.
  `tokio::time::pause()` used to make timing assertions stable.

### 9.2 Integration tests (real spawn path)

- `tests-support/fake-claude/` — a small Rust bin (second workspace member).
  Reads `MOSAIC_FAKE_SCRIPT` pointing at a JSONL fixture of lines to emit
  (with optional `sleep_ms` between lines), exits with `MOSAIC_FAKE_EXIT_CODE`.
- Integration tests at `tests/dispatch_flows.rs` override `claude.binary` to
  the fake binary path and exercise:
  - End-to-end dispatch of 3 tasks with mixed outcomes.
  - `halt_on_failure = true` cascade correctness.
  - Two-phase Ctrl-C (drain, then terminate).
  - `worktree_cleanup` all three policies.
  - Interrupted run → assembled partial `summary.json` on next invocation.
  - Per-task `timeout_secs` kill path.
- Real-git tests use `tempfile::TempDir` + `git2` to build throwaway repos.

### 9.3 What we do not test automatically
- The **real** `claude` binary (requires API credentials, money, network).
  Manual smoke test is documented in `README.md` as the final acceptance step
  for each release tag.

## 10. Dependencies

From the spec, pruned to what v0.1 actually needs:

```toml
# crates/pitboss-core/Cargo.toml
[dependencies]
tokio              = { version = "1", features = ["rt-multi-thread","process","io-util","sync","time","signal","macros","fs"] }
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
uuid               = { version = "1", features = ["v7","serde"] }
chrono             = { version = "0.4", features = ["serde"] }
git2               = "0.19"
thiserror          = "1"
tracing            = "0.1"
async-trait        = "0.1"

[dev-dependencies]
tempfile           = "3"
tokio              = { version = "1", features = ["test-util"] }
```

```toml
# crates/pitboss-cli/Cargo.toml
[dependencies]
pitboss-core        = { path = "../pitboss-core" }
tokio              = { version = "1", features = ["rt-multi-thread","signal","macros"] }
clap               = { version = "4", features = ["derive"] }
toml               = "0.8"
serde              = { version = "1", features = ["derive"] }
serde_json         = "1"
anyhow             = "1"
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter","fmt"] }
shellexpand        = "3"                          # ~ and $VAR expansion
atty               = "0.2"                        # TTY detection for table UX
```

Deferred deps (land with the TUI or SQLite milestones): `ratatui`, `crossterm`,
`rusqlite`.

## 11. Out of Scope for v0.1 (explicit ledger)

Captured so future contributors know these were considered and intentionally
deferred, not missed:

- Mosaic TUI grid (ratatui/crossterm) — next milestone.
- Session resume (`claude --resume`) — requires `SqliteStore`.
- `SqliteStore: SessionStore` — added when TUI needs cross-restart state.
- Hierarchical lead/worker orchestration — needs its own design doc covering
  the control channel (MCP server? IPC? tool callback?).
- Task dependency DAG (`depends_on`).
- `prompt_file`, `secrets_file`, `on_failure` hooks.
- Pre-run token usage estimation.
- `shire gc` for orphaned worktrees from `worktree_cleanup = "never"` runs.
- Broadcast mode.
- Multi-machine distribution.

## 12. Open Questions

None at design time. Any ambiguity encountered during implementation is handled
by adding to this section in a follow-up PR.

---

## Appendix A — Command Surface

```
pitboss validate <manifest.toml>
    Parse + resolve + validate. Prints validation report. Exit 0 if clean.

pitboss dispatch <manifest.toml> [--run-dir PATH] [--dry-run] [-q|-v|-vv]
    Execute the manifest. --dry-run prints the resolved spawn commands and exits.

pitboss version
    Prints pitboss version and, if available, claude version.
```

## Appendix B — First-PR Sequence (informal, for writing-plans)

This is a hint, not a plan. The writing-plans skill will produce the authoritative
task-level plan.

1. Workspace skeleton, Cargo.toml files, rust-toolchain pin. CI configuration
   optional; land `cargo fmt`/`cargo clippy` pre-commit parity at minimum.
2. `pitboss-core::parser` with fixtures — lowest-risk, purely functional.
3. `pitboss-core::process::ProcessSpawner` trait + tokio impl + `FakeSpawner`.
4. `pitboss-core::session` state machine against `FakeSpawner`.
5. `pitboss-core::worktree` with real-git `TempDir` tests.
6. `pitboss-core::store::JsonFileStore` with append/read round-trip tests.
7. `pitboss-cli::manifest` load/resolve/validate.
8. `pitboss-cli::dispatch::runner` — semaphore, task lifecycle, summary writing.
9. `pitboss-cli::dispatch::signals` — two-phase Ctrl-C.
10. `tests-support/fake-claude` + integration tests.
11. Stdout progress table polish.
12. Manual smoke test against real `claude`.
