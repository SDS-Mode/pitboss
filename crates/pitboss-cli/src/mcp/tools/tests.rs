//! Unit tests for the `tools/` submodules. Kept in one file so the
//! shared `test_state` / `completing_test_state` / `mk_plan_state` /
//! `register_test_sublead` / `index_worker` builders can be reused
//! across spawn / lifecycle / approval / wait coverage without
//! duplication.

use std::sync::Arc;

use super::spawn::{initial_estimate_for, worker_spawn_args};
use super::*;
use crate::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};

#[test]
fn worker_spawn_args_has_plugin_isolation_flags() {
    // Parallel to the runner-side
    // `every_spawn_variant_has_plugin_isolation_flags` test.
    // worker_spawn_args is emitted from this module, so it needs its
    // own canary for task #67 (plugin isolation). Without both flags,
    // the operator's superpowers / other `~/.claude/` plugins bleed
    // into worker claude subprocesses and cause the Skill-trap bug.
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        Default::default(),
        crate::manifest::schema::CommunicationMode::Disabled,
        &[],
    );
    assert!(
        argv.iter().any(|a| a == "--strict-mcp-config"),
        "worker_spawn_args missing --strict-mcp-config: {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == "--disable-slash-commands"),
        "worker_spawn_args missing --disable-slash-commands: {argv:?}"
    );
}

async fn test_state() -> Arc<DispatchState> {
    test_state_with_budget(5.0).await
}

async fn test_state_with_budget(budget: f64) -> Arc<DispatchState> {
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    // Minimal lead that turns off worktree prep so the background worker
    // spawn path doesn't require a real git repo to run against.
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "lead prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: Default::default(),
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(budget),
        lead_timeout_secs: None,
        default_approval_policy: None,
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let run_id = Uuid::now_v7();
    // Use a FakeSpawner that holds its children open until terminated.
    // This keeps spawned workers in the Running state throughout the test
    // (rather than transitioning to Done quickly as TokioSpawner + /bin/true
    // would), which keeps the `active_worker_count()` guard deterministic.
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    // Leak the TempDir — the state holds paths into it and the test
    // may spawn background workers that write logs inside it.
    let dir_path = dir.path().to_path_buf();
    std::mem::forget(dir);
    let _ = dir_path;
    Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ))
}

#[test]
fn worker_spawn_args_passes_dangerously_skip_permissions_under_path_a() {
    // Companion to the runner-side test: worker spawns are emitted from
    // a different module and must independently pass the flag under
    // Path A. Pinned to Path A explicitly because v0.12 flipped the
    // default to Path B, which deliberately omits
    // `--dangerously-skip-permissions` (claude's gate routes through
    // `mcp__pitboss__permission_prompt`). The Path-B path is covered
    // by `path_b_worker_emits_permission_prompt_tool` below.
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        PermissionRouting::PathA,
        CommunicationMode::Disabled,
        &[],
    );
    assert!(
        argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "Path-A worker_spawn_args missing --dangerously-skip-permissions: {argv:?}"
    );
}

#[test]
fn path_b_worker_emits_permission_prompt_tool() {
    // Path B counterpart to `worker_spawn_args_passes_dangerously_skip_permissions`.
    // When `permission_routing = "path_b"`, the worker must NOT carry
    // `--dangerously-skip-permissions` and MUST carry
    // `--permission-prompt-tool mcp__pitboss__permission_prompt`. Without
    // the flag claude falls back to its interactive gate — which can't
    // be answered under `-p` — and the worker silently stalls.
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        PermissionRouting::PathB,
        CommunicationMode::Disabled,
        &[],
    );
    assert!(
        !argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "Path B worker must NOT have --dangerously-skip-permissions: {argv:?}"
    );
    let ppt_idx = argv
        .iter()
        .position(|a| a == "--permission-prompt-tool")
        .unwrap_or_else(|| panic!("Path B worker missing --permission-prompt-tool: {argv:?}"));
    assert_eq!(
        argv.get(ppt_idx + 1).map(String::as_str),
        Some("mcp__pitboss__permission_prompt"),
        "Path B worker --permission-prompt-tool target wrong: {argv:?}"
    );
    let idx = argv.iter().position(|a| a == "--allowedTools").unwrap();
    let list = &argv[idx + 1];
    assert!(
        list.contains("mcp__pitboss__permission_prompt"),
        "Path B worker allowedTools must include permission_prompt: {list}"
    );
}

/// #369 regression: when `mcp_config = None` (the recovery path
/// after `write_worker_mcp_config` failed) AND
/// `permission_routing = "path_b"`, the worker would have been
/// spawned with `--permission-prompt-tool mcp__pitboss__permission_prompt`
/// pointing at an unreachable endpoint — claude falls back to its
/// interactive gate, can't be answered under `-p`, and the worker
/// silently stalls. `worker_spawn_args` now downgrades to Path A in
/// this case so the worker still runs, with a `tracing::warn!` so
/// the operator knows to investigate.
#[test]
fn path_b_worker_with_no_mcp_config_falls_back_to_path_a() {
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        None, // ← the recovery condition (#369)
        PermissionRouting::PathB,
        CommunicationMode::Disabled,
        &[],
    );
    // Must have the Path A flag (the fallback) — the worker runs
    // without per-tool gate routing.
    assert!(
        argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "fallback to Path A must add --dangerously-skip-permissions when \
         mcp_config is unavailable under Path B (#369): {argv:?}"
    );
    // Must NOT have --permission-prompt-tool: claude would otherwise
    // try to route through an unreachable endpoint.
    assert!(
        !argv.iter().any(|a| a == "--permission-prompt-tool"),
        "fallback path must NOT keep --permission-prompt-tool — the endpoint \
         is unreachable when mcp_config is None (#369): {argv:?}"
    );
}

/// Companion to #369: when mcp_config IS available, Path B argv is
/// unchanged from the pre-fix behavior — fallback only triggers on
/// the failure path, never silently disables Path B for a healthy
/// worker spawn.
#[test]
fn path_b_worker_with_mcp_config_keeps_path_b_args() {
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        PermissionRouting::PathB,
        CommunicationMode::Disabled,
        &[],
    );
    assert!(
        !argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "Path B with valid mcp_config must NOT degrade to Path A: {argv:?}"
    );
    assert!(
        argv.iter().any(|a| a == "--permission-prompt-tool"),
        "Path B with valid mcp_config must keep --permission-prompt-tool: {argv:?}"
    );
}

/// Companion to #369: under Path A, `mcp_config = None` is benign
/// (it always was — `--dangerously-skip-permissions` means no gate to
/// consult). Verify the fallback logic doesn't accidentally rewrite
/// argv on the Path A path.
#[test]
fn path_a_worker_with_no_mcp_config_unchanged() {
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        None,
        PermissionRouting::PathA,
        CommunicationMode::Disabled,
        &[],
    );
    assert!(
        argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "Path A worker with no mcp_config must still get \
         --dangerously-skip-permissions: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--permission-prompt-tool"),
        "Path A worker must never get --permission-prompt-tool: {argv:?}"
    );
}

#[test]
fn worker_spawn_args_excludes_comm_tools_when_mode_disabled() {
    use crate::dispatch::runner::COMMUNICATION_MCP_TOOLS;
    use crate::manifest::schema::CommunicationMode;
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        Default::default(),
        CommunicationMode::Disabled,
        &[],
    );
    let idx = argv.iter().position(|a| a == "--allowedTools").unwrap();
    let list = &argv[idx + 1];
    for t in COMMUNICATION_MCP_TOOLS {
        assert!(
            !list.contains(t),
            "comm tool {t} must NOT be in worker allowedTools when mode=disabled, \
             got: {list}"
        );
    }
}

#[test]
fn worker_spawn_args_includes_comm_tools_when_mode_parent_child() {
    use crate::dispatch::runner::COMMUNICATION_MCP_TOOLS;
    use crate::manifest::schema::CommunicationMode;
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        Default::default(),
        CommunicationMode::ParentChild,
        &[],
    );
    let idx = argv.iter().position(|a| a == "--allowedTools").unwrap();
    let list = &argv[idx + 1];
    for t in COMMUNICATION_MCP_TOOLS {
        assert!(
            list.contains(t),
            "expected comm tool {t} in worker allowedTools (mode=parent_child), \
             got: {list}"
        );
    }
}

// ---- per-server [[mcp_server]].tools spawn-time filter (#391/#399) ----

fn mcp_server_with_tools(id: &str, tools: Vec<&str>) -> crate::manifest::schema::McpServerSpec {
    crate::manifest::schema::McpServerSpec {
        id: id.into(),
        command: "/bin/true".into(),
        args: vec![],
        env: Default::default(),
        scope: None,
        tools: Some(tools.into_iter().map(String::from).collect()),
    }
}

/// Under Path B, a non-allowlisted MCP tool in the worker's `tools`
/// list is dropped from `--allowedTools` at spawn time. Without the
/// drop, claude would auto-approve the call (anything in
/// `--allowedTools` skips `permission_prompt`) and the runtime gate
/// would never fire — the per-server allowlist would be silently
/// ineffective for programmatic spawns. (#391/#399)
#[test]
fn path_b_worker_filters_non_allowlisted_mcp_tool_from_allowed_tools() {
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    use std::path::PathBuf;

    let servers = vec![mcp_server_with_tools("fs", vec!["read_file"])];
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &[
            "Read".to_string(),
            "mcp__fs__read_file".to_string(),
            "mcp__fs__write_file".to_string(),
        ],
        Some(&PathBuf::from("/tmp/cfg.json")),
        PermissionRouting::PathB,
        CommunicationMode::Disabled,
        &servers,
    );
    let idx = argv.iter().position(|a| a == "--allowedTools").unwrap();
    let list = &argv[idx + 1];
    assert!(
        list.contains("Read"),
        "non-MCP tool must be preserved: {list}"
    );
    assert!(
        list.contains("mcp__fs__read_file"),
        "allowlisted MCP tool must be preserved: {list}"
    );
    assert!(
        !list.split(',').any(|t| t == "mcp__fs__write_file"),
        "non-allowlisted MCP tool must be dropped from allowedTools: {list}"
    );
}

/// **Load-bearing contract pin (#399 design verification):** under
/// Path A (`--dangerously-skip-permissions`), the per-server
/// allowlist has NO runtime effect. Claude bypasses the entire
/// permission layer in `bypassPermissions` mode, so `--allowedTools`
/// is ignored and filtering it would change nothing about
/// enforcement while misleading operators who picked Path A as the
/// "skip all gates" escape hatch. The non-allowlisted tool must
/// remain in the argv. A future "let's just filter under both paths
/// for safety" refactor would silently break this contract — this
/// test catches it.
#[test]
fn path_a_worker_leaves_non_allowlisted_mcp_tool_in_allowed_tools() {
    use crate::manifest::schema::{CommunicationMode, PermissionRouting};
    use std::path::PathBuf;

    let servers = vec![mcp_server_with_tools("fs", vec!["read_file"])];
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string(), "mcp__fs__write_file".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        PermissionRouting::PathA,
        CommunicationMode::Disabled,
        &servers,
    );
    let idx = argv.iter().position(|a| a == "--allowedTools").unwrap();
    let list = &argv[idx + 1];
    assert!(
        list.split(',').any(|t| t == "mcp__fs__write_file"),
        "Path A is the documented 'skip all gates' escape hatch — \
         per-server allowlist must have NO runtime effect under it: {list}"
    );
}

#[tokio::test]
async fn list_workers_empty_when_no_spawns() {
    let state = test_state().await;
    let result = handle_list_workers(&state).await;
    assert_eq!(result.len(), 0);
}

#[tokio::test]
async fn list_workers_shows_pending_and_running() {
    let state = test_state().await;
    {
        let mut w = state.root.workers.write().await;
        w.insert("w-1".into(), WorkerState::Pending);
        w.insert(
            "w-2".into(),
            WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: None,
            },
        );
    }
    let mut result = handle_list_workers(&state).await;
    result.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].task_id, "w-1");
    assert_eq!(result[0].state, "Pending");
    assert_eq!(result[1].task_id, "w-2");
    assert_eq!(result[1].state, "Running");
}

#[tokio::test]
async fn spawn_worker_adds_entry_to_state() {
    let state = test_state().await;
    let args = SpawnWorkerArgs {
        prompt: "investigate issue #1".into(),
        directory: Some("/tmp".into()),
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let result = handle_spawn_worker(&state, args).await.unwrap();
    assert!(result.task_id.starts_with("worker-"));

    // The background task may have already transitioned the worker to
    // Running or Done by the time we read, so we just assert the key
    // exists and is in a valid state (Pending / Running / Done).
    let workers = state.root.workers.read().await;
    assert_eq!(workers.len(), 1);
    let entry = workers.get(&result.task_id).unwrap();
    assert!(matches!(
        entry,
        WorkerState::Pending | WorkerState::Running { .. } | WorkerState::Done(_)
    ));

    // Verify prompt_preview was recorded.
    let prompts = state.root.worker_prompts.read().await;
    assert_eq!(
        prompts.get(&result.task_id).unwrap(),
        "investigate issue #1"
    );
}

#[tokio::test]
async fn spawn_worker_refuses_when_max_workers_reached() {
    let state = test_state().await; // max_workers = 4
                                    // Fill up to cap
    for i in 0..4 {
        let args = SpawnWorkerArgs {
            prompt: format!("w{}", i),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            worker_type: None,
            meta: None,
        };
        handle_spawn_worker(&state, args).await.unwrap();
    }
    // 5th call must fail
    let args = SpawnWorkerArgs {
        prompt: "overflow".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    assert!(err.to_string().contains("worker cap reached"), "err: {err}");
}

#[tokio::test]
async fn spawn_worker_refuses_when_budget_exceeded() {
    let state = test_state().await; // budget_usd = 5.0
    *state.root.spent_usd.lock().await = 5.0; // at cap
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    assert!(err.to_string().contains("budget exceeded"), "err: {err}");
}

#[tokio::test]
async fn spawn_worker_refuses_when_api_rate_limited() {
    use pitboss_core::store::FailureReason;
    let state = test_state().await;
    // Simulate a just-finished worker that hit rate-limit with a
    // reset 10 minutes in the future — any new spawn must refuse.
    let future = chrono::Utc::now() + chrono::Duration::minutes(10);
    state
        .api_health
        .record(&FailureReason::RateLimit {
            resets_at: Some(future),
        })
        .await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    assert!(
        err.to_string().contains("rate-limited"),
        "err should mention rate-limited: {err}"
    );
}

#[tokio::test]
async fn spawn_worker_refuses_when_api_auth_failed() {
    use pitboss_core::store::FailureReason;
    let state = test_state().await;
    state.api_health.record(&FailureReason::AuthFailure).await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    assert!(
        err.to_string().contains("auth failed"),
        "err should mention auth failed: {err}"
    );
}

#[tokio::test]
async fn spawn_worker_refuses_when_draining() {
    let state = test_state().await;
    state.root.cancel.drain();
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    assert!(err.to_string().contains("draining"), "err: {err}");
}

#[tokio::test]
async fn worker_status_reads_state() {
    let state = test_state().await;
    let args = SpawnWorkerArgs {
        prompt: "investigate bug".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let spawn = handle_spawn_worker(&state, args).await.unwrap();
    let status = handle_worker_status(&state, &spawn.task_id).await.unwrap();
    // The background task may have already transitioned the worker to
    // Running; we accept either state here. Done is not expected because
    // the test FakeSpawner holds its children open until signalled.
    assert!(
        matches!(status.state.as_str(), "Pending" | "Running"),
        "unexpected state: {}",
        status.state
    );
    // prompt_preview is populated synchronously before the background task.
    assert_eq!(status.prompt_preview, "investigate bug");
}

#[tokio::test]
async fn worker_status_unknown_id_errors() {
    let state = test_state().await;
    let err = handle_worker_status(&state, "nope-123").await.unwrap_err();
    assert!(err.to_string().contains("unknown task_id"));
}

#[tokio::test]
async fn cancel_worker_sets_cancelled_state() {
    let state = test_state().await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let spawn = handle_spawn_worker(&state, args).await.unwrap();

    let result = handle_cancel_worker(&state, &spawn.task_id).await.unwrap();
    assert!(result.ok);

    // Note: in real wiring, CancelToken signals the SessionHandle to terminate
    // and the subsequent Done(...) entry in state.root.workers carries status=Cancelled.
    // For v0.3 Task 14 (unit-level), we just verify the cancel call succeeded
    // and didn't panic. Full flow is tested in integration tests (Phase 6).
}

#[tokio::test]
async fn wait_for_worker_returns_outcome_on_completion() {
    use pitboss_core::store::{TaskRecord, TaskStatus};
    use std::time::Duration;

    let state = test_state().await;
    let task_id = "worker-test-1".to_string();
    {
        let mut w = state.root.workers.write().await;
        w.insert(task_id.clone(), WorkerState::Pending);
    }

    // Spawn a task that marks the worker Done after 50 ms.
    let state_clone = state.clone();
    let task_id_clone = task_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let rec = TaskRecord {
            task_id: task_id_clone.clone(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 42,
            worktree_path: None,
            log_path: std::path::PathBuf::new(),
            token_usage: Default::default(),
            claude_session_id: None,
            final_message_preview: Some("ok".into()),
            final_message: Some("ok".into()),
            parent_task_id: Some("lead".into()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
            cost_usd: None,
            actor_type: None,
        };
        let mut w = state_clone.root.workers.write().await;
        w.insert(task_id_clone.clone(), WorkerState::Done(rec));
        let _ = state_clone.root.done_tx.send(task_id_clone);
    });

    let outcome = handle_wait_for_worker(&state, &task_id, Some(5))
        .await
        .unwrap();
    assert!(matches!(outcome.status, TaskStatus::Success));
}

#[tokio::test]
async fn wait_for_worker_times_out() {
    let state = test_state().await;
    let task_id = "worker-stuck".to_string();
    {
        let mut w = state.root.workers.write().await;
        w.insert(task_id.clone(), WorkerState::Pending);
    }
    let err = handle_wait_for_worker(&state, &task_id, Some(0))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timed out"), "err: {err}");
}

#[tokio::test]
async fn cancel_worker_unknown_id_errors() {
    let state = test_state().await;
    let err = handle_cancel_worker(&state, "never-existed")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown task_id"));
}

#[tokio::test]
async fn wait_for_any_returns_first_completed() {
    use pitboss_core::store::{TaskRecord, TaskStatus};
    use std::time::Duration;

    let state = test_state().await;
    let ids = vec!["w-a".to_string(), "w-b".to_string(), "w-c".to_string()];
    {
        let mut w = state.root.workers.write().await;
        for id in &ids {
            w.insert(id.clone(), WorkerState::Pending);
        }
    }

    // Race: w-b finishes first at 30ms, w-a at 100ms.
    let state_clone = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let rec = TaskRecord {
            task_id: "w-b".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 30,
            worktree_path: None,
            log_path: std::path::PathBuf::new(),
            token_usage: Default::default(),
            claude_session_id: None,
            final_message_preview: None,
            final_message: None,
            parent_task_id: Some("lead".into()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
            cost_usd: None,
            actor_type: None,
        };
        let mut w = state_clone.root.workers.write().await;
        w.insert("w-b".into(), WorkerState::Done(rec));
        let _ = state_clone.root.done_tx.send("w-b".into());
    });

    let (winner_id, _rec) = handle_wait_for_any(&state, &ids, Some(5)).await.unwrap();
    assert_eq!(winner_id, "w-b");
}

/// Build a test_state whose FakeSpawner produces a completed session
/// (with a result event carrying a known token_usage), so the
/// backgrounded worker actually transitions through the full spawn path.
async fn completing_test_state() -> Arc<DispatchState> {
    completing_test_state_with_budget(None).await
}

async fn completing_test_state_with_budget(budget: Option<f64>) -> Arc<DispatchState> {
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "lead prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: Default::default(),
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: budget,
        lead_timeout_secs: None,
        default_approval_policy: None,
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let run_id = Uuid::now_v7();
    // Emit a single result event with known token usage, then exit 0.
    let script = FakeScript::new()
        .stdout_line(r#"{"type":"system","subtype":"init"}"#)
        .stdout_line(
            r#"{"type":"result","session_id":"sess_ok","usage":{"input_tokens":1000,"output_tokens":2000}}"#,
        )
        .exit_code(0);
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    std::mem::forget(dir);
    Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ))
}

#[tokio::test]
async fn spawn_worker_completes_and_updates_spent_usd_and_parent_task_id() {
    use pitboss_core::store::TaskStatus;
    use std::time::Duration;

    let state = completing_test_state().await;
    let args = SpawnWorkerArgs {
        prompt: "analyze bug #42".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None, // falls back to lead model (claude-haiku-4-5)
        worker_type: None,
        meta: None,
    };

    // Subscribe to done events BEFORE spawning.
    let mut rx = state.root.done_tx.subscribe();
    let spawn = handle_spawn_worker(&state, args).await.unwrap();

    // Wait for the broadcast.
    let id = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("broadcast arrives in time")
        .expect("broadcast channel open");
    assert_eq!(id, spawn.task_id, "broadcast id matches spawn id");

    // Verify Done state + Success + parent_task_id.
    let workers = state.root.workers.read().await;
    let entry = workers.get(&spawn.task_id).expect("worker recorded");
    match entry {
        WorkerState::Done(rec) => {
            assert!(
                matches!(rec.status, TaskStatus::Success),
                "status is Success"
            );
            assert_eq!(rec.parent_task_id.as_deref(), Some("lead"));
            assert_eq!(rec.token_usage.input, 1000);
            assert_eq!(rec.token_usage.output, 2000);
            assert_eq!(rec.claude_session_id.as_deref(), Some("sess_ok"));
        }
        other => panic!("expected Done, got {other:?}"),
    }
    drop(workers);

    // Verify cost accumulation. claude-haiku-4-5: input $0.80/1M, output $4.00/1M.
    // 1000 input = $0.0008; 2000 output = $0.008; total = $0.0088.
    let spent = *state.root.spent_usd.lock().await;
    assert!(
        (spent - 0.0088).abs() < 1e-6,
        "expected spent_usd ≈ 0.0088, got {spent}"
    );

    // Verify prompt_preview is present.
    let preview = state
        .root
        .worker_prompts
        .read()
        .await
        .get(&spawn.task_id)
        .cloned()
        .unwrap_or_default();
    assert_eq!(preview, "analyze bug #42");
}

#[tokio::test]
async fn burst_spawn_is_budget_capped_via_reservation() {
    // Budget = $0.25. With a per-worker haiku estimate of $0.10 (the
    // fallback for haiku when no workers have completed), only 2 workers
    // should pass the guard in a burst:
    //   spawn 1: spent 0 + reserved 0 + est 0.10 = 0.10 ≤ 0.25 → OK, reserved becomes 0.10
    //   spawn 2: spent 0 + reserved 0.10 + est 0.10 = 0.20 ≤ 0.25 → OK, reserved becomes 0.20
    //   spawn 3: spent 0 + reserved 0.20 + est 0.10 = 0.30 > 0.25 → REJECT
    let state = test_state_with_budget(0.25).await;
    // Lead model defaults to "claude-haiku-4-5" in test_state.

    let args = |prompt: &str| SpawnWorkerArgs {
        prompt: prompt.into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };

    let r1 = handle_spawn_worker(&state, args("w1")).await;
    assert!(r1.is_ok(), "first spawn should pass: {r1:?}");

    let r2 = handle_spawn_worker(&state, args("w2")).await;
    assert!(r2.is_ok(), "second spawn should pass: {r2:?}");

    let r3 = handle_spawn_worker(&state, args("w3")).await;
    assert!(r3.is_err(), "third spawn should be rejected by reservation");
    let msg = r3.unwrap_err().to_string();
    assert!(
        msg.contains("budget exceeded"),
        "expected budget-exceeded message, got: {msg}"
    );

    // Sanity: the reservation should now reflect the two passing spawns.
    let reserved_now = *state.root.reserved_usd.lock().await;
    assert!(
        (reserved_now - 0.20).abs() < 1e-9,
        "expected reserved ≈ 0.20, got {reserved_now}"
    );
}

#[tokio::test]
async fn reservation_released_on_worker_completion() {
    // Spawn one worker, wait for completion, verify reserved_usd returns to 0.
    use std::time::Duration;

    let state = completing_test_state_with_budget(Some(1.00)).await;

    // Subscribe to done events BEFORE spawning — the completion path is
    // fast (FakeScript exits immediately after emitting the result line).
    let mut rx = state.root.done_tx.subscribe();

    let spawn = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            worker_type: None,
            meta: None,
        },
    )
    .await
    .unwrap();

    // Reservation should be > 0 at some point between spawn and completion;
    // under a very fast FakeSpawner the worker can complete before this
    // read, so we only assert "reservation was initialized to >0". That's
    // checked indirectly via the `worker_reservations` map having an entry
    // (or having had one — it's removed on release).
    // The primary assertion is post-completion.

    let completed_id = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("broadcast arrives in time")
        .expect("broadcast channel open");
    assert_eq!(completed_id, spawn.task_id);

    let reserved_after = *state.root.reserved_usd.lock().await;
    assert!(
        reserved_after.abs() < 1e-9,
        "reservation should be released after completion, got {reserved_after}"
    );
    let reservations = state.root.worker_reservations.read().await;
    assert!(
        !reservations.contains_key(&spawn.task_id),
        "reservation entry should be removed on completion"
    );
}

#[test]
fn initial_estimate_is_model_aware() {
    assert!((initial_estimate_for("claude-haiku-4-5") - 0.10).abs() < 1e-9);
    assert!((initial_estimate_for("claude-sonnet-4-6") - 0.50).abs() < 1e-9);
    assert!((initial_estimate_for("claude-opus-4-7") - 2.00).abs() < 1e-9);
    // Unknown model falls back to Haiku's rate.
    assert!((initial_estimate_for("claude-unknown-x-y") - 0.10).abs() < 1e-9);
    // Dated suffix is normalized (matches `rates_for` in pitboss-core::prices).
    assert!((initial_estimate_for("claude-haiku-4-5-20251001") - 0.10).abs() < 1e-9);
    assert!((initial_estimate_for("claude-sonnet-4-6-20251001") - 0.50).abs() < 1e-9);
    assert!((initial_estimate_for("claude-opus-4-7-20251001") - 2.00).abs() < 1e-9);
}

#[tokio::test]
async fn running_worker_state_gets_session_id_after_init() {
    use std::time::Duration;

    let state = completing_test_state().await;
    let mut rx = state.root.done_tx.subscribe();
    let args = SpawnWorkerArgs {
        prompt: "analyze".into(),
        directory: None,
        branch: None,
        tools: None,
        timeout_secs: None,
        model: None,
        worker_type: None,
        meta: None,
    };
    let spawn = handle_spawn_worker(&state, args).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("broadcast arrives")
        .expect("broadcast open");

    // Post-completion, the worker is in Done state. The session_id is
    // preserved on TaskRecord via SessionOutcome. Assert it.
    let workers = state.root.workers.read().await;
    match workers.get(&spawn.task_id).unwrap() {
        WorkerState::Done(rec) => {
            assert_eq!(rec.claude_session_id.as_deref(), Some("sess_ok"));
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn continue_worker_args_roundtrip() {
    let a = ContinueWorkerArgs {
        task_id: "w".into(),
        prompt: Some("next step".into()),
    };
    let s = serde_json::to_string(&a).unwrap();
    let back: ContinueWorkerArgs = serde_json::from_str(&s).unwrap();
    assert_eq!(back.task_id, "w");
    assert_eq!(back.prompt.as_deref(), Some("next step"));
}

#[test]
fn reprompt_worker_args_roundtrip() {
    let a = RepromptWorkerArgs {
        task_id: "w-1".into(),
        prompt: "new plan".into(),
    };
    let s = serde_json::to_string(&a).unwrap();
    let back: RepromptWorkerArgs = serde_json::from_str(&s).unwrap();
    assert_eq!(back.task_id, "w-1");
    assert_eq!(back.prompt, "new plan");
}

#[test]
fn request_approval_args_roundtrip() {
    // Bare form — no plan.
    let a = RequestApprovalArgs {
        summary: "spawn 3 workers".into(),
        timeout_secs: Some(60),
        plan: None,
        ..Default::default()
    };
    let s = serde_json::to_string(&a).unwrap();
    let back: RequestApprovalArgs = serde_json::from_str(&s).unwrap();
    assert_eq!(back.summary, "spawn 3 workers");
    assert_eq!(back.timeout_secs, Some(60));
    assert!(back.plan.is_none());

    // Typed form.
    let b = RequestApprovalArgs {
        summary: "drop staging index".into(),
        timeout_secs: None,
        plan: Some(ApprovalPlan {
            summary: "drop staging index".into(),
            rationale: Some("obsolete since v2".into()),
            resources: vec!["db/idx_foo".into()],
            risks: vec!["slow reads if live".into()],
            rollback: Some("restore from snapshot".into()),
        }),
        ..Default::default()
    };
    let s = serde_json::to_string(&b).unwrap();
    let back: RequestApprovalArgs = serde_json::from_str(&s).unwrap();
    let plan = back.plan.unwrap();
    assert_eq!(plan.rationale.as_deref(), Some("obsolete since v2"));
    assert_eq!(plan.resources, vec!["db/idx_foo".to_string()]);
}

#[tokio::test]
async fn handle_pause_worker_pauses_running_worker() {
    let state = test_state().await;
    let worker_token = pitboss_core::session::CancelToken::new();
    state
        .root
        .worker_cancels
        .write()
        .await
        .insert("w-1".into(), worker_token.clone());
    state.root.workers.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );
    let res = handle_pause_worker(&state, "w-1", PauseMode::Cancel)
        .await
        .unwrap();
    assert!(res.ok);
    assert!(worker_token.is_terminated());
    let workers = state.root.workers.read().await;
    assert!(matches!(
        workers.get("w-1").unwrap(),
        WorkerState::Paused { .. }
    ));
}

/// End-to-end freeze: spawn a real sleeping child, register its pid
/// slot + a Running WorkerState, call handle_pause_worker(Freeze),
/// verify Frozen state + that /proc (on Linux) sees the process as
/// stopped. Then handle_continue_worker to thaw.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn freeze_and_thaw_transition_via_handler() {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let state = test_state().await;

    // Spawn a real long-sleep child we can safely SIGSTOP/SIGCONT.
    // Process-group-isolated so freeze() (which signals `-pgid`) does
    // not deliver SIGSTOP to the cargo-test runner itself — matches
    // what TokioSpawner does in production.
    let mut cmd = Command::new("sleep");
    cmd.arg("30").process_group(0);
    let child = cmd.spawn().unwrap();
    let pid = child.id();

    // Register the pid + Running state.
    let slot = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(pid));
    state
        .root
        .worker_pids
        .write()
        .await
        .insert("w-freeze".into(), slot);
    state
        .root
        .worker_cancels
        .write()
        .await
        .insert("w-freeze".into(), pitboss_core::session::CancelToken::new());
    state.root.workers.write().await.insert(
        "w-freeze".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess-freeze".into()),
        },
    );

    // Freeze.
    let res = handle_pause_worker(&state, "w-freeze", PauseMode::Freeze)
        .await
        .unwrap();
    assert!(res.ok);
    assert!(matches!(
        state.root.workers.read().await.get("w-freeze").unwrap(),
        WorkerState::Frozen { .. }
    ));

    // /proc should show 'T' (stopped).
    std::thread::sleep(std::time::Duration::from_millis(50));
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let state_line = status
        .lines()
        .find(|l| l.starts_with("State:"))
        .unwrap_or("State: ?");
    assert!(
        state_line.contains('T'),
        "expected stopped state, got {state_line}"
    );

    // Thaw via continue_worker (no prompt — freeze path ignores it).
    let cres = handle_continue_worker(
        &state,
        ContinueWorkerArgs {
            task_id: "w-freeze".into(),
            prompt: None,
        },
    )
    .await
    .unwrap();
    assert!(cres.ok);
    assert!(matches!(
        state.root.workers.read().await.get("w-freeze").unwrap(),
        WorkerState::Running { .. }
    ));

    // Cleanup.
    let mut owned = child;
    let _ = owned.kill();
    let _ = owned.wait();
}

#[tokio::test]
async fn handle_continue_worker_resumes_paused() {
    let state = test_state().await;
    state.root.workers.write().await.insert(
        "w-1".into(),
        WorkerState::Paused {
            session_id: "sess".into(),
            paused_at: chrono::Utc::now(),
            prior_token_usage: Default::default(),
        },
    );
    state
        .root
        .worker_prompts
        .write()
        .await
        .insert("w-1".into(), "hi".into());
    state
        .root
        .worker_models
        .write()
        .await
        .insert("w-1".into(), "claude-haiku-4-5".into());
    let res = handle_continue_worker(
        &state,
        ContinueWorkerArgs {
            task_id: "w-1".into(),
            prompt: Some("resume please".into()),
        },
    )
    .await
    .unwrap();
    assert!(res.ok);
    let workers = state.root.workers.read().await;
    assert!(matches!(
        workers.get("w-1").unwrap(),
        WorkerState::Running { .. }
    ));
}

#[tokio::test]
async fn handle_reprompt_worker_from_running() {
    let state = test_state().await;
    let worker_token = pitboss_core::session::CancelToken::new();
    state
        .root
        .worker_cancels
        .write()
        .await
        .insert("w-1".into(), worker_token.clone());
    state.root.workers.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess-abc".into()),
        },
    );
    state
        .root
        .worker_prompts
        .write()
        .await
        .insert("w-1".into(), "original".into());
    state
        .root
        .worker_models
        .write()
        .await
        .insert("w-1".into(), "claude-haiku-4-5".into());

    let res = handle_reprompt_worker(
        &state,
        RepromptWorkerArgs {
            task_id: "w-1".into(),
            prompt: "new plan".into(),
        },
    )
    .await
    .unwrap();

    assert!(res.ok);
    // Counter bumps on success.
    let counters = state
        .root
        .worker_counters
        .read()
        .await
        .get("w-1")
        .cloned()
        .unwrap_or_default();
    assert_eq!(counters.reprompt_count, 1);
    // events.jsonl records the reprompt.
    let events_path = state
        .root
        .run_subdir
        .join("tasks")
        .join("w-1")
        .join("events.jsonl");
    let events = tokio::fs::read_to_string(&events_path).await.unwrap();
    assert!(
        events.contains("\"kind\":\"reprompt\""),
        "events.jsonl missing reprompt: {events}"
    );
    // Worker transitioned back to Running via spawn_resume_worker.
    let workers = state.root.workers.read().await;
    assert!(matches!(
        workers.get("w-1").unwrap(),
        WorkerState::Running { .. }
    ));
}

#[tokio::test]
async fn handle_reprompt_worker_from_done_errors() {
    let state = test_state().await;
    // Insert a Done worker — terminal state, no reprompt allowed.
    let rec = pitboss_core::store::TaskRecord {
        task_id: "w-done".into(),
        status: pitboss_core::store::TaskStatus::Success,
        exit_code: Some(0),
        started_at: chrono::Utc::now(),
        ended_at: chrono::Utc::now(),
        duration_ms: 0,
        worktree_path: None,
        log_path: std::path::PathBuf::from("/tmp/x"),
        token_usage: Default::default(),
        claude_session_id: Some("sess-done".into()),
        final_message_preview: None,
        final_message: None,
        parent_task_id: Some("lead".into()),
        pause_count: 0,
        reprompt_count: 0,
        approvals_requested: 0,
        approvals_approved: 0,
        approvals_rejected: 0,
        model: None,
        failure_reason: None,
        cost_usd: None,
        actor_type: None,
    };
    state
        .root
        .workers
        .write()
        .await
        .insert("w-done".into(), WorkerState::Done(rec));

    let err = handle_reprompt_worker(
        &state,
        RepromptWorkerArgs {
            task_id: "w-done".into(),
            prompt: "retry".into(),
        },
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string().contains("already completed"),
        "expected 'already completed' in error, got: {err}"
    );
}

/// Register a sub-lead `LayerState` on `state` keyed by `sublead_id`,
/// inheriting the spawner / store / etc. from the root layer. Used by
/// the issue-#146 regression tests below to verify that mutating
/// handlers route to the owning sub-lead's layer.
async fn register_test_sublead(
    state: &Arc<DispatchState>,
    sublead_id: &str,
) -> Arc<crate::dispatch::layer::LayerState> {
    use crate::dispatch::layer::LayerState;
    use pitboss_core::session::CancelToken;
    use pitboss_core::worktree::CleanupPolicy;

    let sub_layer = Arc::new(LayerState::new(
        state.root.run_id,
        state.root.manifest.clone(),
        state.root.store.clone(),
        CancelToken::new(),
        sublead_id.to_string(),
        state.root.spawner.clone(),
        state.root.claude_binary.clone(),
        state.root.wt_mgr.clone(),
        CleanupPolicy::Never,
        state.root.run_subdir.clone(),
        state.root.approval_policy,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        None,
    ));
    state
        .subleads
        .write()
        .await
        .insert(sublead_id.to_string(), sub_layer.clone());
    sub_layer
}

/// Register `task_id` on the `worker_layer_index` so `layer_for_worker`
/// resolves it via the O(1) path (matching production registration in
/// `spawn_worker`).
async fn index_worker(state: &Arc<DispatchState>, task_id: &str, sublead_id: Option<&str>) {
    state
        .worker_layer_index
        .write()
        .await
        .insert(task_id.to_string(), sublead_id.map(|s| s.to_string()));
}

/// Issue #146 regression: handle_pause_worker must target the
/// owning sub-lead's `LayerState`, not always root.
#[tokio::test]
async fn handle_pause_worker_targets_sublead_layer() {
    let state = test_state().await;
    let sub_layer = register_test_sublead(&state, "sublead-A").await;
    index_worker(&state, "w-sub", Some("sublead-A")).await;

    let worker_token = pitboss_core::session::CancelToken::new();
    sub_layer
        .worker_cancels
        .write()
        .await
        .insert("w-sub".into(), worker_token.clone());
    sub_layer.workers.write().await.insert(
        "w-sub".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );
    // Sanity: root has no entry — pre-fix code would bail here.
    assert!(state.root.workers.read().await.get("w-sub").is_none());

    let res = handle_pause_worker(&state, "w-sub", PauseMode::Cancel)
        .await
        .unwrap();
    assert!(res.ok);
    assert!(worker_token.is_terminated());
    let workers = sub_layer.workers.read().await;
    assert!(matches!(
        workers.get("w-sub").unwrap(),
        WorkerState::Paused { .. }
    ));
}

/// Issue #146 regression: handle_continue_worker must target the
/// owning sub-lead's `LayerState`.
#[tokio::test]
async fn handle_continue_worker_targets_sublead_layer() {
    let state = test_state().await;
    let sub_layer = register_test_sublead(&state, "sublead-A").await;
    index_worker(&state, "w-sub", Some("sublead-A")).await;

    sub_layer.workers.write().await.insert(
        "w-sub".into(),
        WorkerState::Paused {
            session_id: "sess".into(),
            paused_at: chrono::Utc::now(),
            prior_token_usage: Default::default(),
        },
    );
    sub_layer
        .worker_prompts
        .write()
        .await
        .insert("w-sub".into(), "hi".into());
    sub_layer
        .worker_models
        .write()
        .await
        .insert("w-sub".into(), "claude-haiku-4-5".into());

    let res = handle_continue_worker(
        &state,
        ContinueWorkerArgs {
            task_id: "w-sub".into(),
            prompt: Some("resume please".into()),
        },
    )
    .await
    .unwrap();
    assert!(res.ok);
    // The sublead's layer has the resumed worker — root must remain empty.
    assert!(state.root.workers.read().await.get("w-sub").is_none());
    let workers = sub_layer.workers.read().await;
    assert!(matches!(
        workers.get("w-sub").unwrap(),
        WorkerState::Running { .. }
    ));
}

/// Issue #146 regression: handle_reprompt_worker must target the
/// owning sub-lead's `LayerState` for both the cancel-and-respawn
/// path AND the counter bump.
#[tokio::test]
async fn handle_reprompt_worker_targets_sublead_layer() {
    let state = test_state().await;
    let sub_layer = register_test_sublead(&state, "sublead-A").await;
    index_worker(&state, "w-sub", Some("sublead-A")).await;

    let worker_token = pitboss_core::session::CancelToken::new();
    sub_layer
        .worker_cancels
        .write()
        .await
        .insert("w-sub".into(), worker_token.clone());
    sub_layer.workers.write().await.insert(
        "w-sub".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess-abc".into()),
        },
    );
    sub_layer
        .worker_prompts
        .write()
        .await
        .insert("w-sub".into(), "original".into());
    sub_layer
        .worker_models
        .write()
        .await
        .insert("w-sub".into(), "claude-haiku-4-5".into());

    let res = handle_reprompt_worker(
        &state,
        RepromptWorkerArgs {
            task_id: "w-sub".into(),
            prompt: "new plan".into(),
        },
    )
    .await
    .unwrap();
    assert!(res.ok);
    // Counter bumps on the sub-lead's layer (NOT root).
    let counters = sub_layer
        .worker_counters
        .read()
        .await
        .get("w-sub")
        .cloned()
        .unwrap_or_default();
    assert_eq!(counters.reprompt_count, 1);
    // Root counters should be empty.
    assert!(state
        .root
        .worker_counters
        .read()
        .await
        .get("w-sub")
        .is_none());
}

/// Issue #146 regression: handle_cancel_worker must terminate the
/// owning sub-lead's CancelToken.
#[tokio::test]
async fn handle_cancel_worker_targets_sublead_layer() {
    let state = test_state().await;
    let sub_layer = register_test_sublead(&state, "sublead-A").await;
    index_worker(&state, "w-sub", Some("sublead-A")).await;

    let worker_token = pitboss_core::session::CancelToken::new();
    sub_layer
        .worker_cancels
        .write()
        .await
        .insert("w-sub".into(), worker_token.clone());
    sub_layer.workers.write().await.insert(
        "w-sub".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    // Pre-fix: this would bail "unknown task_id" because the read was
    // pinned to state.root.worker_cancels.
    let res = handle_cancel_worker(&state, "w-sub").await.unwrap();
    assert!(res.ok);
    assert!(worker_token.is_terminated());
}

#[tokio::test]
async fn handle_request_approval_auto_approves() {
    use crate::dispatch::state::ApprovalPolicy;
    // Rebuild a state with AutoApprove.
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "p".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: Default::default(),
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        default_approval_policy: Some(ApprovalPolicy::AutoApprove),
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    std::mem::forget(dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::AutoApprove,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ));
    let resp = handle_request_approval(
        &state,
        RequestApprovalArgs {
            summary: "spawn 3".into(),
            timeout_secs: Some(2),
            plan: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(resp.approved);
}

/// Path B: `permission_prompt` routes to the approval queue and returns
/// Claude Code's gate response shape (`decision`/`behavior`).
#[tokio::test]
async fn permission_prompt_auto_approves_and_returns_gate_response() {
    use crate::dispatch::state::ApprovalPolicy;
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, PermissionRouting, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "p".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: PermissionRouting::PathB,
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        default_approval_policy: Some(ApprovalPolicy::AutoApprove),
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    std::mem::forget(dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::AutoApprove,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ));
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "auto-approve should yield Allow variant, got: {resp:?}"
    );
}

/// Path B + per-server `[[mcp_server]].tools` allowlist: when claude
/// routes a `mcp__<server>__<not-allowlisted>` call through
/// `permission_prompt` (because the spawn-time filter dropped it from
/// `--allowedTools`), the runtime gate denies with
/// `DeniedByMcpServerAllowlist` and returns the model-readable
/// reason string. The deny short-circuit must fire BEFORE the
/// operator policy + typed-profile gates so `auto_approve` policy
/// rules can't widen what the per-server allowlist forbids — most
/// restrictive wins. (#391/#399)
#[tokio::test]
async fn permission_prompt_denies_mcp_tool_outside_server_allowlist() {
    use crate::dispatch::state::ApprovalPolicy;
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, McpServerSpec, PermissionRouting, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "p".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: PermissionRouting::PathB,
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    // Operator policy auto-approves everything — verifies the
    // mcp_server allowlist gate fires BEFORE the operator rule
    // (which would otherwise have approved the call). Most-
    // restrictive wins.
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        default_approval_policy: Some(ApprovalPolicy::AutoApprove),
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![McpServerSpec {
            id: "fs".into(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: None,
            tools: Some(vec!["read_file".into()]),
        }],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    std::mem::forget(dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::AutoApprove,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ));
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "mcp__fs__write_file".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    let PermissionPromptResponse::Deny { message, .. } = resp else {
        panic!("expected Deny for non-allowlisted MCP tool, got: {resp:?}");
    };
    assert!(
        message.contains("write_file") && message.contains("fs"),
        "deny reason should name the offending tool and server: {message}"
    );
    assert!(
        message.contains("not in") && message.contains("allowlist"),
        "deny reason should phrase as 'not in <X> allowlist' for model legibility: {message}"
    );
}

/// Companion to the deny test: when the requested tool IS in the
/// per-server allowlist, the gate doesn't fire — control falls through
/// to the existing gates (operator policy / typed-profile / bridge).
/// Without this pin, an over-aggressive gate could deny legitimate
/// allowlisted calls.
#[tokio::test]
async fn permission_prompt_admits_mcp_tool_inside_server_allowlist() {
    use crate::dispatch::state::ApprovalPolicy;
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, McpServerSpec, PermissionRouting, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "p".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: PermissionRouting::PathB,
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        // AutoApprove default: lets `read_file` through the operator
        // policy gate so we know the mcp_server allowlist DIDN'T
        // deny — the call reaches Allow via the next gate.
        default_approval_policy: Some(ApprovalPolicy::AutoApprove),
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![McpServerSpec {
            id: "fs".into(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: None,
            tools: Some(vec!["read_file".into()]),
        }],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    std::mem::forget(dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::AutoApprove,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ));
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "mcp__fs__read_file".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "allowlisted MCP tool should fall through to AutoApprove, got: {resp:?}"
    );
}

/// Build a `DispatchState` with the specified approval policy and
/// `require_plan_approval` flag. Pins `denial_termination_policy =
/// Reclassify` (the legacy behavior) so existing tests asserting
/// reclassification continue to pass — the production default flipped
/// to `Adapt` in #377. Tests of the new default use
/// `mk_plan_state_with_termination_policy` directly.
async fn mk_plan_state(
    policy: crate::dispatch::state::ApprovalPolicy,
    require_plan_approval: bool,
) -> Arc<DispatchState> {
    mk_plan_state_with_termination_policy(
        policy,
        require_plan_approval,
        crate::dispatch::state::DenialTerminationPolicy::Reclassify,
    )
    .await
}

/// Variant of `mk_plan_state` that exposes the
/// `denial_termination_policy` field. Use directly when a test needs to
/// exercise the `Adapt` (post-#377 default) reclassification behavior.
async fn mk_plan_state_with_termination_policy(
    policy: crate::dispatch::state::ApprovalPolicy,
    require_plan_approval: bool,
    termination_policy: crate::dispatch::state::DenialTerminationPolicy,
) -> Arc<DispatchState> {
    use crate::dispatch::state::ApprovalPolicy;
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;
    let _ = ApprovalPolicy::Block; // silence unused-variant warning on import

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "p".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 60,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: Default::default(),
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        default_approval_policy: Some(policy),
        denial_termination_policy: Some(termination_policy),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    std::mem::forget(dir);
    Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        policy,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ))
}

#[tokio::test]
async fn spawn_worker_blocks_when_plan_not_approved() {
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoApprove, true).await;
    // plan_approved starts false; even with AutoApprove policy for
    // per-action approvals, spawn_worker must refuse until a plan
    // has actually been approved.
    let err = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "do work".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            worker_type: None,
            meta: None,
        },
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("plan approval required"),
        "expected plan-approval error, got: {msg}"
    );
}

#[tokio::test]
async fn spawn_worker_allowed_when_require_plan_approval_off() {
    // Default behavior: runs without the opt-in flag never gate on
    // plan_approved. Whether the spawn ultimately succeeds or fails
    // depends on unrelated state we don't exercise here — we only
    // assert that the plan-approval guard itself doesn't fire.
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoApprove, false).await;
    let res = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "do work".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            worker_type: None,
            meta: None,
        },
    )
    .await;
    match res {
        Ok(_) => {} // guard correctly skipped
        Err(e) => assert!(
            !e.to_string().contains("plan approval required"),
            "plan-approval guard should not fire when require_plan_approval=false, got: {e}"
        ),
    }
}

#[tokio::test]
async fn propose_plan_auto_approve_flips_flag() {
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoApprove, true).await;
    assert!(!state
        .root
        .plan_approved
        .load(std::sync::atomic::Ordering::Acquire));

    let resp = handle_propose_plan(
        &state,
        ProposePlanArgs {
            plan: ApprovalPlan {
                summary: "phase-1".into(),
                rationale: Some("prep".into()),
                resources: vec!["3 worktrees".into()],
                risks: vec![],
                rollback: Some("none".into()),
            },
            timeout_secs: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(resp.approved);
    assert!(state
        .root
        .plan_approved
        .load(std::sync::atomic::Ordering::Acquire));
}

/// #151 M5 regression: a `cost_over` rule fires for `propose_plan`
/// when the caller passes an explicit `cost_estimate` that exceeds
/// the threshold. Pre-fix the matcher invocation hard-coded
/// `cost = None`, so even `cost_over = 0.0` rules silently never
/// matched for plan-level approvals.
#[tokio::test]
async fn propose_plan_cost_over_rule_auto_rejects_when_estimate_exceeds() {
    use crate::mcp::policy::{ApprovalAction, ApprovalRule, PolicyMatcher};

    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::Block, true).await;
    // Operator-declared rule: any plan whose cost > $5 is auto-rejected.
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch {
                cost_over: Some(5.0),
                ..Default::default()
            },
            action: ApprovalAction::AutoReject,
        }]))
        .await;

    // Above threshold → auto-reject fires.
    let resp = handle_propose_plan(
        &state,
        ProposePlanArgs {
            plan: ApprovalPlan {
                summary: "expensive plan".into(),
                ..Default::default()
            },
            timeout_secs: Some(2),
            cost_estimate: Some(10.0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        !resp.approved,
        "cost_estimate=10 over threshold=5 must auto-reject"
    );
    assert!(
        !state
            .root
            .plan_approved
            .load(std::sync::atomic::Ordering::Acquire),
        "rejected plan must not flip plan_approved"
    );
}

/// #151 M5 regression: a `cost_over` rule does NOT fire for
/// `propose_plan` when the caller's `cost_estimate` is at or below
/// the threshold. Cheap plans must still flow through the normal
/// approval path.
#[tokio::test]
async fn propose_plan_cost_over_rule_does_not_fire_below_threshold() {
    use crate::mcp::policy::{ApprovalAction, ApprovalRule, PolicyMatcher};

    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoApprove, true).await;
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch {
                cost_over: Some(5.0),
                ..Default::default()
            },
            action: ApprovalAction::AutoReject,
        }]))
        .await;

    // Below threshold → rule does not match; falls through to
    // the bridge which auto-approves under the AutoApprove policy.
    let resp = handle_propose_plan(
        &state,
        ProposePlanArgs {
            plan: ApprovalPlan {
                summary: "cheap plan".into(),
                ..Default::default()
            },
            timeout_secs: Some(2),
            cost_estimate: Some(0.50),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        resp.approved,
        "cost_estimate=0.50 below threshold=5 must fall through to AutoApprove"
    );
}

/// #151 M5 regression: a `cost_over` rule fires for
/// `permission_prompt` when the caller passes an explicit
/// `cost_estimate` that exceeds the threshold. Returns Claude
/// Code's deny gate response.
#[tokio::test]
async fn permission_prompt_cost_over_rule_denies_when_estimate_exceeds() {
    use crate::mcp::policy::{ApprovalAction, ApprovalRule, PolicyMatcher};

    // Default Block policy so the matcher's verdict is the only
    // path to a fast resolve — no AutoApprove fallback to mask a
    // missed cost_over evaluation.
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::Block, false).await;
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch {
                cost_over: Some(2.0),
                ..Default::default()
            },
            action: ApprovalAction::AutoReject,
        }]))
        .await;

    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: Some(7.5),
            meta: None,
        },
    )
    .await
    .unwrap();
    let reason = match &resp {
        PermissionPromptResponse::Deny { message, interrupt } => {
            assert!(!interrupt, "deny should not request interrupt by default");
            message.as_str()
        }
        _ => panic!("cost_estimate=7.5 over threshold=2 must yield Deny: {resp:?}"),
    };
    assert!(
        reason.contains("Bash") && reason.contains("rule"),
        "reason should name the tool and the rule path: {reason}"
    );

    // Verify the denial was logged to events.jsonl so an operator has
    // post-hoc visibility even though the model silently routes around it.
    // Path: <run_subdir>/tasks/<actor_id>/events.jsonl. caller_id is the
    // root lead id ("lead") because PermissionPromptArgs.meta is None and
    // build_caller_identity falls through to the root identity.
    let events_path = state
        .root
        .run_subdir
        .join("tasks")
        .join("lead")
        .join("events.jsonl");
    let body = tokio::fs::read_to_string(&events_path)
        .await
        .unwrap_or_else(|e| panic!("expected {events_path:?} to exist: {e}"));
    assert!(
        body.contains("\"kind\":\"tool_denied\""),
        "events.jsonl missing tool_denied row: {body}"
    );
    assert!(
        body.contains("\"reason_kind\":\"denied_by_rule\""),
        "events.jsonl missing denied_by_rule kind: {body}"
    );
    assert!(
        body.contains("\"tool_name\":\"Bash\""),
        "events.jsonl missing tool_name=Bash: {body}"
    );

    // #367: the rule short-circuit must also record approval state so
    // counters and `approval_driven_termination` work under Path B.
    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("lead")
        .expect("rule-driven deny must bump approval counters for caller");
    assert_eq!(
        entry.approvals_requested, 1,
        "approvals_requested must be 1"
    );
    assert_eq!(entry.approvals_rejected, 1, "approvals_rejected must be 1");
    assert_eq!(
        entry.approvals_approved, 0,
        "approvals_approved must stay 0"
    );
    drop(counters);
    assert_eq!(
        state.approval_driven_termination("lead").await,
        Some(crate::dispatch::state::ApprovalTerminationKind::Rejected),
        "rule-driven deny must register a rejected last_approval_response so a \
         fast-exit worker reclassifies as ApprovalRejected (not Success)"
    );
}

/// #367 regression: the policy-matcher AutoApprove path in
/// `handle_permission_prompt` must record the response and bump
/// approval counters, mirroring `handle_request_approval`. Pre-fix
/// the path returned `decision="allow"` without touching either,
/// silently undercounting Path B approvals.
#[tokio::test]
async fn permission_prompt_rule_auto_approve_records_state_and_counters() {
    use crate::mcp::policy::{ApprovalAction, ApprovalRule, PolicyMatcher};

    // Default Block so the matcher's verdict is the only resolution path.
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::Block, false).await;
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch {
                tool_name: Some("Read".into()),
                ..Default::default()
            },
            action: ApprovalAction::AutoApprove,
        }]))
        .await;

    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "rule-driven approve must yield Allow: {resp:?}"
    );

    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("lead")
        .expect("rule-driven approve must bump approval counters for caller");
    assert_eq!(entry.approvals_requested, 1);
    assert_eq!(entry.approvals_approved, 1);
    assert_eq!(entry.approvals_rejected, 0);
    drop(counters);
    // approve → no termination reclassification.
    assert_eq!(state.approval_driven_termination("lead").await, None);
}

/// #367 regression: the bridge AutoApprove path in
/// `handle_permission_prompt` must record `last_approval_response` so
/// `approval_driven_termination` sees the recent positive decision.
/// The bridge bumps the counters itself; the handler owns the last-
/// response record (mirrors `handle_request_approval`).
#[tokio::test]
async fn permission_prompt_default_policy_auto_approve_records_last_response() {
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoApprove, false).await;

    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "bridge AutoApprove must yield Allow: {resp:?}"
    );

    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("lead")
        .expect("bridge AutoApprove must bump approval counters");
    assert_eq!(entry.approvals_requested, 1);
    assert_eq!(entry.approvals_approved, 1);
    assert_eq!(entry.approvals_rejected, 0);
    drop(counters);
    assert_eq!(
        state.approval_driven_termination("lead").await,
        None,
        "approve outcome must not register as a termination-driving rejection"
    );
}

/// #367 + #373 regression: the bridge AutoReject path in
/// `handle_permission_prompt` must (a) record a rejected
/// `last_approval_response` so termination reclassifies to
/// `ApprovalRejected` (#367), and (b) attribute the denial to
/// `DeniedByPolicy` — NOT `OperatorRejected` — because no operator
/// was involved. Pre-#373 this was misclassified as `operator_rejected`
/// in both the audit log and the model-facing message, causing
/// operators to read audit trails and incorrectly conclude a human
/// clicked reject.
#[tokio::test]
async fn permission_prompt_default_policy_auto_reject_records_denied_by_policy() {
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoReject, false).await;

    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    let reason = match &resp {
        PermissionPromptResponse::Deny { message, interrupt } => {
            assert!(!interrupt, "deny should not request interrupt by default");
            message.as_str()
        }
        _ => panic!("bridge-driven deny must yield Deny: {resp:?}"),
    };
    assert!(
        reason.contains("Bash"),
        "reason should name the tool: {reason}"
    );
    // #373: the message must attribute the denial to the policy, not
    // to a non-existent operator.
    assert!(
        reason.contains("policy"),
        "reason should attribute the denial to the policy, not an \
         operator (#373): {reason}"
    );
    assert!(
        !reason.contains("operator"),
        "reason must NOT claim 'operator' rejected when default policy \
         did (#373): {reason}"
    );

    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("lead")
        .expect("bridge AutoReject must bump approval counters");
    assert_eq!(entry.approvals_requested, 1);
    assert_eq!(entry.approvals_rejected, 1);
    assert_eq!(entry.approvals_approved, 0);
    drop(counters);
    assert_eq!(
        state.approval_driven_termination("lead").await,
        Some(crate::dispatch::state::ApprovalTerminationKind::Rejected),
        "bridge-driven deny must register a rejected last_approval_response"
    );

    // Per-actor events.jsonl audit row should attribute the denial
    // correctly — `denied_by_policy`, not `operator_rejected` (#373).
    let events_path = state
        .root
        .run_subdir
        .join("tasks")
        .join("lead")
        .join("events.jsonl");
    let body = tokio::fs::read_to_string(&events_path)
        .await
        .unwrap_or_else(|e| panic!("expected {events_path:?} to exist: {e}"));
    assert!(
        body.contains("\"reason_kind\":\"denied_by_policy\""),
        "events.jsonl must record reason_kind=denied_by_policy for \
         default-policy auto-reject (#373): {body}"
    );
    assert!(
        !body.contains("\"reason_kind\":\"operator_rejected\""),
        "events.jsonl must NOT misattribute default-policy auto-reject \
         to operator (#373): {body}"
    );
}

/// #377: under `denial_termination_policy = "adapt"` (the post-#377
/// default), `approval_driven_termination` must always return `None`
/// — even immediately after a denied permission_prompt. Trusts the
/// actor's exit code; per-actor `events.jsonl` and the
/// `approvals_rejected` counter remain authoritative for what was
/// blocked. Pre-#377, this scenario reclassified clean exits as
/// `ApprovalRejected` and mislabeled successful adaptation (e.g.
/// "denied Write → fell back to Bash → completed task → exited 0")
/// as failure.
#[tokio::test]
async fn approval_driven_termination_adapt_policy_returns_none_after_denial() {
    let state = mk_plan_state_with_termination_policy(
        crate::dispatch::state::ApprovalPolicy::AutoReject,
        false,
        crate::dispatch::state::DenialTerminationPolicy::Adapt,
    )
    .await;

    // Trigger a real denial via the Path B handler so
    // last_approval_response is populated with approved=false.
    let _ = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();

    // Sanity: the denial WAS recorded — counters and last_approval_response
    // both fired (the Adapt policy does not silence those signals,
    // only the reclassification).
    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("lead")
        .expect("denial must still bump approvals_rejected");
    assert_eq!(entry.approvals_rejected, 1);
    drop(counters);

    // The crux of the assertion: with Adapt policy, no reclassification.
    assert_eq!(
        state.approval_driven_termination("lead").await,
        None,
        "Adapt policy must NEVER return a reclassification kind, even \
         immediately after a denial — the actor's exit code is its \
         terminal status (#377)"
    );
}

/// #377 companion: under `denial_termination_policy = "reclassify"`
/// (the legacy behavior, kept for operators who want fast-give-up
/// distinguished in the status table), the same scenario reclassifies
/// as `Rejected`. This pins both modes against future drift.
#[tokio::test]
async fn approval_driven_termination_reclassify_policy_fires_after_denial() {
    let state = mk_plan_state_with_termination_policy(
        crate::dispatch::state::ApprovalPolicy::AutoReject,
        false,
        crate::dispatch::state::DenialTerminationPolicy::Reclassify,
    )
    .await;

    let _ = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(
        state.approval_driven_termination("lead").await,
        Some(crate::dispatch::state::ApprovalTerminationKind::Rejected),
        "Reclassify policy must surface the recent denial as Rejected"
    );
}

/// #373 companion: ensure the genuine operator-driven rejection path
/// still classifies as `OperatorRejected` (no over-correction). Drives
/// a Block-policy approval, captures the request_id, calls `respond()`
/// with `approved=false` and no comment marker — the only path that
/// should produce `OperatorRejected`.
#[tokio::test]
async fn permission_prompt_operator_driven_reject_records_operator_rejected() {
    use crate::control::protocol::ControlEvent;
    use crate::dispatch::state::{ApprovalPolicy, ApprovalResponse};
    use std::time::Duration;
    use tokio::sync::mpsc;

    let state = mk_plan_state(ApprovalPolicy::Block, false).await;
    let (tx, mut rx) = mpsc::channel::<ControlEvent>(8);
    *state.root.control_writer.lock().await = Some(crate::dispatch::layer::ControlWriterSlot {
        id: uuid::Uuid::now_v7(),
        sender: tx,
    });

    // Driver: wait for the bridge to emit the request, then respond
    // operator-reject with no marker comment (the genuine human-action
    // path — distinct from the bridge's policy auto-reject which sets
    // `BRIDGE_AUTO_REJECT_COMMENT`).
    let state_for_resp = std::sync::Arc::clone(&state);
    let driver = tokio::spawn(async move {
        let request_id = match rx.recv().await.unwrap() {
            ControlEvent::ApprovalRequest { request_id, .. } => request_id,
            other => panic!("unexpected event: {other:?}"),
        };
        let bridge = crate::mcp::approval::ApprovalBridge::new(state_for_resp);
        bridge
            .respond(
                &request_id,
                ApprovalResponse {
                    approved: false,
                    comment: None,
                    edited_summary: None,
                    reason: None,
                    from_ttl: false,
                },
            )
            .await
            .unwrap();
    });

    let resp = tokio::time::timeout(
        Duration::from_secs(2),
        handle_permission_prompt(
            &state,
            PermissionPromptArgs {
                tool_name: "Bash".into(),
                tool_input: None,
                cost_estimate: None,
                meta: None,
            },
        ),
    )
    .await
    .expect("handler must resolve before timeout")
    .unwrap();
    driver.await.unwrap();

    let reason = match &resp {
        PermissionPromptResponse::Deny { message, .. } => message.as_str(),
        _ => panic!("expected Deny: {resp:?}"),
    };
    assert!(
        reason.contains("operator"),
        "operator-driven rejection must attribute to operator: {reason}"
    );
    assert!(
        !reason.contains("policy"),
        "operator-driven rejection must NOT attribute to policy: {reason}"
    );

    let events_path = state
        .root
        .run_subdir
        .join("tasks")
        .join("lead")
        .join("events.jsonl");
    let body = tokio::fs::read_to_string(&events_path)
        .await
        .unwrap_or_else(|e| panic!("expected {events_path:?} to exist: {e}"));
    assert!(
        body.contains("\"reason_kind\":\"operator_rejected\""),
        "events.jsonl must record reason_kind=operator_rejected for \
         genuine operator action: {body}"
    );
    assert!(
        !body.contains("\"reason_kind\":\"denied_by_policy\""),
        "events.jsonl must NOT label operator action as denied_by_policy: {body}"
    );
}

/// #370 (item 3): the `TtlExpired` denial path in
/// `handle_permission_prompt` was untested through #378. Drives a
/// Block-policy approval and responds via `bridge.respond()` with
/// `from_ttl=true` (the same shape the TTL watcher would produce
/// when a queued approval's `ttl_secs` elapses without operator
/// action). Asserts:
///
/// - response carries the canonical TTL-denial message
/// - per-actor `events.jsonl` records `reason_kind=ttl_expired`
/// - under `Reclassify` policy, `approval_driven_termination`
///   returns `TimedOut` (not `Rejected`) — the only path that
///   distinguishes silent-exit-after-TTL from silent-exit-after-deny
///   in `pitboss status` / `summary.json`.
#[tokio::test]
async fn permission_prompt_ttl_driven_reject_records_ttl_expired() {
    use crate::control::protocol::ControlEvent;
    use crate::dispatch::state::{ApprovalPolicy, ApprovalResponse, DenialTerminationPolicy};
    use std::time::Duration;
    use tokio::sync::mpsc;

    // Reclassify policy so the termination assertion is meaningful.
    let state = mk_plan_state_with_termination_policy(
        ApprovalPolicy::Block,
        false,
        DenialTerminationPolicy::Reclassify,
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<ControlEvent>(8);
    *state.root.control_writer.lock().await = Some(crate::dispatch::layer::ControlWriterSlot {
        id: uuid::Uuid::now_v7(),
        sender: tx,
    });

    let state_for_resp = std::sync::Arc::clone(&state);
    let driver = tokio::spawn(async move {
        let request_id = match rx.recv().await.unwrap() {
            ControlEvent::ApprovalRequest { request_id, .. } => request_id,
            other => panic!("unexpected event: {other:?}"),
        };
        let bridge = crate::mcp::approval::ApprovalBridge::new(state_for_resp);
        bridge
            .respond(
                &request_id,
                ApprovalResponse {
                    approved: false,
                    // No comment marker — distinguishes this from
                    // policy auto-reject. TTL is signaled by `from_ttl`.
                    comment: None,
                    edited_summary: None,
                    reason: None,
                    from_ttl: true,
                },
            )
            .await
            .unwrap();
    });

    let resp = tokio::time::timeout(
        Duration::from_secs(2),
        handle_permission_prompt(
            &state,
            PermissionPromptArgs {
                tool_name: "Bash".into(),
                tool_input: None,
                cost_estimate: None,
                meta: None,
            },
        ),
    )
    .await
    .expect("handler must resolve before timeout")
    .unwrap();
    driver.await.unwrap();

    let reason = match &resp {
        PermissionPromptResponse::Deny { message, .. } => message.as_str(),
        _ => panic!("expected Deny: {resp:?}"),
    };
    assert!(
        reason.contains("timed out"),
        "TTL-driven denial must mention timeout in the message: {reason}"
    );

    let events_path = state
        .root
        .run_subdir
        .join("tasks")
        .join("lead")
        .join("events.jsonl");
    let body = tokio::fs::read_to_string(&events_path)
        .await
        .unwrap_or_else(|e| panic!("expected {events_path:?} to exist: {e}"));
    assert!(
        body.contains("\"reason_kind\":\"ttl_expired\""),
        "events.jsonl must record reason_kind=ttl_expired for TTL-driven \
         denial: {body}"
    );

    // Under Reclassify, TTL-driven denial reclassifies as TimedOut,
    // not Rejected — the distinction in `pitboss status` between
    // "operator declined" and "no operator responded in time."
    assert_eq!(
        state.approval_driven_termination("lead").await,
        Some(crate::dispatch::state::ApprovalTerminationKind::TimedOut),
        "TTL denial must reclassify as TimedOut, not Rejected, under \
         Reclassify policy"
    );
}

/// #368: Allow variant must serialize as `{"behavior":"allow", "updatedInput": ...}`
/// when input is provided. `updatedInput` (camelCase) is the upstream
/// SDK field name; `updated_input` (snake_case) would silently fail the
/// gate parser.
#[test]
fn permission_prompt_response_allow_with_input_serializes_canonical_shape() {
    let resp = PermissionPromptResponse::Allow {
        updated_input: Some(serde_json::json!({"command": "ls -la"})),
    };
    let parsed: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(
        parsed["behavior"].as_str(),
        Some("allow"),
        "behavior must be the literal string 'allow' (matches PermissionResultAllow): {parsed}"
    );
    assert_eq!(
        parsed["updatedInput"]["command"].as_str(),
        Some("ls -la"),
        "input must round-trip under camelCase 'updatedInput': {parsed}"
    );
    // No legacy fields — these are what pre-#368 shipped, and would
    // silently fail Claude's parser.
    assert!(parsed.get("decision").is_none(), "no legacy 'decision' key");
    assert!(parsed.get("reason").is_none(), "no legacy 'reason' key");
}

/// #368: Allow without input must omit `updatedInput` entirely (not
/// emit `null`). Some MCP-tool consumers reject explicit nulls.
#[test]
fn permission_prompt_response_allow_without_input_omits_updated_input() {
    let resp = PermissionPromptResponse::Allow {
        updated_input: None,
    };
    let parsed: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(parsed["behavior"].as_str(), Some("allow"));
    assert!(
        parsed.get("updatedInput").is_none(),
        "updatedInput must be omitted, not null: {parsed}"
    );
    // The whole response is essentially a single-key object on the
    // wire when the input isn't being modified.
    let obj = parsed.as_object().unwrap();
    assert_eq!(obj.len(), 1, "expected exactly {{behavior}}, got: {parsed}");
}

/// #368: Deny variant must serialize as `{"behavior":"deny","message":"..."}`
/// — `message` is the upstream SDK field name. `interrupt` defaults to
/// false and is omitted in that case so the wire stays compact.
#[test]
fn permission_prompt_response_deny_serializes_canonical_shape() {
    let resp = PermissionPromptResponse::Deny {
        message: "denied: tool 'Bash' rejected by [[approval_policy]] rule".to_string(),
        interrupt: false,
    };
    let parsed: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(parsed["behavior"].as_str(), Some("deny"));
    assert_eq!(
        parsed["message"].as_str(),
        Some("denied: tool 'Bash' rejected by [[approval_policy]] rule"),
        "message must round-trip under 'message' key: {parsed}"
    );
    assert!(
        parsed.get("interrupt").is_none(),
        "interrupt=false must be omitted to keep wire compact: {parsed}"
    );
    assert!(parsed.get("reason").is_none(), "no legacy 'reason' key");
    assert!(parsed.get("decision").is_none(), "no legacy 'decision' key");
}

/// #368: when interrupt is explicitly set true, it must be serialized
/// (claude treats it as a request to halt the current turn).
#[test]
fn permission_prompt_response_deny_emits_interrupt_when_true() {
    let resp = PermissionPromptResponse::Deny {
        message: "stop".to_string(),
        interrupt: true,
    };
    let parsed: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(parsed["interrupt"].as_bool(), Some(true));
}

/// #368: round-trip the wire shape through serde to catch any tag
/// configuration error. `behavior: "allow"` must deserialize into the
/// Allow variant; `behavior: "deny"` into Deny.
#[test]
fn permission_prompt_response_round_trips_via_canonical_wire() {
    let allow_wire = serde_json::json!({
        "behavior": "allow",
        "updatedInput": {"a": 1}
    });
    let allow: PermissionPromptResponse = serde_json::from_value(allow_wire).unwrap();
    assert!(matches!(
        allow,
        PermissionPromptResponse::Allow {
            updated_input: Some(_)
        }
    ));

    let deny_wire = serde_json::json!({
        "behavior": "deny",
        "message": "no",
        "interrupt": true
    });
    let deny: PermissionPromptResponse = serde_json::from_value(deny_wire).unwrap();
    match deny {
        PermissionPromptResponse::Deny { message, interrupt } => {
            assert_eq!(message, "no");
            assert!(interrupt);
        }
        _ => panic!("expected Deny"),
    }
}

#[tokio::test]
async fn propose_plan_auto_reject_leaves_flag_false() {
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoReject, true).await;
    let resp = handle_propose_plan(
        &state,
        ProposePlanArgs {
            plan: ApprovalPlan {
                summary: "phase-1".into(),
                ..Default::default()
            },
            timeout_secs: Some(2),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!resp.approved);
    assert!(
        !state
            .root
            .plan_approved
            .load(std::sync::atomic::Ordering::Acquire),
        "rejected plan must not flip plan_approved — lead should be able to retry"
    );
}

// ----------------------------------------------------------------------
// #252 — typed worker profiles, dispatcher-side enforcement
// ----------------------------------------------------------------------

/// Build a `DispatchState` with the given worker_types profiles and
/// `require_actor_type` flag. Mirrors `test_state` but lets the caller
/// inject typed profile config without forking the whole helper.
async fn test_state_with_worker_types(
    worker_types: Vec<crate::manifest::schema::WorkerType>,
    require_actor_type: bool,
) -> Arc<DispatchState> {
    test_state_with_worker_types_full(
        worker_types,
        require_actor_type,
        ApprovalPolicy::Block,
        crate::manifest::schema::UntypedActorPolicy::Bridge,
    )
    .await
}

async fn test_state_with_worker_types_and_policy(
    worker_types: Vec<crate::manifest::schema::WorkerType>,
    require_actor_type: bool,
    approval_policy: ApprovalPolicy,
) -> Arc<DispatchState> {
    test_state_with_worker_types_full(
        worker_types,
        require_actor_type,
        approval_policy,
        crate::manifest::schema::UntypedActorPolicy::Bridge,
    )
    .await
}

async fn test_state_with_worker_types_full(
    worker_types: Vec<crate::manifest::schema::WorkerType>,
    require_actor_type: bool,
    approval_policy: ApprovalPolicy,
    untyped_actor_policy: crate::manifest::schema::UntypedActorPolicy,
) -> Arc<DispatchState> {
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "lead prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: Default::default(),
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        default_approval_policy: None,
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types,
        sublead_types: vec![],
        require_actor_type,
        untyped_actor_policy,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let run_id = Uuid::now_v7();
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    let dir_path = dir.path().to_path_buf();
    std::mem::forget(dir);
    let _ = dir_path;
    Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        approval_policy,
        None,
        std::sync::Arc::new(crate::shared_store::SharedStore::new()),
    ))
}

fn extraction_profile() -> crate::manifest::schema::WorkerType {
    crate::manifest::schema::WorkerType {
        id: "extraction".into(),
        tools: vec!["Read".into(), "Glob".into(), "Grep".into()],
        allowed_models: vec!["claude-haiku-4-5".into()],
        max_timeout_secs: Some(900),
    }
}

#[tokio::test]
async fn spawn_worker_rejects_unknown_worker_type() {
    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        worker_type: Some("nope".into()),
        ..Default::default()
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown worker_type"), "{msg}");
    assert!(msg.contains("extraction"), "{msg}");
}

#[tokio::test]
async fn spawn_worker_rejects_tool_outside_profile() {
    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        worker_type: Some("extraction".into()),
        tools: Some(vec!["Read".into(), "Bash".into()]),
        ..Default::default()
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("\"Bash\""), "{msg}");
    assert!(msg.contains("worker_type \"extraction\""), "{msg}");
}

#[tokio::test]
async fn spawn_worker_rejects_model_outside_allowlist() {
    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        worker_type: Some("extraction".into()),
        model: Some("claude-opus-4-7".into()),
        ..Default::default()
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("claude-opus-4-7"), "{msg}");
    assert!(msg.contains("allowed_models"), "{msg}");
}

#[tokio::test]
async fn spawn_worker_rejects_typeless_when_required() {
    let state = test_state_with_worker_types(vec![extraction_profile()], true).await;
    let args = SpawnWorkerArgs {
        prompt: "p".into(),
        worker_type: None,
        ..Default::default()
    };
    let err = handle_spawn_worker(&state, args).await.unwrap_err();
    assert!(err.to_string().contains("require_actor_type"));
}

#[tokio::test]
async fn spawn_worker_typed_subset_succeeds() {
    let state = test_state_with_worker_types(vec![extraction_profile()], true).await;
    let args = SpawnWorkerArgs {
        prompt: "investigate".into(),
        directory: Some("/tmp".into()),
        worker_type: Some("extraction".into()),
        tools: Some(vec!["Read".into(), "Glob".into()]),
        model: Some("claude-haiku-4-5".into()),
        ..Default::default()
    };
    let res = handle_spawn_worker(&state, args).await.unwrap();
    assert!(res.task_id.starts_with("worker-"));
}

/// End-to-end check that `actor_type` reaches the persisted record.
/// The helper's `FakeSpawner` holds workers in Running until signaled;
/// we drive the worker to Done by terminating its cancel token, then
/// poll briefly for the `WorkerState::Done(rec)` whose `actor_type`
/// is the source of truth for `summary.jsonl` and downstream
/// consumers. Earlier rejection tests only prove the guard rejects
/// invalid spawns; this is the test that pins the spawn →
/// `run_worker` → `TaskRecord` plumbing.
#[tokio::test]
async fn spawn_worker_typed_persists_actor_type_on_record() {
    let state = test_state_with_worker_types(vec![extraction_profile()], true).await;
    let args = SpawnWorkerArgs {
        prompt: "investigate".into(),
        directory: Some("/tmp".into()),
        worker_type: Some("extraction".into()),
        tools: Some(vec!["Read".into(), "Glob".into()]),
        model: Some("claude-haiku-4-5".into()),
        ..Default::default()
    };
    let res = handle_spawn_worker(&state, args).await.unwrap();

    // Drive the worker to terminate so `run_worker` writes the record.
    // The helper's FakeSpawner is `hold_until_signal`; without this,
    // the worker stays Running and the test would just timeout.
    for _ in 0..50 {
        if let Some(tok) = state
            .root
            .worker_cancels
            .read()
            .await
            .get(&res.task_id)
            .cloned()
        {
            tok.terminate();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Bounded poll for the Done(rec). If the writer ever silently
    // failed to land the record, we want the test to fail loudly
    // rather than retry forever.
    let mut found_actor_type: Option<String> = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if let Some(WorkerState::Done(rec)) = state.root.workers.read().await.get(&res.task_id) {
            found_actor_type = rec.actor_type.clone();
            break;
        }
    }
    assert_eq!(
        found_actor_type.as_deref(),
        Some("extraction"),
        "TaskRecord.actor_type must reflect the resolved [[worker_type]] id"
    );
}

// ---------------------------------------------------------------------------
// Profile-driven Path-B short-circuit (#252).
// ---------------------------------------------------------------------------
//
// `handle_permission_prompt` consults the caller's `[[worker_type]]` /
// `[[sublead_type]]` profile after any `[[approval_policy]]` rule has
// run: rule auto-reject / auto-approve still wins (operator override),
// but on `Block | None` the profile's `tools` allowlist becomes the
// deciding signal — membership ⇒ Allow, absence ⇒ Deny — without an
// operator round-trip. Untyped callers (lead, or pre-#252 manifests)
// fall through to the bridge unchanged.

/// Helper for the profile tests below: inserts a typed worker entry
/// into the root layer's `worker_actor_types` map. Mirrors what
/// `handle_spawn_worker` does on a real spawn, without driving the
/// FakeSpawner — the approval path only reads the maps.
async fn insert_typed_worker(state: &Arc<DispatchState>, worker_id: &str, worker_type_id: &str) {
    state
        .root
        .worker_actor_types
        .write()
        .await
        .insert(worker_id.to_string(), worker_type_id.to_string());
    // Mark as a root-layer worker for the layer-index lookup.
    state
        .worker_layer_index
        .write()
        .await
        .insert(worker_id.to_string(), None);
}

fn worker_meta(worker_id: &str) -> crate::shared_store::tools::MetaField {
    crate::shared_store::tools::MetaField {
        actor_id: worker_id.to_string(),
        actor_role: crate::shared_store::ActorRole::Worker,
    }
}

#[tokio::test]
async fn permission_prompt_profile_auto_approves_tool_in_allowlist() {
    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    insert_typed_worker(&state, "worker-1", "extraction").await;

    // `Read` is in `extraction_profile()` → Allow without bridge.
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-1")),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "profile-allowlisted tool must auto-approve: {resp:?}"
    );

    // Counters bumped — symmetric with rule-driven AutoApprove (#367).
    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("worker-1")
        .expect("profile auto-approve must bump approval counters");
    assert_eq!(entry.approvals_requested, 1);
    assert_eq!(entry.approvals_approved, 1);
    assert_eq!(entry.approvals_rejected, 0);
}

#[tokio::test]
async fn permission_prompt_profile_auto_denies_tool_outside_allowlist() {
    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    insert_typed_worker(&state, "worker-1", "extraction").await;

    // `Bash` is NOT in `extraction_profile()` → Deny + DeniedByProfile.
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-1")),
        },
    )
    .await
    .unwrap();
    let PermissionPromptResponse::Deny { message, .. } = resp else {
        panic!("profile-disallowed tool must auto-deny");
    };
    assert!(
        message.contains("not in worker_type 'extraction' allowlist"),
        "model-facing reason must name the profile so claude can adapt: {message}"
    );

    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("worker-1")
        .expect("profile auto-deny must bump approval counters");
    assert_eq!(entry.approvals_requested, 1);
    assert_eq!(entry.approvals_approved, 0);
    assert_eq!(entry.approvals_rejected, 1);
}

/// Untyped workers (no entry in `worker_actor_types`) MUST fall through
/// to the bridge so back-compat with v0.9 manifests is preserved.
/// Verified by setting the bridge's default policy to AutoApprove and
/// confirming the call returns Allow — if the profile path had
/// mistakenly fired, the call would have hit the lead-caller fallthrough
/// and either been auto-rejected or stalled on the bridge with no TUI.
#[tokio::test]
async fn permission_prompt_untyped_worker_falls_through_to_bridge() {
    let state = mk_plan_state(crate::dispatch::state::ApprovalPolicy::AutoApprove, false).await;
    // Note: NOT calling insert_typed_worker. Caller is treated as the
    // root lead by build_caller_identity (no _meta) — same un-typed code
    // path as a Lead caller.
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "untyped caller must reach the bridge AutoApprove path: {resp:?}"
    );
}

/// Lead callers MUST NOT short-circuit on profile, even when typed
/// `[[worker_type]]` entries exist in the manifest. The root lead is
/// never typed (#252 Phase 1.5: "Root lead is never typed") and an
/// off-by-one that wired `worker_actor_types` lookups for `ActorRole::Lead`
/// would silently apply a worker profile's caps to the lead — which
/// would forbid orchestration tools the lead must always have.
#[tokio::test]
async fn permission_prompt_lead_caller_never_uses_profile_path() {
    // AutoApprove policy so the bridge fast-paths to Allow instead of
    // waiting for the (no-TUI) operator response and TTL'ing — the
    // assertion is about which arm the caller takes, not the wait.
    let state = test_state_with_worker_types_and_policy(
        vec![extraction_profile()],
        false,
        ApprovalPolicy::AutoApprove,
    )
    .await;
    // Typed worker entries exist but the caller's role is Lead — the
    // profile lookup must short-circuit at the `ActorRole::Lead` arm.
    // If it didn't, looking up `"lead"` in `worker_actor_types` would
    // find the planted "extraction" entry and apply a worker profile
    // to the lead — forbidding orchestration tools the lead must
    // always have.
    state
        .root
        .worker_actor_types
        .write()
        .await
        .insert("lead".into(), "extraction".into());

    let lead_meta = crate::shared_store::tools::MetaField {
        actor_id: "lead".into(),
        actor_role: crate::shared_store::ActorRole::Lead,
    };

    // `Bash` is NOT in `extraction_profile()`. If lead were treated as
    // typed, this would auto-deny via DeniedByProfile. Instead the
    // ActorRole::Lead arm of `caller_profile` returns None and the
    // call falls through to the bridge AutoApprove fast-path → Allow.
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(lead_meta),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "lead caller must skip profile lookup and reach bridge fast-path: {resp:?}"
    );
}

/// Order-of-evaluation contract: an operator-declared `[[approval_policy]]
/// action = "auto_reject"` rule must fire BEFORE the profile short-circuit,
/// so an operator can ban a tool globally regardless of any profile that
/// would otherwise allow it. Without this guarantee, a permissive profile
/// silently widens the operator's safety belt.
#[tokio::test]
async fn permission_prompt_rule_auto_reject_beats_profile_allow() {
    use crate::mcp::policy::{ApprovalAction, ApprovalRule, PolicyMatcher};

    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    insert_typed_worker(&state, "worker-1", "extraction").await;
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch {
                tool_name: Some("Read".into()),
                ..Default::default()
            },
            action: ApprovalAction::AutoReject,
        }]))
        .await;

    // `Read` IS in the profile (would Allow), but the rule auto-rejects
    // first → Deny + DeniedByRule (NOT DeniedByProfile).
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-1")),
        },
    )
    .await
    .unwrap();
    let PermissionPromptResponse::Deny { message, .. } = resp else {
        panic!("rule auto-reject must beat profile allow");
    };
    assert!(
        message.contains("[[approval_policy]] rule"),
        "denial must be attributed to the rule, not the profile: {message}"
    );
}

/// Symmetric to `rule_auto_reject_beats_profile_allow`: an operator
/// `auto_approve` rule beats a profile that would deny. This is the
/// cross-cutting "operator can widen if they explicitly want" path —
/// rare in practice, but the rule machinery has been the override
/// surface since pre-#252 and we don't quietly take it away.
#[tokio::test]
async fn permission_prompt_rule_auto_approve_beats_profile_deny() {
    use crate::mcp::policy::{ApprovalAction, ApprovalRule, PolicyMatcher};

    let state = test_state_with_worker_types(vec![extraction_profile()], false).await;
    insert_typed_worker(&state, "worker-1", "extraction").await;
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch {
                tool_name: Some("Bash".into()),
                ..Default::default()
            },
            action: ApprovalAction::AutoApprove,
        }]))
        .await;

    // `Bash` is NOT in the profile (would Deny), but the rule
    // auto-approves first → Allow.
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Bash".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-1")),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "rule auto-approve must beat profile deny: {resp:?}"
    );
}

// ---------------------------------------------------------------------------
// Synthetic-default profile under [run].untyped_actor_policy = "block".
// ---------------------------------------------------------------------------
//
// The block policy converts the un-typed bridge fallback (today's
// pre-#252 default) into auto-deny via a synthesized empty profile.
// Behavior matrix:
//
//   actor typed?  policy=bridge  policy=block
//   ------------  -------------  ----------------
//   yes           profile fires  profile fires
//   no            bridge         synthetic deny

/// Helper for `block`-policy tests. Mirrors
/// `test_state_with_worker_types` but pivots the manifest's
/// `untyped_actor_policy` to `Block`. Validate-time rejects the
/// empty-profiles + block combination, so every test reaching this
/// helper declares at least one profile.
async fn test_state_block_untyped(
    worker_types: Vec<crate::manifest::schema::WorkerType>,
) -> Arc<DispatchState> {
    test_state_with_worker_types_full(
        worker_types,
        false,
        ApprovalPolicy::Block,
        crate::manifest::schema::UntypedActorPolicy::Block,
    )
    .await
}

#[tokio::test]
async fn permission_prompt_synthetic_default_blocks_untyped_worker() {
    let state = test_state_block_untyped(vec![extraction_profile()]).await;
    // No `insert_typed_worker` call — the worker is un-typed, so the
    // synthetic empty profile must fire.
    state
        .worker_layer_index
        .write()
        .await
        .insert("worker-untyped".into(), None);

    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-untyped")),
        },
    )
    .await
    .unwrap();
    let PermissionPromptResponse::Deny { message, .. } = resp else {
        panic!("untyped worker under block policy must auto-deny: {resp:?}");
    };
    // The sentinel id `<synthetic>` shows up in the message so an
    // operator reading the model-facing reason knows this denial
    // came from the synthesized profile, not a declared one.
    assert!(
        message.contains("worker_type '<synthetic>'"),
        "synthetic denial must name the sentinel profile id: {message}"
    );

    let counters = state.root.worker_counters.read().await;
    let entry = counters
        .get("worker-untyped")
        .expect("synthetic auto-deny must bump approval counters");
    assert_eq!(entry.approvals_requested, 1);
    assert_eq!(entry.approvals_rejected, 1);
}

/// Declared profiles still win under `block`. Strict mode is meant to
/// close the un-typed escape hatch — it must not perturb the typed
/// fast-path that #388 shipped.
#[tokio::test]
async fn permission_prompt_synthetic_default_does_not_override_declared_profile() {
    let state = test_state_block_untyped(vec![extraction_profile()]).await;
    insert_typed_worker(&state, "worker-typed", "extraction").await;

    // `Read` is in `extraction_profile()` → Allow via declared profile,
    // not the synthetic empty one.
    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-typed")),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "declared profile's allowlist must still fire under block: {resp:?}"
    );
}

/// Bridge policy (the default) preserves the pre-#252 un-typed fallback
/// — un-typed callers route through the bridge instead of auto-denying.
/// Symmetric with `permission_prompt_untyped_worker_falls_through_to_bridge`,
/// but anchored in the synthetic-default test block so a future
/// refactor that flips the default catches the regression.
#[tokio::test]
async fn permission_prompt_bridge_policy_keeps_untyped_bridge_fallback() {
    let state = test_state_with_worker_types_and_policy(
        vec![extraction_profile()],
        false,
        ApprovalPolicy::AutoApprove,
    )
    .await;
    // Default `untyped_actor_policy = Bridge` — the helper does NOT
    // patch it.
    state
        .worker_layer_index
        .write()
        .await
        .insert("worker-untyped".into(), None);

    let resp = handle_permission_prompt(
        &state,
        PermissionPromptArgs {
            tool_name: "Read".into(),
            tool_input: None,
            cost_estimate: None,
            meta: Some(worker_meta("worker-untyped")),
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(resp, PermissionPromptResponse::Allow { .. }),
        "bridge policy must still route un-typed callers to the bridge: {resp:?}"
    );
}
