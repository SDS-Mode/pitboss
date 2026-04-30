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
        provider: pitboss_core::provider::Provider::Anthropic,
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
        max_parallel_tasks: 4,
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
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        lifecycle: None,
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
fn worker_spawn_args_passes_dangerously_skip_permissions() {
    // Companion to the runner-side test: worker spawns are emitted from
    // a different module and must independently pass the flag. Without
    // it, headless workers stall on bash-with-`$VAR` and write-outside-
    // cwd gates that have no `-p`-mode answer — the failure mode is
    // silent (worker exits "successfully" having written nothing).
    use std::path::PathBuf;
    let argv = worker_spawn_args(
        "p",
        "claude-haiku-4-5",
        &["Read".to_string()],
        Some(&PathBuf::from("/tmp/cfg.json")),
        Default::default(),
    );
    assert!(
        argv.iter().any(|a| a == "--dangerously-skip-permissions"),
        "worker_spawn_args missing --dangerously-skip-permissions: {argv:?}"
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
            provider: pitboss_core::provider::Provider::Anthropic,
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
            provider: pitboss_core::provider::Provider::Anthropic,
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
        provider: pitboss_core::provider::Provider::Anthropic,
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
        max_parallel_tasks: 4,
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
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        lifecycle: None,
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
        provider: pitboss_core::provider::Provider::Anthropic,
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
        provider: pitboss_core::provider::Provider::Anthropic,
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
        max_parallel_tasks: 4,
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
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        lifecycle: None,
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
        provider: pitboss_core::provider::Provider::Anthropic,
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
        max_parallel_tasks: 4,
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
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        lifecycle: None,
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
    assert_eq!(resp.decision, "allow", "auto-approve should yield allow");
    assert_eq!(
        resp.behavior.as_deref(),
        Some("allow_once"),
        "allow_once behavior expected"
    );
}

/// Build a `DispatchState` with the specified approval policy and
/// `require_plan_approval` flag. Mirrors `handle_request_approval_auto_approves`
/// test scaffolding but parameterized so plan-approval tests can share it.
async fn mk_plan_state(
    policy: crate::dispatch::state::ApprovalPolicy,
    require_plan_approval: bool,
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
        provider: pitboss_core::provider::Provider::Anthropic,
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
        max_parallel_tasks: 4,
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
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        lifecycle: None,
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
    assert_eq!(
        resp.decision, "deny",
        "cost_estimate=7.5 over threshold=2 must yield deny"
    );
    assert!(resp.behavior.is_none(), "deny carries no behavior field");
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
