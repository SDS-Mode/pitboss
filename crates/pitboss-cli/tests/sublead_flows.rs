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
        approval_rules: vec![],
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
async fn sublead_workers_cannot_wait_on_sibling_peer_slots() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Worker A will write its peer slot asynchronously.
    // Worker B tries to wait on Worker A's peer slot — must be rejected.
    // (strict peer visibility: only worker-A itself or the layer lead can wait on /peer/worker-A/*)
    let mut worker_b = FakeMcpClient::connect_as(&socket, "worker-B", "worker")
        .await
        .unwrap();
    let result = worker_b
        .call_tool(
            "kv_wait",
            json!({"path": "/peer/worker-A/status", "timeout_secs": 1}),
        )
        .await;

    assert!(
        result.is_err(),
        "sibling peer-slot waits must be rejected under strict visibility; got: {:?}",
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

#[tokio::test]
async fn run_lease_blocks_cross_subtree_acquisition() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp1 = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt":"p","model":"claude-haiku-4-5","budget_usd":1.0,"max_workers":1}),
        )
        .await
        .unwrap();
    let s1 = resp1["sublead_id"]
        .as_str()
        .expect("sublead_id")
        .to_string();
    let resp2 = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt":"p","model":"claude-haiku-4-5","budget_usd":1.0,"max_workers":1}),
        )
        .await
        .unwrap();
    let s2 = resp2["sublead_id"]
        .as_str()
        .expect("sublead_id")
        .to_string();

    // S1 acquires the lease
    let mut s1_client = FakeMcpClient::connect_as(&socket, &s1, "sublead")
        .await
        .unwrap();
    let acq1 = s1_client
        .call_tool(
            "run_lease_acquire",
            json!({"key":"output.json","ttl_secs":60}),
        )
        .await;
    assert!(acq1.is_ok(), "s1 should acquire");

    // S2 tries the same key — must fail with holder info
    let mut s2_client = FakeMcpClient::connect_as(&socket, &s2, "sublead")
        .await
        .unwrap();
    let acq2 = s2_client
        .call_tool(
            "run_lease_acquire",
            json!({"key":"output.json","ttl_secs":60}),
        )
        .await;
    assert!(acq2.is_err(), "s2 should be blocked by s1's lease");
    let err = format!("{:?}", acq2.unwrap_err());
    assert!(
        err.contains(&s1),
        "error should mention current holder: {err}"
    );

    // S1 releases; S2 can now acquire
    let _rel1 = s1_client
        .call_tool("run_lease_release", json!({"key":"output.json"}))
        .await
        .unwrap();
    let acq3 = s2_client
        .call_tool(
            "run_lease_acquire",
            json!({"key":"output.json","ttl_secs":60}),
        )
        .await;
    assert!(acq3.is_ok(), "s2 should acquire after s1 releases");
}

#[tokio::test]
async fn sublead_termination_releases_run_global_leases() {
    let (_dir, state) = mk_state_with_subleads();

    // Manually register a sub-tree LayerState + a held lease
    let sublead_id = "sublead-Z";
    let sub_layer = std::sync::Arc::new(pitboss_cli::dispatch::layer::LayerState::new(
        state.root.run_id,
        state.root.manifest.clone(),
        state.root.store.clone(),
        CancelToken::new(),
        sublead_id.into(),
        state.root.spawner.clone(),
        state.root.claude_binary.clone(),
        state.root.wt_mgr.clone(),
        CleanupPolicy::Never,
        state.root.run_subdir.clone(),
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
        None,
    ));
    state
        .subleads
        .write()
        .await
        .insert(sublead_id.into(), sub_layer);
    state
        .run_leases
        .try_acquire(
            "output.json",
            sublead_id,
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap();
    assert_eq!(state.run_leases.snapshot().await.len(), 1);

    // Reconcile (terminate)
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(&state, sublead_id)
        .await
        .unwrap();

    // Lease should be released
    assert_eq!(state.run_leases.snapshot().await.len(), 0);
}

// ── Task 4.2: TOML approval policy matcher ───────────────────────────────────

/// Policy auto-approves a matching actor: set up a policy that auto-approves
/// the "root" actor (the lead in mk_state_with_subleads), simulate a
/// request_approval call, assert it short-circuits without queuing.
#[tokio::test]
async fn policy_auto_approves_matching_actor() {
    use pitboss_cli::mcp::policy::{ApprovalAction, ApprovalMatch, ApprovalRule, PolicyMatcher};
    use pitboss_cli::mcp::tools::{handle_request_approval, RequestApprovalArgs};

    let (_dir, state) = mk_state_with_subleads();

    // Inject a policy that auto-approves anything from "root" (the lead_id).
    let rule = ApprovalRule {
        r#match: ApprovalMatch {
            actor: Some("root".into()),
            ..Default::default()
        },
        action: ApprovalAction::AutoApprove,
    };
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(vec![rule]))
        .await;

    // Request approval — should be auto-approved by the policy, not queued.
    let resp = handle_request_approval(
        &state,
        RequestApprovalArgs {
            summary: "run rm -rf /tmp/foo".into(),
            timeout_secs: Some(1),
            plan: None,
            ..Default::default()
        },
    )
    .await
    .expect("handle_request_approval should succeed");

    assert!(
        resp.approved,
        "policy should auto-approve the matching actor"
    );
    assert_eq!(
        resp.comment.as_deref(),
        Some("auto-approved by policy"),
        "comment should indicate policy auto-approval"
    );

    // Verify no approval was queued (the legacy block-mode path was bypassed).
    let queue = state.root.approval_queue.lock().await;
    assert!(
        queue.is_empty(),
        "policy short-circuit should not enqueue approval; queue has {} items",
        queue.len()
    );
}

/// Sub-lead-specific auto-approval: set up a policy that auto-approves the
/// sub-lead "root→S1" actor path, spawn sub-lead S1, then have S1 call
/// `request_approval` via MCP. The `_meta` injected by FakeMcpClient causes
/// the server to build the correct actor_path `"root→S1"` rather than `"root"`,
/// so the policy matches and the approval is auto-approved without operator
/// interaction.
///
/// This test FAILS before the C1 fix (actor_path was always stamped as "root")
/// and passes after the fix.
#[tokio::test]
async fn policy_auto_approves_sublead_actor() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Root lead spawns sub-lead S1.
    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let spawn_resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "coordinate sub-tasks", "model": "claude-haiku-4-5", "budget_usd": 5.0, "max_workers": 4}),
        )
        .await
        .unwrap();
    let s1_id = spawn_resp["sublead_id"]
        .as_str()
        .expect("spawn_sublead must return sublead_id")
        .to_string();

    // Inject a policy that auto-approves requests from "root→S1".
    // The policy actor string must match the actor_path Display format.
    {
        use pitboss_cli::mcp::policy::{
            ApprovalAction, ApprovalMatch, ApprovalRule, PolicyMatcher,
        };
        let expected_path = format!("root→{}", s1_id);
        let rule = ApprovalRule {
            r#match: ApprovalMatch {
                actor: Some(expected_path),
                ..Default::default()
            },
            action: ApprovalAction::AutoApprove,
        };
        state
            .root
            .set_policy_matcher(PolicyMatcher::new(vec![rule]))
            .await;
    }

    // Sub-lead S1 calls request_approval. FakeMcpClient::connect_as with role
    // "sublead" causes _meta injection of {actor_id: s1_id, actor_role: "sublead"},
    // which build_caller_identity converts to actor_path "root→<s1_id>".
    let mut s1_client = FakeMcpClient::connect_as(&socket, &s1_id, "sublead")
        .await
        .unwrap();
    let result = s1_client
        .call_tool(
            "request_approval",
            json!({"summary": "deploy sub-task artifacts", "timeout_secs": 2}),
        )
        .await
        .expect("request_approval call should succeed");

    assert_eq!(
        result["approved"].as_bool(),
        Some(true),
        "policy should auto-approve the sub-lead's request; got: {result}"
    );
    assert_eq!(
        result["comment"].as_str(),
        Some("auto-approved by policy"),
        "comment should indicate policy auto-approval; got: {result}"
    );

    // Verify nothing was queued in the operator approval queue.
    let queue = state.root.approval_queue.lock().await;
    assert!(
        queue.is_empty(),
        "policy short-circuit must not enqueue approval; queue has {} items",
        queue.len()
    );
}

// ── Task 4.1: Rich approval record fields ────────────────────────────────────

/// Verify that `PendingApproval` carries all Phase 4 rich fields:
/// actor_path, blocks, age-tracking timestamps, TTL, fallback, and category.
#[tokio::test]
async fn approval_record_carries_actor_path_and_age() {
    use pitboss_cli::mcp::approval::ApprovalCategory;

    let (_dir, _state) = mk_state_with_subleads();
    // Construct a PendingApproval directly
    let approval = pitboss_cli::dispatch::state::PendingApproval {
        id: uuid::Uuid::now_v7(),
        requesting_actor_id: "sublead-S1".into(),
        actor_path: pitboss_cli::dispatch::actor::ActorPath::new(["root", "S1"]),
        category: ApprovalCategory::ToolUse,
        summary: "run rm -rf /tmp/foo".into(),
        plan: None,
        blocks: vec!["sublead-S1".into(), "worker-1".into()],
        created_at: chrono::Utc::now(),
        ttl_secs: 1800,
        fallback: pitboss_cli::mcp::approval::ApprovalFallback::AutoReject,
    };
    assert_eq!(approval.actor_path.to_string(), "root→S1");
    assert_eq!(approval.blocks.len(), 2);
    assert_eq!(approval.category, ApprovalCategory::ToolUse);
    assert!(approval.ttl_secs > 0);
}

// ── Task 4.3: Reject-with-reason approval response ────────────────────────

/// Verify that `ApprovalResponse` carries an optional `reason` field
/// and round-trips correctly through JSON serialization.
#[tokio::test]
async fn reject_with_reason_propagates_to_caller() {
    use pitboss_cli::dispatch::state::ApprovalResponse;

    let resp = ApprovalResponse {
        approved: false,
        comment: None,
        edited_summary: None,
        reason: Some("output should be CSV not JSON".into()),
    };
    assert!(!resp.approved);
    assert_eq!(
        resp.reason.as_deref(),
        Some("output should be CSV not JSON")
    );

    // Round-trip via JSON to verify wire compat
    let s = serde_json::to_string(&resp).unwrap();
    let back: ApprovalResponse = serde_json::from_str(&s).unwrap();
    assert_eq!(back.reason, resp.reason);
}

// ── Task 4.4: TTL + fallback for pending approvals ────────────────────

#[tokio::test]
async fn approval_ttl_triggers_auto_reject_fallback() {
    let (_dir, state) = mk_state_with_subleads();

    // Create a oneshot channel for the response (required by QueuedApproval)
    let (responder, _rx) = tokio::sync::oneshot::channel();

    let request_id = uuid::Uuid::now_v7().to_string();
    let approval = pitboss_cli::dispatch::state::QueuedApproval {
        request_id: request_id.clone(),
        task_id: "task-1".into(),
        summary: "test approval".into(),
        plan: None,
        kind: pitboss_cli::control::protocol::ApprovalKind::Action,
        responder,
        ttl_secs: Some(1), // 1 second
        fallback: Some(pitboss_cli::mcp::approval::ApprovalFallback::AutoReject),
        created_at: chrono::Utc::now(),
    };

    state.root.approval_queue.lock().await.push_back(approval);

    // Spawn the TTL watcher
    pitboss_cli::dispatch::runner::install_approval_ttl_watcher(state.clone());

    // Verify it's in the queue before TTL expires
    {
        let queue = state.root.approval_queue.lock().await;
        assert!(
            queue.iter().any(|a| a.request_id == request_id),
            "approval should be in queue initially"
        );
    }

    // Wait past TTL with multiple iterations to allow the watcher task to run.
    // The watcher ticks every 250ms, so we need to yield the event loop multiple times.
    let mut found_removed = false;
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let queue = state.root.approval_queue.lock().await;
        if queue.iter().all(|a| a.request_id != request_id) {
            found_removed = true;
            break;
        }
    }

    assert!(
        found_removed,
        "expired approval should have been removed by TTL watcher"
    );
}

// ── Task 4.5: Kill-with-reason cascade ────────────────────────────────────────

/// Root spawns sub-lead S1, a worker is injected into S1's layer, the worker
/// is killed via `cancel_worker` with a reason, and S1 should receive a
/// synthetic reprompt containing both the worker id and the reason text.
#[tokio::test]
async fn kill_worker_with_reason_reprompts_parent_sublead() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Root lead spawns sub-lead S1.
    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();
    let resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 2}),
        )
        .await
        .unwrap();
    let s1 = resp["sublead_id"]
        .as_str()
        .expect("response should have sublead_id field")
        .to_string();

    // Inject a worker into S1's layer (simulating what S1's Claude session
    // would do in production via the sub-tree MCP socket).
    {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(s1.as_str()).unwrap();
        sub.workers.write().await.insert(
            "worker-A".into(),
            pitboss_cli::dispatch::state::WorkerState::Pending,
        );
        sub.worker_cancels
            .write()
            .await
            .insert("worker-A".into(), CancelToken::new());
    }

    // Install a capture hook on S1's layer so we can assert on the reprompt.
    let s1_reprompts = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(s1.as_str()).unwrap();
        let captured = s1_reprompts.clone();
        sub.install_reprompt_capture(move |msg| {
            let captured = captured.clone();
            // Use tokio::spawn to avoid blocking the hook caller.
            tokio::spawn(async move {
                captured.lock().await.push(msg);
            });
        })
        .await;
    }

    // Operator kills worker-A with a reason via the MCP cancel_worker tool.
    root_client
        .call_tool(
            "cancel_worker",
            json!({
                "target": "worker-A",
                "reason": "output schema wrong"
            }),
        )
        .await
        .unwrap();

    // Allow the background reprompt-capture task to complete.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // S1 should receive exactly one synthetic reprompt containing both the
    // killed worker's id and the operator-supplied reason.
    let received = s1_reprompts.lock().await;
    assert_eq!(
        received.len(),
        1,
        "expected one reprompt to S1; got: {received:?}"
    );
    assert!(
        received[0].contains("worker-A"),
        "reprompt should name the killed worker; got: {}",
        received[0]
    );
    assert!(
        received[0].contains("output schema wrong"),
        "reprompt should include reason; got: {}",
        received[0]
    );
}
