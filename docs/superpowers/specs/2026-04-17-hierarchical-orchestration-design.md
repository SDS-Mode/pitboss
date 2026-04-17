# Agent Shire v0.3 — Hierarchical Orchestration (Design)

**Status:** approved design, pre-implementation
**Date:** 2026-04-17
**Authors:** TheBigScript, Andy (Opus 4.7, max effort)
**Prerequisites:** v0.2.2 landed and tagged (Mosaic TUI, SQLite store, `pitboss resume`, `pitboss diff`).

---

## 1. Overview

v0.3 adds a hierarchical dispatch mode where one "lead" Hobbit receives a
high-level prompt, spawns "worker" Hobbits dynamically at runtime via MCP tool
calls, and coordinates them until the goal is done. The operator writes one
instruction; the lead writes the worker prompts.

Goal is a usable v0.3.0 shipping in one to two weeks of implementation work, not
a research project. Everything expensive about a multi-agent framework — agent
spawning, worktree isolation, subprocess lifecycle, stream-json parsing,
cost/token tracking, observation, persistence, resume — already exists from
v0.1/v0.2 and is reused unchanged. The new surface is (a) a local MCP server
the lead talks to, (b) a manifest section for hierarchical runs, (c) guardrails
for recursion, worker count, and budget, and (d) minor TUI tweaks for lineage.

### 1.1 Goals

- One-prompt operator workflow: write `lead_prompt`, run `pitboss dispatch`.
- Lead can spawn workers dynamically in parallel (no pre-declared worker list).
- Every worker gets the same worktree isolation, session tracking, cost
  accounting, and Mosaic visibility that flat-dispatch tasks get.
- Hard caps on worker count and budget to bound cost.
- No new infrastructure for things already built — MCP is the only new surface.

### 1.2 Non-goals (v0.3)

- **Depth > 1.** Workers cannot spawn sub-workers. Matches Claude Code Agent
  Teams, Claude Code subagents, Agent-MCP, and every other hierarchical
  framework surveyed.
- **Worker-to-worker messaging.** Pure hub-and-spoke; workers only receive
  their spawn prompt and report back to the lead.
- **Plan approval workflow** (teammate submits a plan → lead approves before
  implementation). Nice-to-have later; out of v0.3.
- **Hooks** (`TeammateIdle`, `TaskCreated`, `TaskCompleted`) — defer to v0.3.1.
- **Multi-lead manifests.** Exactly one lead per run.
- **Broadcast / parallel-forward-message** patterns.

## 2. Research evidence

The control-channel decision (MCP server) is informed by surveying how
high-usage multi-agent frameworks handle lead↔worker coordination. Tool-call
delegation is the convergent pattern. No major framework uses prompt-string
parsing or shell-CLI hacks as the primary control surface.

| Framework | Control surface | Dynamic spawn? | Cap |
|---|---|---|---|
| LangGraph supervisor | Tool calls (`handoff_to`, `forward_message`) — *official recommendation* | No (graph nodes) | — |
| CrewAI | Manager's `delegate_work` tool | No (pre-registered crew) | — |
| Claude Code Agent Teams (experimental) | Built-in `SendMessage`, task-list tools | Yes, via team lead | 3–5 recommended |
| Agent-MCP | MCP: `create_agent`, `terminate_agent`, `assign_task`, `send_agent_message` | Yes | Max 10 active |
| Swarmify Agents MCP | MCP: `Spawn`, `Status`, `Stop`, `Tasks` | Yes | — |
| OpenHands | `AgentDelegateAction` in event stream | Yes | — |
| Claude Code Task tool (subagents) | Built-in tool | Yes | — |

**All bounded systems explicitly forbid nested recursion.** Agent Teams says it
outright ("Teammates cannot spawn their own teams"). Claude Code subagents say
the same. Depth = 1 is the near-universal safe default.

The Rust MCP SDK (`rmcp`, 4.7M downloads) provides server scaffolding with
`#[tool]`-annotated functions auto-generating JSON schemas; standing up a shire
MCP server is ~1 day of work, not a new protocol implementation.

We explicitly considered and rejected:

- **Using Claude Code Agent Teams directly.** Built-in, free. But Agent Teams
  provides no worktree isolation — teammates share the repo. That's shire's
  headline v0.1 feature; we'd throw it away. Also depends on an experimental
  feature flag and an API that may change.
- **Prompt-string parsing.** Ship a magic string the lead emits. Fragile: claude
  rewrites and summarizes its own output; no major framework uses this.
- **Shell-CLI spawning.** Lead calls `shire spawn-worker ...` via Bash. Works,
  but claude is trained to call tools, not memorize CLI syntax. Unreliable.

## 3. Manifest

A new top-level `[[lead]]` section expresses hierarchical mode. A manifest must
have **either** `[[task]]` entries (flat dispatch, v0.1 behavior, unchanged)
**or** exactly one `[[lead]]` entry (hierarchical, v0.3). Mixing them is a
validation error.

### 3.1 New `[run]` fields

```toml
[run]
# existing fields (all v0.1/v0.2):
max_parallel     = 5
halt_on_failure  = false
run_dir          = "~/.local/share/shire/runs"
worktree_cleanup = "on_success"

# NEW in v0.3, only meaningful in hierarchical mode:
max_workers       = 5         # hard cap on concurrent workers the lead can have running
budget_usd        = 10.00     # hard ceiling on total run cost (USD)
lead_timeout_secs = 3600      # wall-clock timeout for the lead process (default 3600 = 1h)
```

Validation rules:

- `max_workers`, `budget_usd`, `lead_timeout_secs` are **only** permitted when a
  `[[lead]]` section is present. Setting them in a flat-task manifest is an
  error (prevents silent misreading of intent).
- `max_workers` must be ≥ 1 and ≤ 16. The upper bound is a compile-time guard
  against runaway fan-out; it's well above the industry-recommended 3–5 and
  Agent-MCP's 10, leaving headroom without enabling accidents.
- `budget_usd` must be > 0. No upper limit.
- `lead_timeout_secs` must be > 0.

### 3.2 `[[lead]]` shape

```toml
[[lead]]
id        = "triage"
directory = "~/projects/foo"       # required; lead's working directory (same rules as task.directory)
branch    = "feat/v2-triage"       # optional; worktree branch for the lead itself
prompt    = """
Review all open issues labeled 'v2.0'. Triage by complexity.
Use shire__spawn_worker to delegate investigation of complex ones.
Use shire__wait_for_any to collect results as they arrive.
When done, summarize your findings.
"""

# Optional per-lead overrides (inherit from [defaults] otherwise):
model        = "claude-sonnet-4-6"
effort       = "max"
tools        = ["Read","Write","Edit","Bash","Glob","Grep"]
timeout_secs = 3600                 # falls back to [run].lead_timeout_secs if unset
env          = {}
```

A lead has the exact same shape as a task, plus it implicitly receives the six
MCP tools described in §4. Workers do not receive those tools.

Validation rules beyond task parity:

- Exactly one `[[lead]]` entry.
- `id` and `directory` required.
- `prompt` required.
- `use_worktree` defaults to `true` (same as tasks).

### 3.3 Templates for the lead

`[[template]]` + `vars` work for a lead identically to how they work for tasks
(substitute `{var}` placeholders in the prompt). No special case.

## 4. MCP Server

Shire runs a local MCP server on a unix socket for the duration of the
hierarchical run. The lead's `claude` subprocess is launched with
`--mcp-config <path-to-config.json>` where the config file describes a single
server pointing at the socket. Workers get no MCP config — they're one-shot
`claude -p` processes exactly like in flat dispatch, which enforces depth=1
structurally (workers literally cannot call these tools).

### 4.1 Transport and lifecycle

- **Socket location:** `$XDG_RUNTIME_DIR/shire/<run-id>.sock`, falling back to
  `<run_dir>/<run-id>/mcp.sock` if `$XDG_RUNTIME_DIR` is unset or non-writable.
- **Start:** before the lead's `SessionHandle::spawn` call. The MCP server must
  be listening when claude's MCP client connects.
- **Shutdown:** after the lead exits AND all workers have drained. Socket file
  cleaned up in the same pass that removes worktrees on `on_success` policy.
- **Implementation:** `rmcp` crate, `ServerHandler` trait impl over an
  `Arc<DispatchState>` shared with the runner. Tools are async, implemented
  with `#[tool]` macro on the impl block; `ToolRouter` dispatches.

### 4.2 Tool surface

Six tools, all namespaced `shire__*` in the MCP manifest (the prefix is
configurable in `--mcp-config` if conflicts arise; default matches shire's
name):

```rust
// Pseudocode showing the intent; real signatures generated by #[tool].

/// Spawn a worker Hobbit. Returns immediately with a task_id; the worker runs
/// asynchronously. Call `wait_for_worker` or `wait_for_any` to observe results.
async fn spawn_worker(
    prompt: String,
    // Optional overrides; if omitted, inherit from [defaults] or the lead:
    directory: Option<String>,
    branch: Option<String>,
    tools: Option<Vec<String>>,
    timeout_secs: Option<u64>,
    model: Option<String>,
) -> Result<SpawnWorkerResult, McpError>;

struct SpawnWorkerResult {
    task_id: String,             // "worker-<short-uuid>"
    worktree_path: Option<String>,
}

/// Non-blocking status poll. Returns current state plus any partial data
/// captured so far from the worker's stream.
async fn worker_status(task_id: String) -> Result<WorkerStatus, McpError>;

struct WorkerStatus {
    state: String,               // "Pending" | "Running" | "Completed" | "Failed" | "TimedOut" | "Cancelled" | "SpawnFailed"
    started_at: Option<String>,  // RFC3339
    partial_usage: TokenUsage,   // best-effort; zero until a result event
    last_text_preview: Option<String>,
}

/// Block until the worker exits or the timeout elapses. Returns the full outcome.
async fn wait_for_worker(
    task_id: String,
    timeout_secs: Option<u64>,
) -> Result<TaskOutcome, McpError>;

/// Block until any of the listed workers exits. Useful for fan-in patterns.
async fn wait_for_any(
    task_ids: Vec<String>,
    timeout_secs: Option<u64>,
) -> Result<(String, TaskOutcome), McpError>;

/// Enumerate the current run's workers (does not include the lead itself).
async fn list_workers() -> Result<Vec<WorkerSummary>, McpError>;

struct WorkerSummary {
    task_id: String,
    state: String,
    prompt_preview: String,      // first 80 chars of the worker's prompt
    started_at: Option<String>,
}

/// Terminate a worker. Sends SIGTERM via the existing CancelToken.
async fn cancel_worker(task_id: String) -> Result<CancelResult, McpError>;
```

### 4.3 Guardrail enforcement

All checks happen inside `spawn_worker` before any worktree creation or
subprocess start.

1. **Worker cap.** Count current workers with state in `{Pending, Running}`; if
   ≥ `[run].max_workers`, return `McpError::Custom("worker cap reached: ...")`.
2. **Depth = 1.** Not enforced in tool code; enforced structurally by not
   passing `--mcp-config` to workers. Documented invariant: workers have no
   MCP surface.
3. **Budget.** Sum `cost_usd` across all `TaskRecord`s in this run (lead +
   workers) that have `status != Pending`, plus an estimated cost for the new
   worker. Estimate = median of this-run's prior worker costs; if no worker has
   finished yet, fall back to a compile-time constant (`INITIAL_WORKER_COST_EST
   = 0.10`). If sum ≥ `budget_usd`, return
   `McpError::Custom("budget exceeded: $X.XX of $Y.YY used")`.
4. **Run draining.** If the dispatcher is in drain mode (Ctrl-C phase 1 or
   `halt_on_failure` cascade fired), refuse new spawns.

The lead sees errors as structured `tool_result` content. It can continue with
whatever it has or call `wait_for_any` on already-spawned workers.

## 5. Runtime Flow

```
 operator
    │  pitboss dispatch manifest.toml
    │
    ▼
┌───────────────────────────────────────┐
│ Dispatcher (existing runner, extended) │
│                                        │
│ - Detect hierarchical mode (manifest   │
│   has [[lead]] instead of [[task]])    │
│ - Create run_dir; write manifest       │
│   snapshot + resolved.json             │
│ - Start MCP server on unix socket      │
│ - Render lead tile in Mosaic (bold)    │
│ - Prepare lead's worktree              │
│ - Spawn lead via SessionHandle with    │
│   claude args: --mcp-config <...>      │
└───────────────────────────┬────────────┘
                            │
                            ▼
┌───────────────────────────────────────┐
│ Lead (claude CLI subprocess)           │
│ --output-format stream-json            │
│ --verbose                              │
│ -p <lead_prompt>                       │
│ --mcp-config <socket>                  │
│                                        │
│ tool_use: spawn_worker(prompt=..., ...)│ ───► MCP ──► spawn_worker handler
│                                        │                │
│ tool_result: {task_id: "worker-ab12"}  │ ◄── MCP ◄──    ├──► semaphore permit
│                                        │                ├──► WorktreeManager::prepare
│ tool_use: spawn_worker(prompt=..., ...)│                ├──► SessionHandle::spawn (NO MCP config)
│ ...                                    │                │     (identical to flat task path)
│                                        │                └──► returns immediately
│ tool_use: wait_for_any([ids...])       │ ───► MCP ──► block on CancelToken/exit
│ tool_result: {task_id, outcome}        │ ◄── MCP ◄──
│                                        │
│ ... (more spawn / wait cycles) ...     │
│                                        │
│ assistant: final summary text          │
│ result event                           │
│ exits                                  │
└───────────────────────────┬────────────┘
                            │
                            ▼
┌───────────────────────────────────────┐
│ Dispatcher finalization                 │
│                                        │
│ - In-flight workers: SIGTERM + grace + │
│   SIGKILL; recorded as Cancelled       │
│ - summary.json assembled with lead +   │
│   all workers as TaskRecord entries    │
│ - MCP server shut down; socket unlink  │
│ - Worktree cleanup per policy          │
│ - Exit code per v0.1/§7 rules          │
└────────────────────────────────────────┘
```

The workers in the middle go through the exact v0.1 dispatch path — same
`execute_task`, same `SessionHandle`, same `WorktreeManager`, same `TaskRecord`
write to store, same Mosaic tile. The only difference at the worker level is
`parent_task_id = Some("<lead-id>")` on the stored `TaskRecord`.

## 6. Data Model Additions

### 6.1 `TaskRecord`

```rust
pub struct TaskRecord {
    // ... all existing fields ...
    #[serde(default)]
    pub parent_task_id: Option<String>,   // NEW — None for flat tasks + the lead
}
```

`#[serde(default)]` makes old JSON records deserialize cleanly.

### 6.2 `JsonFileStore`

No schema change. The field is just serialized into summary.jsonl and
summary.json.

### 6.3 `SqliteStore`

One new column on `task_records`:

```sql
ALTER TABLE task_records ADD COLUMN parent_task_id TEXT NULL;
```

Migration: `CREATE TABLE IF NOT EXISTS` path handles new DBs; existing DBs get
the column added the first time a v0.3 shire opens them (simple ALTER TABLE
idempotent via `PRAGMA table_info` check).

### 6.4 `ResolvedManifest`

```rust
pub struct ResolvedManifest {
    // ... existing run-level fields ...

    // Either `tasks` (flat) or `lead` (hierarchical), never both:
    pub tasks: Vec<ResolvedTask>,         // empty in hierarchical mode
    pub lead:  Option<ResolvedLead>,      // None in flat mode

    // NEW hierarchical-only config (None in flat mode):
    pub max_workers:       Option<u32>,
    pub budget_usd:        Option<f64>,
    pub lead_timeout_secs: Option<u64>,
}

pub struct ResolvedLead {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub branch: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub tools: Vec<String>,
    pub timeout_secs: u64,
    pub use_worktree: bool,
    pub env: HashMap<String, String>,
}
```

## 7. Error handling, cancellation, budget semantics

Directly from the approved policy table:

| Event | Behavior |
|---|---|
| Worker fails | Lead sees `status: "Failed"` in `wait_for_worker` tool_result; lead decides. `[run].halt_on_failure = true` opts into dispatcher auto-drain on first worker failure. |
| `spawn_worker` returns error | Lead receives error in tool_result; lead continues. Errors: `"worker cap reached"`, `"budget exceeded"`, `"run is draining"`. |
| Budget exceeded | `spawn_worker` refuses new calls. In-flight workers finish naturally (they've already committed cost). |
| Lead exits before workers | All in-flight workers get SIGTERM → TERMINATE_GRACE → SIGKILL. Status `Cancelled`. Records persisted to summary.jsonl. |
| First Ctrl-C | Drain: `spawn_worker` refuses; lead keeps running; workers complete or hit their own timeout. |
| Second Ctrl-C within 5 s | Terminate: lead + all workers receive SIGTERM. |
| Per-worker timeout | Honors `timeout_secs` on `spawn_worker` call, else `[defaults].timeout_secs`, else compile-time default 3600. |
| Lead timeout | `[run].lead_timeout_secs`; on fire, lead gets SIGTERM + grace + SIGKILL, same as a normal session timeout. |

## 8. Mosaic TUI changes

No new modes. Tile grid shows lead + all workers as tiles. Small UX touches:

- **Lead tile:** rendered first (position 0, top-left). Title bar reads
  `[LEAD] <id>` in bold. Block border uses a distinct color (cyan) from
  regular worker tiles.
- **Worker tiles:** subtitle line under the tile id reads `← <lead-id>` so
  lineage is visible at a glance.
- **Top status bar:** adds `<N> workers spawned` alongside the existing
  `<done>/<total>` counter. For a hierarchical run, "total" equals lead (1) +
  spawned workers; grows as the lead spawns.
- **Focus pane for lead:** when the lead tile is focused, tool_use events for
  `shire__spawn_worker` and friends render specially — `> spawn_worker →
  worker-ab12` with the target task_id highlighted and clickable-in-spirit
  (press `G` then worker id in a future version; v0.3 just shows the mapping).
- **Run picker:** unchanged. Hierarchical runs look like other runs with an
  `N` task count that may grow during observation.

No new keybindings. No new widgets. This is deliberately the minimum change —
if the hierarchical UI needs to be richer (tree view, nested collapse), that's
v0.3.1.

## 9. Resume, diff, validate

- **`pitboss validate`** extends to accept `[[lead]]` manifests. Rejects mixed
  `[[task]]` + `[[lead]]`, reports budget/cap/timeout out-of-range.
- **`pitboss dispatch`** detects hierarchical mode when the resolved manifest has
  `lead.is_some()` and routes to a new `run_hierarchical` internal function.
  The existing flat `execute` is unchanged; they share sub-helpers.
- **`pitboss resume`** re-runs the lead with `--resume <lead_session_id>` from
  the prior run's `TaskRecord`. Workers are NOT resumed — the lead decides
  whether to spawn fresh workers based on its context. Documentation says so
  explicitly; attempting to resume a specific worker by id errors.
- **`pitboss diff`** works out of the box — leads and workers are `TaskRecord`
  entries. The diff output is grouped by run, so a lead vs lead diff naturally
  pairs up.

## 10. Testing

### 10.1 Unit tests

- `manifest::schema` — `[[lead]]` parses; `[[lead]] + [[task]]` rejects;
  `max_workers`, `budget_usd`, `lead_timeout_secs` without a `[[lead]]` rejects.
- `manifest::resolve` — lead inherits `[defaults]`, per-lead fields override.
- `mcp_server::tools` — each tool's handler with `FakeSpawner`:
  - `spawn_worker` happy path creates a `TaskRecord` with `parent_task_id` set.
  - `spawn_worker` over `max_workers` returns the cap error.
  - `spawn_worker` over `budget_usd` returns the budget error.
  - `wait_for_worker` blocks until the fake child exits.
  - `wait_for_any` returns the first of N.
  - `cancel_worker` sets state to Cancelled.
  - `list_workers` enumerates correctly.
- `dispatch::hierarchical::finalize` — lead-exit triggers worker cancellation.

### 10.2 Integration tests

- `tests-support/fake-claude`: extend the JSONL scripting to support emitting
  `tool_use` / `tool_result` events. Not in v0.1 because flat dispatch didn't
  need it. **Implementation risk:** claude's real MCP client does stateful
  handshaking (init / tools/list / tools/call). The fake binary either (a)
  links against `rmcp` as a client to do the handshake for real against
  shire's server, or (b) emits a pre-baked transcript that mocks the real
  conversation. (a) is cleaner; (b) is simpler. Plan tasks should pick one
  explicitly before starting integration test work.
- `tests-support/fake-mcp-client` (new): a small helper crate that speaks
  MCP-over-unix-socket and lets an integration test act as a fake lead. Invokes
  tools on the shire MCP server and asserts on results.
- Integration tests:
  - End-to-end hierarchical run with one lead spawning two workers, all fake.
  - `max_workers` cascade: lead tries to spawn 3rd worker, gets cap error,
    continues.
  - `budget_usd` cascade: lead spawns workers until budget hit, gets error,
    continues.
  - Lead exits while 2 workers in-flight → both recorded as Cancelled.
  - Ctrl-C drain while lead is mid-spawn.

### 10.3 Smoke test

Update `docs/v0.1-smoke-test.md` (or write `docs/v0.3-smoke-test.md`) with a
real-claude hierarchical test: one lead, manifest prompt instructs it to spawn
3 workers on trivial prompts and summarize. Verify costs stay under a small
budget; verify Mosaic shows lead + workers with lineage annotations.

## 11. Crate layout impact

```
crates/
├── pitboss-core/          # +1 field on TaskRecord, +1 SQLite column
├── pitboss-tui/           # +lead/worker annotations in tile render
├── pitboss-cli/            # NEW: src/mcp/    — MCP server + tool handlers
│                         # NEW: src/dispatch/hierarchical.rs
│                         # EXT: src/manifest/schema.rs — [[lead]] section
│                         # EXT: src/manifest/validate.rs — hierarchical rules
│                         # EXT: src/main.rs — dispatch mode detection
├── tests-support/
│   ├── fake-claude/      # Extended to emit tool_use events
│   └── fake-mcp-client/  # NEW — lets integration tests drive the shire MCP
```

New workspace dependency: `rmcp = "0.8"` with `server`, `transport-io`, and
`macros` features.

## 12. Non-goals (explicit ledger)

Deliberately deferred so they aren't mistaken for v0.3 work:

- Depth > 1 recursion (workers spawning sub-workers).
- Worker-to-worker messaging (peer coordination).
- `TeammateIdle` / `TaskCreated` / `TaskCompleted` hooks.
- Plan approval workflow.
- Multi-lead manifests.
- Per-worker resume via `pitboss resume`.
- Broadcast / parallel-forward-message patterns.
- MCP server over anything but unix sockets (TCP, named pipes, etc.).
- A GUI tree view of the run's worker lineage in Mosaic.

Each of these is its own design increment. This spec stays focused on the
single-lead, depth-1, hub-and-spoke case.

## 13. Estimated implementation effort

Against the 1–2 week ceiling:

| Piece | Days |
|---|---|
| MCP server scaffolding + 6 tool handlers | 2–3 |
| `[[lead]]` manifest section + validation | 0.5 |
| `run_hierarchical` dispatch path (reuses 90% of flat runner) | 1–2 |
| `parent_task_id` plumbing through stores + records | 1 |
| Mosaic UI tweaks (lead tile + worker annotations + status bar) | 1 |
| Testing infrastructure (fake-claude tool events, fake-mcp-client) | 2 |
| Integration tests (5 scenarios from §10.2) | 1 |
| Manual smoke + budget/cap edge cases + docs | 1–2 |
| **Total** | **~9–12 days** |

Fits inside the 2-week ceiling with margin for unplanned discoveries.

## 14. Open questions

None at design time. Any ambiguity encountered during implementation gets
added in a follow-up PR to this spec.

---

## Appendix A — Example hierarchical manifest

```toml
# triage-v2.toml
#
# Lead reviews all open 'v2.0'-labeled issues, picks 3-5 complex ones,
# dispatches worker hobbits to investigate each, synthesizes findings.

[run]
max_parallel       = 5
max_workers        = 5
budget_usd         = 8.00
lead_timeout_secs  = 1800

[defaults]
model        = "claude-haiku-4-5"
tools        = ["Read","Write","Edit","Bash","Glob","Grep"]
timeout_secs = 300
use_worktree = true

[[lead]]
id        = "triage"
directory = "~/projects/myapp"
branch    = "feat/v2-triage"
model     = "claude-sonnet-4-6"
prompt    = """
Review all open issues in this repo labeled 'v2.0'. For each complex
issue, spawn a worker using shire__spawn_worker with a focused
investigation prompt. Use shire__wait_for_any to gather results as
workers finish. When all complex issues have been investigated,
produce a triage summary at TRIAGE.md ranking them by complexity
and effort estimate.

Budget: $8. Max concurrent workers: 5. Don't spawn more than 10
workers total.
"""
```

## Appendix B — Example MCP tool transcript (abbreviated)

```
lead → shire__spawn_worker(
    prompt = "Investigate issue #142 about websocket reconnect. Summarize root cause, affected files, complexity estimate.",
    directory = "~/projects/myapp",
    branch = "investigate/142",
    timeout_secs = 300
)
shire → {task_id: "worker-019d9b...", worktree_path: "/home/dan/projects/myapp-shire-worker-019d9b..."}

lead → shire__spawn_worker(prompt = "Investigate issue #187 about memory leak.", ...)
shire → {task_id: "worker-019d9c...", worktree_path: "..."}

lead → shire__spawn_worker(prompt = "Investigate issue #201 about auth edge case.", ...)
shire → {task_id: "worker-019d9d...", worktree_path: "..."}

lead → shire__wait_for_any(
    task_ids = ["worker-019d9b...", "worker-019d9c...", "worker-019d9d..."],
    timeout_secs = 600
)
shire → {task_id: "worker-019d9c...", outcome: {
    status: "Completed",
    exit_code: 0,
    token_usage: {input: 32, output: 1204, cache_read: 18, cache_creation: 84129},
    final_message_preview: "Root cause: event listener never removed when component unmounts..."
}}

lead → shire__wait_for_any(task_ids = ["worker-019d9b...", "worker-019d9d..."])
shire → {task_id: "worker-019d9b...", outcome: {...}}

lead → shire__wait_for_any(task_ids = ["worker-019d9d..."])
shire → {task_id: "worker-019d9d...", outcome: {...}}

lead writes TRIAGE.md synthesizing the three findings.
lead → result event, exits.
```
