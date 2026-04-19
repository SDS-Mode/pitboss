//! Integration tests for v0.6 depth-2 sub-leads. Driven through the
//! pitboss MCP server using fake-mcp-client, mirroring the
//! hierarchical_flows.rs pattern.

use std::path::PathBuf;
use std::sync::Arc;

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
use uuid::Uuid;

/// Same shape as hierarchical_flows::mk_state but with allow_subleads
/// enabled on the lead. Used by every test in this file.
fn mk_state_with_subleads() -> (TempDir, Arc<DispatchState>) {
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
    };
    let manifest = ResolvedManifest {
        max_parallel: 8,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(20),
        budget_usd: Some(20.0),
        lead_timeout_secs: None,
        approval_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
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
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    (dir, state)
}

#[tokio::test]
async fn spawn_sublead_tool_is_exposed_to_root() {
    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name == "spawn_sublead"),
        "spawn_sublead should be in the root lead's MCP toolset"
    );
}

#[tokio::test]
async fn spawn_sublead_creates_isolated_layer() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Plain connect() defaults to root_lead identity, which passes the role check.
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let resp = client
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "execute phase 1",
                "model": "claude-haiku-4-5",
                "budget_usd": 5.0,
                "max_workers": 4,
                "lead_timeout_secs": 1800,
                "initial_ref": { "plan": "do thing" },
                "read_down": false
            }),
        )
        .await
        .expect("spawn_sublead should succeed");

    // Response should include the new sublead_id.
    let sublead_id = resp["sublead_id"]
        .as_str()
        .expect("response should have sublead_id field");
    assert!(sublead_id.starts_with("sublead-"), "got id: {sublead_id}");

    // The sub-tree LayerState should now exist on DispatchState.
    let subleads = state.subleads.read().await;
    assert!(
        subleads.contains_key(sublead_id),
        "sub-tree layer should be registered"
    );

    // The sub-tree's /ref/plan should be seeded.
    let layer = subleads.get(sublead_id).unwrap();
    let entry = layer
        .shared_store
        .get("/ref/plan")
        .await
        .expect("layer shared_store should have /ref/plan");
    let plan_value: serde_json::Value =
        serde_json::from_slice(&entry.value).expect("value should be valid JSON");
    assert_eq!(plan_value, json!("do thing"));

    // Root's reservation should reflect the sub-lead's envelope.
    assert_eq!(*state.root.reserved_usd.lock().await, 5.0);
}

#[tokio::test]
async fn root_cancel_cascades_to_sublead_workers() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Install the cascade watcher for this test run (in production this is
    // called inside run_hierarchical after the MCP server starts).
    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    let mut client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp = client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 2}),
        )
        .await
        .unwrap();
    let sublead_id = resp["sublead_id"]
        .as_str()
        .expect("response should have sublead_id field")
        .to_string();

    // Sub-lead spawns a worker into its own layer. Simulate by
    // reaching into the sub-tree's layer directly (in production
    // this happens via the sub-lead's MCP session).
    {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(sublead_id.as_str()).unwrap();
        sub.workers.write().await.insert(
            "worker-A".into(),
            pitboss_cli::dispatch::state::WorkerState::Pending,
        );
        sub.worker_cancels
            .write()
            .await
            .insert("worker-A".into(), CancelToken::new());
    }

    // Cancel root (drain triggers cascade)
    state.root.cancel.drain();

    // Wait for cascade to settle
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Sub-tree's worker cancel token should be tripped
    let subleads = state.subleads.read().await;
    let sub = subleads.get(sublead_id.as_str()).unwrap();
    let toks = sub.worker_cancels.read().await;
    let tok = toks.get("worker-A").unwrap();
    assert!(
        tok.is_draining(),
        "sub-tree worker should be cancelled by root cascade"
    );
}

#[tokio::test]
async fn sublead_cannot_call_spawn_sublead() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Connect as a sub-lead actor (the fake client simulates what mcp-bridge
    // would do in production, injecting _meta into the call_tool request).
    let mut client = FakeMcpClient::connect_as(&socket, "sublead-x", "sublead")
        .await
        .unwrap();
    let result = client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 1}),
        )
        .await;

    // The call should fail because the sublead role is not allowed
    assert!(
        result.is_err(),
        "sub-lead should not be able to call spawn_sublead"
    );
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("depth-2") || err_msg.contains("only available to the root lead"),
        "error message should mention depth-2 invariant, got: {err_msg}"
    );
}
