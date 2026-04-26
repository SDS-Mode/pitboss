//! Integration tests for v0.3 hierarchical orchestration. These drive the
//! pitboss MCP server as if we were a lead claude subprocess, using fake-mcp-client.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use fake_mcp_client::FakeMcpClient;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
use pitboss_cli::mcp::{socket_path_for_run, McpServer};
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use uuid::Uuid;

fn mk_state() -> (TempDir, Arc<DispatchState>) {
    let dir = TempDir::new().unwrap();
    // Lead with use_worktree=false so background worker spawns don't require
    // an actual git repo at state.root.manifest.lead.directory.
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
        max_parallel_tasks: 4,
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
    // FakeSpawner.hold_until_signal() keeps backgrounded workers Running so
    // `active_worker_count()` stays deterministic across the test.
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
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
    (dir, state)
}

#[tokio::test]
async fn mcp_spawn_and_list_round_trip() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let spawn_result = client
        .call_tool(
            "spawn_worker",
            json!({
                "prompt": "investigate issue #1"
            }),
        )
        .await
        .unwrap();
    let task_id = spawn_result["task_id"].as_str().unwrap().to_string();
    assert!(task_id.starts_with("worker-"));

    let list_result = client.call_tool("list_workers", json!({})).await.unwrap();
    // list_workers returns `{ workers: [...] }` — MCP spec requires
    // structuredContent to be a record, not a bare array.
    let list = list_result["workers"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["task_id"].as_str().unwrap(), task_id);

    client.close().await.unwrap();
}

#[tokio::test]
async fn mcp_spawn_over_max_workers_returns_error() {
    let (_dir, state) = mk_state(); // max_workers = 4
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();

    for i in 0..4 {
        client
            .call_tool(
                "spawn_worker",
                json!({
                    "prompt": format!("w{}", i)
                }),
            )
            .await
            .unwrap();
    }
    let err = client
        .call_tool("spawn_worker", json!({"prompt": "over"}))
        .await;
    match err {
        Err(e) => {
            let msg = format!("{e:?}");
            assert!(
                msg.contains("worker cap reached"),
                "expected 'worker cap reached' in error, got: {msg}"
            );
        }
        Ok(v) => {
            let s = v.to_string();
            assert!(
                s.contains("worker cap reached"),
                "expected Err or result containing 'worker cap reached', got Ok: {s}"
            );
        }
    }

    client.close().await.unwrap();
}

#[tokio::test]
async fn mcp_spawn_over_budget_returns_error() {
    let (_dir, state) = mk_state(); // budget_usd = 5.0
    *state.root.spent_usd.lock().await = 5.0;
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();

    let err = client
        .call_tool("spawn_worker", json!({"prompt": "p"}))
        .await;
    match err {
        Err(e) => {
            let msg = format!("{e:?}");
            assert!(
                msg.contains("budget exceeded"),
                "expected 'budget exceeded' in error, got: {msg}"
            );
        }
        Ok(v) => {
            let s = v.to_string();
            assert!(
                s.contains("budget exceeded"),
                "expected Err or result containing 'budget exceeded', got Ok: {s}"
            );
        }
    }

    client.close().await.unwrap();
}

#[tokio::test]
async fn mcp_spawn_while_draining_returns_error() {
    let (_dir, state) = mk_state();
    state.root.cancel.drain();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();

    let err = client
        .call_tool("spawn_worker", json!({"prompt": "p"}))
        .await;
    match err {
        Err(e) => {
            let msg = format!("{e:?}");
            assert!(
                msg.contains("draining"),
                "expected 'draining' in error, got: {msg}"
            );
        }
        Ok(v) => {
            let s = v.to_string();
            assert!(
                s.contains("draining"),
                "expected Err or result containing 'draining', got Ok: {s}"
            );
        }
    }

    client.close().await.unwrap();
}

// Task 26 of the v0.3 plan left a placeholder here; the full
// hierarchical end-to-end coverage landed in v0.4.1's e2e_flows.rs and
// was extended in v0.4.5 (`e2e_lead_spawns_worker_via_real_subprocess`,
// `e2e_lead_propose_plan_gate_unblocks_spawn`,
// `e2e_lead_through_mcp_bridge_injects_meta`, and others). The empty
// placeholder test has been removed.

#[tokio::test]
async fn wait_actor_alias_resolves_worker_id() {
    use pitboss_core::store::{TaskRecord, TaskStatus};
    use std::time::Duration;

    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let spawn_result = client
        .call_tool(
            "spawn_worker",
            json!({"prompt": "p", "model": "claude-haiku-4-5"}),
        )
        .await
        .unwrap();
    let worker_id = spawn_result["task_id"].as_str().unwrap().to_string();
    assert!(worker_id.starts_with("worker-"));

    // Spawn a task to mark the worker Done after a brief delay, simulating
    // actual completion. This allows wait_actor to unblock.
    let state_clone = state.clone();
    let worker_id_clone = worker_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let rec = TaskRecord {
            task_id: worker_id_clone.clone(),
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
        };
        let mut w = state_clone.root.workers.write().await;
        w.insert(worker_id_clone.clone(), WorkerState::Done(rec));
        let _ = state_clone.root.done_tx.send(worker_id_clone);
    });

    // wait_actor should accept a worker id (back-compat path) and
    // resolve identically to wait_for_worker.
    let result = client
        .call_tool(
            "wait_actor",
            json!({"actor_id": worker_id, "timeout_secs": 5}),
        )
        .await;
    assert!(result.is_ok(), "wait_actor should accept worker actor_ids");

    client.close().await.unwrap();
}

#[test]
fn mcp_bridge_accepts_sublead_role_in_meta() {
    use pitboss_cli::mcp::bridge::inject_meta;
    use serde_json::json;

    let mut request = json!({
        "method": "tools/call",
        "params": { "name": "spawn_worker", "arguments": {} }
    });
    inject_meta(&mut request, "sublead-1", "sublead", None);
    let meta = request
        .pointer("/params/arguments/_meta")
        .expect("_meta should be injected at params.arguments._meta");
    assert_eq!(meta["actor_id"], "sublead-1");
    assert_eq!(meta["actor_role"], "sublead");
}
