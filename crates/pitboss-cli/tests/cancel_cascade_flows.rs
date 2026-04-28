//! Late-registration cancel-cascade contract tests for issue #100.
//!
//! Pins the eager-propagation behavior currently inlined at
//! `mcp/tools.rs:506-521` (worker registration) and
//! `dispatch/sublead.rs:374-383` (sub-lead spawn) so the per-sub-tree
//! runner refactor (PR 100.2) cannot regress it.
//!
//! ## Reachable matrix
//!
//! | Phase            | Sub-lead spawn               | Worker spawn (sub-tree)     |
//! |------------------|------------------------------|-----------------------------|
//! | Layer drained    | inherits drain               | inherits drain              |
//! | Layer terminated | inherits terminate           | (defensive — see below)     |
//!
//! Worker spawns at the **root** layer post-root-cancel are blocked by
//! Guard 1 in `handle_spawn_worker` (`tools.rs:362-365`) — those paths
//! never reach the eager-cascade block, so they have no contract to pin.
//! The sub-tree worker drain case below exercises the cancel-actor flow
//! where a sub-lead's own layer is cancelled while the root remains
//! healthy.
//!
//! The sub-tree worker **terminate** case is omitted: terminating a
//! sub-tree's `cancel` triggers the sub-lead's session task to kill the
//! claude subprocess, which fires reconciliation, which removes the
//! sub-lead from `state.subleads`. Subsequent worker spawns then fail
//! at `resolve_target_layer` with "unknown sublead_id" before reaching
//! the eager-cascade block. The `is_terminated()` branch at
//! `tools.rs:514-516` is genuine defensive coverage for an intra-handler
//! race (terminate fires after target-layer resolution succeeds but
//! before worker_cancels is read), which is not observable through the
//! external MCP API. Pinning that race would require an in-process hook
//! that 100.2's refactor will provide naturally; revisit then.
//!
//! Each test mirrors production's hierarchical setup (calls
//! `install_cascade_cancel_watcher` so the root → sub-tree fire-once
//! watcher runs first), then drives the sub-lead/worker spawn through
//! the live MCP server. The fire-once watcher snapshots an empty subleads
//! map and exits; the eager check is the only thing that can propagate
//! the signal to the late-registered actor.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use fake_mcp_client::FakeMcpClient;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
use pitboss_cli::mcp::{socket_path_for_run, McpServer};
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use serde_json::json;
use uuid::Uuid;

/// Build a `DispatchState` with `allow_subleads = true` and budgets
/// generous enough to cover one sub-lead and one worker. Same shape as
/// `sublead_flows::mk_state_with_subleads`, kept local so the test file
/// is self-contained.
fn mk_state() -> (TempDir, Arc<DispatchState>) {
    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "root".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "root prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        permission_routing: Default::default(),
        allow_subleads: true,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: 8,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(20),
        budget_usd: Some(20.0),
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
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "root".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::Block,
        None,
        Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    (dir, state)
}

/// Sleep long enough for the fire-once cascade-watcher task to wake,
/// snapshot, and exit before the test proceeds. The watcher's wakeup is
/// a single channel notification; 50ms is generous on every machine the
/// rest of the suite tolerates.
async fn yield_for_cascade_watcher() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// Sub-lead spawned **after** `state.root.cancel.drain()` must inherit
/// drain via the eager cascade at `sublead.rs:374-383`. The fire-once
/// `install_cascade_cancel_watcher` task has already snapshotted an
/// empty subleads map and exited by the time this sub-lead is spawned.
#[tokio::test]
async fn sublead_spawned_after_root_drain_inherits_drain() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    state.root.cancel.drain();
    yield_for_cascade_watcher().await;

    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 1}),
        )
        .await
        .unwrap();
    let sublead_id = resp["sublead_id"].as_str().unwrap().to_string();

    let subleads = state.subleads.read().await;
    let sub_layer = subleads
        .get(&sublead_id)
        .expect("sub-tree layer must exist after spawn_sublead");
    assert!(
        sub_layer.cancel.is_draining(),
        "sub-lead spawned post-drain must inherit drain via the eager cascade"
    );
}

/// Sub-lead spawned **after** `state.root.cancel.terminate()` must
/// inherit terminate via the eager cascade at `sublead.rs:374-383`.
/// Mirrors the drain test; terminate is the dominant signal so the
/// eager check applies it instead of (and not in addition to) drain.
#[tokio::test]
async fn sublead_spawned_after_root_terminate_inherits_terminate() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    state.root.cancel.terminate();
    yield_for_cascade_watcher().await;

    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 1}),
        )
        .await
        .unwrap();
    let sublead_id = resp["sublead_id"].as_str().unwrap().to_string();

    let subleads = state.subleads.read().await;
    let sub_layer = subleads
        .get(&sublead_id)
        .expect("sub-tree layer must exist after spawn_sublead");
    assert!(
        sub_layer.cancel.is_terminated(),
        "sub-lead spawned post-terminate must inherit terminate via the eager cascade"
    );
}

/// Worker registered in a **drained sub-tree** (root still healthy) must
/// inherit drain via the eager cascade at `tools.rs:506-521`. The
/// per-sublead fire-once watcher already woke when the sub-tree's
/// `cancel` was tripped and snapshotted an empty `worker_cancels` —
/// only the eager check can reach this late-registered worker.
#[tokio::test]
async fn worker_spawned_in_subtree_after_subtree_drain_inherits_drain() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 5.0, "max_workers": 4}),
        )
        .await
        .unwrap();
    let sublead_id = resp["sublead_id"].as_str().unwrap().to_string();

    {
        let subleads = state.subleads.read().await;
        let sub_layer = subleads
            .get(&sublead_id)
            .expect("sub-tree layer must exist after spawn_sublead");
        sub_layer.cancel.drain();
    }
    yield_for_cascade_watcher().await;

    let mut sub_client = FakeMcpClient::connect_as(&socket, &sublead_id, "sublead")
        .await
        .unwrap();
    let spawn_resp = sub_client
        .call_tool("spawn_worker", json!({"prompt": "late worker"}))
        .await
        .expect("sub-lead spawn_worker should still succeed (root not draining)");
    let task_id = spawn_resp["task_id"].as_str().unwrap().to_string();

    let subleads = state.subleads.read().await;
    let sub_layer = subleads.get(&sublead_id).unwrap();
    let worker_cancels = sub_layer.worker_cancels.read().await;
    let tok = worker_cancels
        .get(&task_id)
        .expect("worker cancel token must be registered on the sub-tree layer");
    assert!(
        tok.is_draining(),
        "worker spawned in drained sub-tree must inherit drain via the eager cascade"
    );
}

/// Production-behavior pin: terminating a sub-tree reaps the sub-lead.
/// Subsequent `spawn_worker` calls into that sub-tree fail at
/// `resolve_target_layer` with "unknown sublead_id" — the eager-terminate
/// branch at `tools.rs:514-516` is unreachable through the external MCP
/// surface. If 100.2 changes the reaping semantics (e.g., adds a grace
/// period before sub-leads disappear from `state.subleads`), this test
/// will start producing a different error and we must revisit whether
/// the eager-terminate path becomes observable.
#[tokio::test]
async fn worker_spawn_after_subtree_terminate_fails_with_unknown_sublead() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 5.0, "max_workers": 4}),
        )
        .await
        .unwrap();
    let sublead_id = resp["sublead_id"].as_str().unwrap().to_string();

    {
        let subleads = state.subleads.read().await;
        let sub_layer = subleads
            .get(&sublead_id)
            .expect("sub-tree layer must exist after spawn_sublead");
        sub_layer.cancel.terminate();
    }
    yield_for_cascade_watcher().await;

    let mut sub_client = FakeMcpClient::connect_as(&socket, &sublead_id, "sublead")
        .await
        .unwrap();
    let result = sub_client
        .call_tool("spawn_worker", json!({"prompt": "late worker"}))
        .await;

    let err = result.expect_err("spawn_worker into a terminated sub-tree must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("unknown sublead_id"),
        "expected reaping to make the sub-lead unknown; got: {msg}"
    );
}
