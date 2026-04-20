//! End-to-end hierarchical integration tests. Unlike `hierarchical_flows.rs`
//! which drives the MCP server directly as a client (via FakeMcpClient),
//! these tests spawn fake-claude as a real subprocess via SessionHandle +
//! TokioSpawner. The subprocess connects to the MCP socket directly and
//! issues real tool calls via its mcp_call script action.
//!
//! Decisions locked in
//! docs/superpowers/specs/2026-04-17-pitboss-v041-fake-claude-mcp-e2e-design.md.

#![allow(dead_code)]
#![allow(unused_imports)] // Helpers + types re-used by Tasks 7-10.

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
use pitboss_cli::mcp::{socket_path_for_run, McpServer};
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::{ChildProcess, ProcessSpawner, SpawnCmd, TokioSpawner};
use pitboss_core::session::{CancelToken, SessionHandle, SessionOutcome};
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use uuid::Uuid;

use support::fake_claude_path;

/// Build a DispatchState configured with a FakeSpawner producing short,
/// successful worker runs. Lead is NOT run here — callers spawn fake-claude
/// themselves via a separate TokioSpawner.
fn mk_state(
    run_dir: &std::path::Path,
    approval_policy: ApprovalPolicy,
) -> (Uuid, Arc<DispatchState>) {
    let run_id = Uuid::now_v7();
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
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: run_dir.to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: Some(approval_policy),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.to_path_buf()));
    // Worker-side spawner: emits a complete stream-json run then exits 0.
    let worker_script = FakeScript::new()
        .stdout_line(r#"{"type":"system","subtype":"init","session_id":"worker-sess"}"#)
        .stdout_line(
            r#"{"type":"result","session_id":"worker-sess","usage":{"input_tokens":10,"output_tokens":20}}"#,
        )
        .exit_code(0);
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(worker_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = run_dir.join(run_id.to_string());
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
        approval_policy,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    (run_id, state)
}

/// Build + spawn fake-claude as the lead subprocess, connecting to `mcp_sock`.
/// Returns the SessionOutcome.
async fn run_fake_claude_lead(
    cwd: &std::path::Path,
    script_path: &std::path::Path,
    mcp_sock: &std::path::Path,
    cancel: CancelToken,
    timeout: Duration,
) -> SessionOutcome {
    run_fake_claude_lead_impl(cwd, script_path, mcp_sock, None, cancel, timeout).await
}

/// Variant of `run_fake_claude_lead` that wires fake-claude through a
/// spawned `pitboss mcp-bridge` subprocess instead of connecting to the
/// socket directly. `actor_id` / `actor_role` get injected into every
/// `tools/call` via `_meta` by the bridge.
async fn run_fake_claude_lead_via_bridge(
    cwd: &std::path::Path,
    script_path: &std::path::Path,
    mcp_sock: &std::path::Path,
    actor_id: &str,
    actor_role: &str,
    cancel: CancelToken,
    timeout: Duration,
) -> SessionOutcome {
    run_fake_claude_lead_impl(
        cwd,
        script_path,
        mcp_sock,
        Some((
            support::pitboss_binary(),
            actor_id.into(),
            actor_role.into(),
        )),
        cancel,
        timeout,
    )
    .await
}

async fn run_fake_claude_lead_impl(
    cwd: &std::path::Path,
    script_path: &std::path::Path,
    mcp_sock: &std::path::Path,
    bridge: Option<(PathBuf, String, String)>,
    cancel: CancelToken,
    timeout: Duration,
) -> SessionOutcome {
    let mut env = HashMap::new();
    env.insert(
        "PITBOSS_FAKE_SCRIPT".to_string(),
        script_path.to_string_lossy().to_string(),
    );
    env.insert(
        "PITBOSS_FAKE_MCP_SOCKET".to_string(),
        mcp_sock.to_string_lossy().to_string(),
    );
    if let Some((pitboss_bin, actor_id, actor_role)) = bridge {
        env.insert(
            "PITBOSS_FAKE_MCP_BRIDGE_CMD".to_string(),
            pitboss_bin.to_string_lossy().to_string(),
        );
        env.insert("PITBOSS_FAKE_ACTOR_ID".to_string(), actor_id);
        env.insert("PITBOSS_FAKE_ACTOR_ROLE".to_string(), actor_role);
    }
    let cmd = SpawnCmd {
        program: fake_claude_path(),
        args: vec![],
        cwd: cwd.to_path_buf(),
        env,
    };
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    SessionHandle::new("lead", spawner, cmd)
        .run_to_completion(cancel, timeout)
        .await
}

#[tokio::test]
async fn fake_claude_smoke_prints_version_stderr() {
    // Sanity-check that the fake-claude binary was built and can run without
    // the MCP env vars set. Uses --version fast-path which prints to stdout
    // and exits 0, doesn't touch any script.
    support::ensure_built();
    let output = std::process::Command::new(fake_claude_path())
        .arg("--version")
        .output()
        .expect("exec fake-claude --version");
    assert!(
        output.status.success(),
        "fake-claude --version failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fake-claude"),
        "expected --version output to mention fake-claude, got: {stdout:?}"
    );
}

#[tokio::test]
async fn e2e_lead_spawns_worker_via_real_subprocess() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let (run_id, state) = mk_state(dir.path(), ApprovalPolicy::Block);

    // Start the MCP server so fake-claude's mcp_call can land somewhere.
    let sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _server = McpServer::start(sock.clone(), state.clone()).await.unwrap();

    // Write the script. spawn_worker returns {task_id: "worker-..."},
    // stored under "w1"; wait_for_worker then consumes $w1.task_id.
    let script = dir.path().join("script.jsonl");
    // Lead emits init + result stream-json so SessionHandle sees a clean
    // Completed outcome (saw_result=true). The mcp_call actions between
    // them exercise the actual MCP flow.
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"hi"},"bind":"w1"}}
{"mcp_call":{"name":"wait_for_worker","args":{"task_id":"$w1.task_id"},"bind":"rec"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    // Run fake-claude as the lead. A real TokioSpawner subprocess, not the
    // FakeSpawner in state.spawner (which backs the workers it spawns).
    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    // Subprocess must have exited cleanly.
    assert_eq!(
        outcome.exit_code,
        Some(0),
        "fake-claude exited non-zero: outcome={outcome:?}"
    );
    assert!(matches!(
        outcome.final_state,
        pitboss_core::session::SessionState::Completed
    ));

    // Worker state should be Done with a captured session_id.
    let workers = state.workers.read().await;
    assert_eq!(
        workers.len(),
        1,
        "expected exactly one worker, got {}",
        workers.len()
    );
    let (task_id, w) = workers.iter().next().unwrap();
    match w {
        pitboss_cli::dispatch::state::WorkerState::Done(rec) => {
            assert_eq!(&rec.task_id, task_id);
            assert!(rec.claude_session_id.is_some(), "session_id not captured");
        }
        other => panic!("expected Done, got {other:?}"),
    }

    // Explicit cleanup: cancel the run token so any stray tasks exit.
    state.cancel.terminate();
}

#[tokio::test]
async fn e2e_lead_spawns_three_workers_and_waits_for_any() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let (run_id, state) = mk_state(dir.path(), ApprovalPolicy::Block);

    let sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _server = McpServer::start(sock.clone(), state.clone()).await.unwrap();

    // Three spawn_workers then one wait_for_any. Workers complete quickly
    // under the FakeSpawner so wait_for_any resolves once the first exits.
    let script = dir.path().join("script.jsonl");
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"a"},"bind":"w1"}}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"b"},"bind":"w2"}}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"c"},"bind":"w3"}}
{"mcp_call":{"name":"wait_for_any","args":{"task_ids":["$w1.task_id","$w2.task_id","$w3.task_id"]},"bind":"first"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(outcome.exit_code, Some(0), "exit non-zero: {outcome:?}");
    assert!(matches!(
        outcome.final_state,
        pitboss_core::session::SessionState::Completed
    ));

    // All 3 workers should be registered (at least 1 Done; the rest can be
    // Done or Running depending on timing).
    let workers = state.workers.read().await;
    assert_eq!(
        workers.len(),
        3,
        "expected 3 workers, got {}",
        workers.len()
    );

    let done_count = workers
        .values()
        .filter(|w| matches!(w, pitboss_cli::dispatch::state::WorkerState::Done(_)))
        .count();
    assert!(
        done_count >= 1,
        "expected at least one Done worker after wait_for_any, got {done_count}"
    );

    state.cancel.terminate();
}

#[tokio::test]
async fn e2e_lead_cancels_worker_mid_flight() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();

    // Custom state: worker script holds until signal so cancel has
    // something to actually cancel.
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
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let hold_script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(hold_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
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
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    let sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _server = McpServer::start(sock.clone(), state.clone()).await.unwrap();

    // spawn_worker (worker hangs), sleep for slot fill, cancel_worker, list.
    let script = dir.path().join("script.jsonl");
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"hi"},"bind":"w1"}}
{"sleep_ms":200}
{"mcp_call":{"name":"cancel_worker","args":{"task_id":"$w1.task_id"},"bind":"cancel_res"}}
{"mcp_call":{"name":"list_workers","args":{},"bind":"snapshot"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(outcome.exit_code, Some(0), "exit non-zero: {outcome:?}");

    // Give the backgrounded worker a moment to finalize as Cancelled after
    // receiving the terminate signal from cancel_worker.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Worker should now be Cancelled.
    let workers = state.workers.read().await;
    let (_, w) = workers.iter().next().expect("at least one worker");
    match w {
        pitboss_cli::dispatch::state::WorkerState::Done(rec) => {
            assert_eq!(
                rec.status,
                pitboss_core::store::TaskStatus::Cancelled,
                "expected Cancelled status, got {:?}",
                rec.status
            );
        }
        other => panic!("expected Done(Cancelled), got {other:?}"),
    }

    state.cancel.terminate();
}

#[tokio::test]
async fn e2e_lead_request_approval_round_trip() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let (run_id, state) = mk_state(dir.path(), ApprovalPolicy::Block);

    // Ensure the run subdir exists so events.jsonl writes don't fail.
    tokio::fs::create_dir_all(&state.run_subdir).await.unwrap();

    // Start BOTH the MCP server (for the lead) and the Control server
    // (for FakeControlClient).
    let mcp_sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _mcp_server = McpServer::start(mcp_sock.clone(), state.clone())
        .await
        .unwrap();

    let ctrl_sock = pitboss_cli::control::control_socket_path(run_id, &state.manifest.run_dir);
    let _ctrl_server = pitboss_cli::control::server::start_control_server(
        ctrl_sock.clone(),
        "0.4.1".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();

    // Background task: poll until there's a queued approval so we don't race
    // the lead. When queue non-empty, connect the FakeControlClient (which
    // triggers the server-side drain + ApprovalRequest push). FCC responds
    // with Approve{approved:true}.
    let ctrl_sock_bg = ctrl_sock.clone();
    let state_for_fcc = state.clone();
    let fcc_task = tokio::spawn(async move {
        // Poll up to 2s for the approval_queue to fill.
        let poll_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if !state_for_fcc.approval_queue.lock().await.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= poll_deadline {
                panic!("FCC timed out waiting for queued approval");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut client =
            fake_control_client::FakeControlClient::connect(&ctrl_sock_bg, "0.4.1-fcc")
                .await
                .unwrap();
        // First event should be the drained ApprovalRequest.
        match client.recv_timeout(Duration::from_secs(5)).await.unwrap() {
            Some(pitboss_cli::control::protocol::ControlEvent::ApprovalRequest {
                request_id,
                ..
            }) => {
                client
                    .send(&pitboss_cli::control::protocol::ControlOp::Approve {
                        request_id,
                        approved: true,
                        comment: None,
                        edited_summary: None,
                        reason: None,
                    })
                    .await
                    .unwrap();
            }
            Some(other) => panic!("expected ApprovalRequest first, got {other:?}"),
            None => panic!("FakeControlClient timed out waiting for event"),
        }
    });

    // Script: one request_approval call.
    let script = dir.path().join("script.jsonl");
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"request_approval","args":{"summary":"spawn 3 workers","timeout_secs":10},"bind":"approval"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &mcp_sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    // Clean up the background FCC task (should have finished by now).
    fcc_task.await.expect("FCC task panicked");

    assert_eq!(
        outcome.exit_code,
        Some(0),
        "fake-claude exit non-zero: {outcome:?}"
    );

    // Check events.jsonl for both approval_request and approval_response.
    let events_path = state
        .run_subdir
        .join("tasks")
        .join("lead")
        .join("events.jsonl");
    let events = tokio::fs::read_to_string(&events_path)
        .await
        .unwrap_or_else(|e| {
            panic!("read {}: {e}", events_path.display());
        });
    assert!(
        events.contains("\"kind\":\"approval_request\""),
        "events.jsonl missing approval_request: {events}"
    );
    assert!(
        events.contains("\"kind\":\"approval_response\""),
        "events.jsonl missing approval_response: {events}"
    );

    // Counters should record one request + one approval.
    let counters = state
        .worker_counters
        .read()
        .await
        .get("lead")
        .cloned()
        .unwrap_or_default();
    assert_eq!(counters.approvals_requested, 1);
    assert_eq!(counters.approvals_approved, 1);
    assert_eq!(counters.approvals_rejected, 0);

    state.cancel.terminate();
}

#[tokio::test]
async fn e2e_lead_reprompts_running_worker() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();

    // Custom state: worker script holds until signal so the reprompt has
    // something mid-flight to redirect.
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
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    // Worker script emits init+result so session_id gets captured via the
    // promote_task mpsc channel, THEN holds so the worker stays Running
    // when reprompt_worker runs (reprompt requires session_id).
    let hold_script = FakeScript::new()
        .stdout_line(r#"{"type":"system","subtype":"init","session_id":"worker-sess"}"#)
        .stdout_line(
            r#"{"type":"result","session_id":"worker-sess","usage":{"input_tokens":1,"output_tokens":1}}"#,
        )
        .hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(hold_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
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
        run_subdir.clone(),
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    let sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _server = McpServer::start(sock.clone(), state.clone()).await.unwrap();

    // Spawn a worker, sleep for init+result to land (session_id captured),
    // reprompt it.
    let script = dir.path().join("script.jsonl");
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"original"},"bind":"w1"}}
{"sleep_ms":200}
{"mcp_call":{"name":"reprompt_worker","args":{"task_id":"$w1.task_id","prompt":"reconsider"},"bind":"rep"}}
{"sleep_ms":100}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(outcome.exit_code, Some(0), "exit non-zero: {outcome:?}");

    // Extract the worker's task_id for subsequent assertions.
    let task_id = {
        let workers = state.workers.read().await;
        workers.keys().next().cloned().expect("at least one worker")
    };

    // events.jsonl should contain a reprompt entry.
    let events_path = run_subdir.join("tasks").join(&task_id).join("events.jsonl");
    let events = tokio::fs::read_to_string(&events_path).await.unwrap();
    assert!(
        events.contains("\"kind\":\"reprompt\""),
        "events.jsonl missing reprompt: {events}"
    );

    // Counter bumped.
    let counters = state
        .worker_counters
        .read()
        .await
        .get(&task_id)
        .cloned()
        .unwrap_or_default();
    assert_eq!(counters.reprompt_count, 1);

    state.cancel.terminate();
}

#[tokio::test]
async fn e2e_run_finished_notifies_webhook() {
    support::ensure_built();

    // wiremock server captures one POST matching the RunFinished shape.
    let mock = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::body_partial_json(
            serde_json::json!({ "event": { "kind": "run_finished" } }),
        ))
        .respond_with(wiremock::ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock)
        .await;
    let webhook_url = format!("{}/hook", mock.uri());

    // Build a router with one webhook sink directly (bypass manifest
    // parse since DispatchState::new takes the router as arg).
    let http = std::sync::Arc::new(reqwest::Client::new());
    let sink = std::sync::Arc::new(pitboss_cli::notify::sinks::WebhookSink::new(
        0,
        webhook_url.clone(),
        http,
    ));
    let router = std::sync::Arc::new(pitboss_cli::notify::NotificationRouter::new(vec![(
        sink as std::sync::Arc<dyn pitboss_cli::notify::NotificationSink>,
        pitboss_cli::notify::SinkFilter {
            events: Some(vec!["run_finished".into()]),
            severity_min: pitboss_cli::notify::Severity::Info,
        },
    )]));

    // Dispatch a RunFinished envelope directly (the full hierarchical
    // lifecycle e2e is covered by existing tests + Tasks 17/18).
    let _ = router
        .dispatch(pitboss_cli::notify::NotificationEnvelope::new(
            "run-e2e",
            pitboss_cli::notify::Severity::Info,
            pitboss_cli::notify::PitbossEvent::RunFinished {
                run_id: "run-e2e".into(),
                tasks_total: 1,
                tasks_failed: 0,
                duration_ms: 10,
                spent_usd: 0.01,
            },
            chrono::Utc::now(),
        ))
        .await;

    // Fire-and-forget — wait briefly for the tokio::spawn'd emit.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    mock.verify().await;
}

/// End-to-end plan-approval flow with a real lead subprocess.
/// `require_plan_approval = true` means spawn_worker fails until an
/// operator approves a plan via the TUI. We drive it as a real lead
/// (fake-claude) + a real control-socket client: lead calls
/// `propose_plan`, background FCC auto-approves, lead then calls
/// `spawn_worker` successfully.
#[tokio::test]
async fn e2e_lead_propose_plan_gate_unblocks_spawn() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();

    // Build a state with `require_plan_approval = true`.
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
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: true,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let worker_script = FakeScript::new()
        .stdout_line(r#"{"type":"system","subtype":"init","session_id":"worker-sess"}"#)
        .stdout_line(
            r#"{"type":"result","session_id":"worker-sess","usage":{"input_tokens":1,"output_tokens":1}}"#,
        )
        .exit_code(0);
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(worker_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
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
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    let mcp_sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _mcp_server = McpServer::start(mcp_sock.clone(), state.clone())
        .await
        .unwrap();
    let ctrl_sock = pitboss_cli::control::control_socket_path(run_id, &state.manifest.run_dir);
    let _ctrl_server = pitboss_cli::control::server::start_control_server(
        ctrl_sock.clone(),
        "0.4.5".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();

    // Background FCC: poll for queued plan approval, connect, approve.
    let ctrl_sock_bg = ctrl_sock.clone();
    let state_for_fcc = state.clone();
    let fcc_task = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if !state_for_fcc.approval_queue.lock().await.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("FCC timed out waiting for queued plan approval");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut client =
            fake_control_client::FakeControlClient::connect(&ctrl_sock_bg, "0.4.5-fcc")
                .await
                .unwrap();
        match client.recv_timeout(Duration::from_secs(5)).await.unwrap() {
            Some(pitboss_cli::control::protocol::ControlEvent::ApprovalRequest {
                request_id,
                kind,
                ..
            }) => {
                assert_eq!(
                    kind,
                    pitboss_cli::control::protocol::ApprovalKind::Plan,
                    "expected kind=Plan for propose_plan request"
                );
                client
                    .send(&pitboss_cli::control::protocol::ControlOp::Approve {
                        request_id,
                        approved: true,
                        comment: None,
                        edited_summary: None,
                        reason: None,
                    })
                    .await
                    .unwrap();
            }
            other => panic!("expected ApprovalRequest, got {other:?}"),
        }
    });

    // Lead script: propose_plan, then spawn_worker. The spawn_worker
    // line would fail with "plan approval required" if it came before
    // the plan approval; success here proves the full flow works.
    let script = dir.path().join("script.jsonl");
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"propose_plan","args":{"plan":{"summary":"phase-1","rationale":"prep work","resources":["worktrees"],"risks":[],"rollback":"drop worktrees"},"timeout_secs":10},"bind":"plan"}}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"go"},"bind":"w1"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &mcp_sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    fcc_task.await.expect("FCC task panicked");

    assert_eq!(
        outcome.exit_code,
        Some(0),
        "lead exit non-zero: outcome={outcome:?}"
    );

    // plan_approved latched true, one worker spawned successfully.
    assert!(state
        .plan_approved
        .load(std::sync::atomic::Ordering::Acquire));
    let workers = state.workers.read().await;
    assert_eq!(
        workers.len(),
        1,
        "expected one worker post-approval, got {}",
        workers.len()
    );

    state.cancel.terminate();
}

/// Full-stack bridge test: fake-claude lead spawns `pitboss mcp-bridge`
/// and speaks stdio JSON-RPC to it. The bridge injects `_meta` on every
/// `tools/call` and forwards to the unix socket. We call `kv_set` with
/// a value and then verify the stored entry's `written_by` field equals
/// the actor_id the bridge was launched with — proving the whole
/// lead → bridge (subprocess) → unix-socket → dispatcher → tool-handler
/// path works end-to-end, including the `_meta` injection that the
/// existing direct-socket e2e tests skip.
#[tokio::test]
async fn e2e_lead_through_mcp_bridge_injects_meta() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let (run_id, state) = mk_state(dir.path(), ApprovalPolicy::Block);

    let sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _server = McpServer::start(sock.clone(), state.clone()).await.unwrap();

    // Lead script: init, kv_set on /shared/bridge_probe, result.
    // kv_set carries `_meta` as a required field — the bridge's job is
    // to populate it, so a direct-socket fake-claude would fail this
    // call. Bridge mode succeeds; the stored entry's `written_by` is
    // the actor_id the bridge was configured with.
    let script = dir.path().join("script.jsonl");
    // kv_set's `value` is a Vec<u8>; serde_json encodes byte-arrays as a
    // JSON array of numbers by default. "ok" => [111, 107].
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"kv_set","args":{"path":"/shared/bridge_probe","value":[111,107]},"bind":"res"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead_via_bridge(
        dir.path(),
        &script,
        &sock,
        "lead", // actor_id the bridge will inject
        "lead", // actor_role
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(
        outcome.exit_code,
        Some(0),
        "fake-claude via bridge exited non-zero: outcome={outcome:?}"
    );
    assert!(matches!(
        outcome.final_state,
        pitboss_core::session::SessionState::Completed
    ));

    // Assertion: the stored entry MUST reflect the bridge's injected
    // actor_id. If _meta failed to arrive, kv_set would either reject
    // the call (missing required _meta) or write with a different
    // actor_id — both caught here.
    let entry = state
        .shared_store
        .get("/shared/bridge_probe")
        .await
        .expect("kv_set should have persisted the entry");
    assert_eq!(
        entry.written_by, "lead",
        "bridge-injected actor_id did not reach the tool handler; \
         written_by = {:?}",
        entry.written_by
    );

    state.cancel.terminate();
}

/// Test-only ProcessSpawner that rewrites each spawn to run fake-claude
/// with a specific env overlay. Needed because the MCP `spawn_worker`
/// path builds `SpawnCmd { env: HashMap::new(), ... }` (no per-worker
/// env plumbing), but fake-claude needs `PITBOSS_FAKE_SCRIPT` +
/// `PITBOSS_FAKE_HOLD` to act as a long-lived worker. This spawner
/// rewrites `program` to fake-claude and drops the claude-shaped `args`
/// (fake-claude ignores them).
struct FakeClaudeWorkerSpawner {
    inner: TokioSpawner,
    fake_claude: PathBuf,
    env_overlay: HashMap<String, String>,
}

#[async_trait::async_trait]
impl ProcessSpawner for FakeClaudeWorkerSpawner {
    async fn spawn(
        &self,
        mut cmd: SpawnCmd,
    ) -> Result<Box<dyn ChildProcess>, pitboss_core::error::SpawnError> {
        cmd.program = self.fake_claude.clone();
        cmd.args = vec![];
        for (k, v) in &self.env_overlay {
            cmd.env.insert(k.clone(), v.clone());
        }
        self.inner.spawn(cmd).await
    }
}

/// Real-subprocess freeze-pause e2e. Workers are spawned as actual
/// fake-claude processes (via FakeClaudeWorkerSpawner) so `worker_pids`
/// gets populated and `pause_worker(Freeze)` can send SIGSTOP.
#[tokio::test]
async fn e2e_freeze_pause_and_continue_real_subprocess_worker() {
    support::ensure_built();

    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();

    // Worker script: emit init + result, then hold. init drives
    // session_id capture (required for pause_worker); HOLD=1 keeps
    // the process alive so freeze has something to SIGSTOP.
    let worker_script_path = dir.path().join("worker-script.jsonl");
    tokio::fs::write(
        &worker_script_path,
        r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"worker-sess\"}"}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"worker-sess\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}"}
"#,
    )
    .await
    .unwrap();

    let mut env_overlay = HashMap::new();
    env_overlay.insert(
        "PITBOSS_FAKE_SCRIPT".into(),
        worker_script_path.to_string_lossy().to_string(),
    );
    env_overlay.insert("PITBOSS_FAKE_HOLD".into(), "1".into());

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
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeClaudeWorkerSpawner {
        inner: TokioSpawner::new(),
        fake_claude: fake_claude_path(),
        env_overlay,
    });
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        // claude_binary placeholder: the FakeClaudeWorkerSpawner
        // rewrites `program` to fake-claude on every spawn.
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    let mcp_sock = socket_path_for_run(run_id, &state.manifest.run_dir);
    let _server = McpServer::start(mcp_sock.clone(), state.clone())
        .await
        .unwrap();

    // Lead script: spawn, wait for init, freeze, continue, cancel.
    let script = dir.path().join("lead-script.jsonl");
    let script_body = r#"{"stdout":"{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"lead-sess\"}"}
{"mcp_call":{"name":"spawn_worker","args":{"prompt":"do a thing"},"bind":"w1"}}
{"sleep_ms":500}
{"mcp_call":{"name":"pause_worker","args":{"task_id":"$w1.task_id","mode":"freeze"},"bind":"pause_res"}}
{"sleep_ms":200}
{"mcp_call":{"name":"continue_worker","args":{"task_id":"$w1.task_id"},"bind":"cont_res"}}
{"sleep_ms":200}
{"mcp_call":{"name":"cancel_worker","args":{"task_id":"$w1.task_id"},"bind":"cancel_res"}}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"lead-sess\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}"}
"#;
    tokio::fs::write(&script, script_body).await.unwrap();

    let outcome = run_fake_claude_lead(
        dir.path(),
        &script,
        &mcp_sock,
        CancelToken::new(),
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(
        outcome.exit_code,
        Some(0),
        "lead exit non-zero: outcome={outcome:?}"
    );

    // Proof of a working freeze→continue→cancel cycle: the lead
    // subprocess exited 0 above, which means *every* mcp_call
    // succeeded (pause_worker with mode=freeze, continue_worker,
    // cancel_worker). Any failure would have bubbled out of fake-claude
    // as exit code 5. The pause_worker call in particular requires a
    // live worker pid, which requires a real TokioSpawner subprocess
    // — so this test also proves the FakeClaudeWorkerSpawner wiring
    // populates `worker_pids` correctly. No further assertions needed.
    //
    // We do wait a tick for the worker background task to settle so
    // the test's Drop teardown doesn't race a live subprocess.
    tokio::time::sleep(Duration::from_millis(200)).await;
    state.cancel.terminate();
    tokio::time::sleep(Duration::from_millis(200)).await;
}
