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

// ── Task 3.1: Per-layer KvStore + strict peer visibility ─────────────────────

/// Sub-lead's KV writes go to its own layer's `SharedStore`, NOT the root
/// layer's. After a sub-lead writes `/shared/key`, the root lead reading the
/// same path from the root layer's store should see `null` (key doesn't exist
/// in the root layer's store). Verifies per-layer KvStore isolation.
#[tokio::test]
async fn sublead_kv_writes_isolated_from_root() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Root lead spawns a sub-lead.
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
    let sublead_id = resp["sublead_id"]
        .as_str()
        .expect("response should have sublead_id field")
        .to_string();

    // Sub-lead writes /shared/key = "from_sub" into its own sub-tree layer.
    let mut sub_client = FakeMcpClient::connect_as(&socket, &sublead_id, "sublead")
        .await
        .unwrap();
    sub_client
        .call_tool(
            "kv_set",
            json!({"path": "/shared/key", "value": [102, 114, 111, 109, 95, 115, 117, 98]}), // "from_sub" as bytes
        )
        .await
        .unwrap();

    // Root reads /shared/key from the ROOT layer's store — should see null
    // because the sub-lead's write went to the sub-tree's store, not root's.
    let root_read = root_client
        .call_tool("kv_get", json!({"path": "/shared/key"}))
        .await
        .unwrap();
    assert!(
        root_read["entry"].is_null(),
        "root should not see sub-tree writes (different KvStore per layer); got: {root_read}"
    );

    // Confirm the sub-lead CAN read back its own write (same layer).
    let sub_read = sub_client
        .call_tool("kv_get", json!({"path": "/shared/key"}))
        .await
        .unwrap();
    assert!(
        !sub_read["entry"].is_null(),
        "sub-lead should be able to read its own write; got: {sub_read}"
    );
}

/// Strict peer-visibility: within any layer, `/peer/<X>/*` is readable only
/// by X itself or the layer's lead. Workers (and sub-leads acting as peers in
/// a layer) CANNOT read sibling peer slots.
///
/// This test uses two workers in the ROOT layer. Worker A writes its peer slot;
/// Worker B attempts to read it — must be rejected.
///
/// Note: the `worker_a_publishes_worker_b_consumes` test in shared_store_flows
/// calls `handle_kv_wait` directly, bypassing the MCP server authz layer, and
/// is testing a lower-level plumbing concern. The strict peer visibility rule
/// applies at the MCP transport layer (mcp/server.rs), not the store layer.
#[tokio::test]
async fn sublead_workers_cannot_read_sibling_peer_slots() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Worker A writes its peer slot.
    // (worker_meta uses ActorRole::Worker → "worker" role → root layer)
    let mut worker_a = FakeMcpClient::connect_as(&socket, "worker-A", "worker")
        .await
        .unwrap();
    worker_a
        .call_tool(
            "kv_set",
            json!({"path": "/peer/self/status", "value": [104, 97, 108, 102, 119, 97, 121]}), // "halfway" as bytes
        )
        .await
        .unwrap();

    // Worker B tries to read Worker A's peer slot — must be rejected.
    // (strict peer visibility: only worker-A itself or the layer lead can read /peer/worker-A/*)
    let mut worker_b = FakeMcpClient::connect_as(&socket, "worker-B", "worker")
        .await
        .unwrap();
    let result = worker_b
        .call_tool("kv_get", json!({"path": "/peer/worker-A/status"}))
        .await;

    assert!(
        result.is_err(),
        "sibling peer-slot reads must be rejected under strict visibility; got: {:?}",
        result
    );
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(
        err_msg.contains("strict peer visibility") || err_msg.contains("forbidden"),
        "error should mention peer visibility; got: {err_msg}"
    );
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

#[tokio::test]
async fn unspent_sublead_envelope_returns_to_root_pool() {
    use pitboss_cli::dispatch::sublead::SubleadSpawnRequest;

    let (_dir, state) = mk_state_with_subleads();
    // Spawn a sub-lead with $5.0 budget via spawn_sublead (the real path).
    let req = SubleadSpawnRequest {
        prompt: "test prompt".into(),
        model: "claude-haiku-4-5".into(),
        budget_usd: Some(5.0),
        max_workers: Some(2),
        lead_timeout_secs: Some(1800),
        initial_ref: Default::default(),
        read_down: false,
    };
    let sublead_id = pitboss_cli::dispatch::sublead::spawn_sublead(&state, req)
        .await
        .expect("spawn_sublead should succeed");

    // Verify the reservation was made
    assert_eq!(*state.root.reserved_usd.lock().await, 5.0);

    // Simulate sub-lead spending only $2 then terminating.
    // (In production this happens automatically as the sub-lead's
    // workers complete and accumulate spend in the sub-tree's
    // LayerState.spent_usd; the reconciliation is triggered by the
    // sub-lead's terminal Event::Result.)
    {
        let subleads = state.subleads.read().await;
        let sub_layer = subleads.get(&sublead_id).expect("sub-layer should exist");
        *sub_layer.spent_usd.lock().await = 2.0;
    }

    // Trigger reconciliation (now without the original_reservation parameter)
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(&state, &sublead_id)
        .await
        .unwrap();

    // After: root's reserved_usd dropped by $5, spent_usd rose by $2,
    // releasing $3 to reservable pool.
    assert_eq!(*state.root.reserved_usd.lock().await, 0.0);
    assert_eq!(*state.root.spent_usd.lock().await, 2.0);
    // Verify sub-layer was removed during reconciliation
    assert!(
        state.subleads.read().await.get(&sublead_id).is_none(),
        "sub-tree should be removed after reconciliation"
    );
}
