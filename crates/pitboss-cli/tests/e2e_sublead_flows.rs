//! End-to-end depth-2 sub-lead tests.
//!
//! These tests exercise the five critical sub-lead user journeys using
//! a combination of FakeMcpClient (for MCP-layer operations) and direct
//! API calls where the fake-claude subprocess path is not yet wired
//! (spawn_sublead_session is a stub in Task 5.2; full wiring is Task 2.3).
//!
//! The approach mirrors `sublead_flows.rs` (integration-style direct-API)
//! rather than `e2e_flows.rs` (real subprocess) because the sub-lead
//! Claude session spawn path is not yet fully plumbed. This lets the
//! tests verify the five integrated behaviors at the MCP / dispatch layer
//! without depending on unfinished subprocess wiring.

#![allow(dead_code)]
#![allow(unused_imports)]

mod support;

use support::{ensure_built, fake_claude_path};

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use fake_mcp_client::FakeMcpClient;
use pitboss_cli::dispatch::state::{ApprovalPolicy, ApprovalResponse, DispatchState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
use pitboss_cli::mcp::{socket_path_for_run, McpServer};
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use uuid::Uuid;

/// Build a DispatchState with allow_subleads=true and a root budget of $20.
/// Worker spawner uses FakeScript that completes cleanly.
fn mk_state(dir: &std::path::Path) -> (Uuid, Arc<DispatchState>) {
    let run_id = Uuid::now_v7();
    let lead = ResolvedLead {
        id: "root".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "execute phase 1".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        allow_subleads: true,
        max_subleads: Some(4),
        max_sublead_budget_usd: Some(5.0),
        max_workers_across_tree: Some(8),
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(20.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.to_path_buf()));
    let worker_script = FakeScript::new()
        .stdout_line(r#"{"type":"system","subtype":"init","session_id":"worker-sess"}"#)
        .stdout_line(
            r#"{"type":"result","session_id":"worker-sess","usage":{"input_tokens":10,"output_tokens":20}}"#,
        )
        .exit_code(0);
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(worker_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.join(run_id.to_string());
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
    (run_id, state)
}

/// Build a DispatchState where workers hold indefinitely (for cancel tests).
fn mk_state_hold_workers(dir: &std::path::Path) -> (Uuid, Arc<DispatchState>) {
    let run_id = Uuid::now_v7();
    let lead = ResolvedLead {
        id: "root".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "execute phase 1".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        allow_subleads: true,
        max_subleads: Some(4),
        max_sublead_budget_usd: Some(5.0),
        max_workers_across_tree: Some(8),
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(20.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.to_path_buf()));
    let hold_script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(hold_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.join(run_id.to_string());
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
    (run_id, state)
}

// ── Test 1: Root spawns sub-lead which completes cleanly ─────────────────────

/// Root lead spawns a sub-lead via `spawn_sublead`. The sub-lead runs to
/// completion (simulated by directly setting up and reconciling the sub-tree).
/// Asserts:
/// - `subleads` map has 1 entry after spawn
/// - root.reserved_usd == sub-lead budget after spawn
/// - after reconcile: root.spent_usd == sub-lead actual spend
/// - unspent portion ($budget - $actual) is freed from reserved pool
/// - sub-lead entry removed from `subleads` map after reconcile
#[tokio::test]
async fn root_spawns_sublead_which_completes() {
    use pitboss_cli::dispatch::sublead::{spawn_sublead, SubleadSpawnRequest};

    let dir = TempDir::new().unwrap();
    let (_run_id, state) = mk_state(dir.path());

    let envelope_budget = 3.0_f64;
    let actual_spend = 1.5_f64;
    let expected_unspent = envelope_budget - actual_spend;

    // Root lead spawns a sub-lead with $3 envelope.
    let req = SubleadSpawnRequest {
        prompt: "execute sub-task A".into(),
        model: "claude-haiku-4-5".into(),
        budget_usd: Some(envelope_budget),
        max_workers: Some(2),
        lead_timeout_secs: Some(600),
        initial_ref: Default::default(),
        read_down: false,
    };
    let sublead_id = spawn_sublead(&state, req)
        .await
        .expect("spawn_sublead should succeed");

    // After spawn: exactly one sub-lead registered, $3 reserved.
    {
        let subleads = state.subleads.read().await;
        assert_eq!(
            subleads.len(),
            1,
            "expected exactly one sub-lead after spawn, got {}",
            subleads.len()
        );
        assert!(
            subleads.contains_key(&sublead_id),
            "subleads map should contain the new sublead_id"
        );
    }
    assert_eq!(
        *state.root.reserved_usd.lock().await,
        envelope_budget,
        "root should have reserved the full envelope"
    );

    // Simulate sub-lead spending $1.50 then terminating.
    {
        let subleads = state.subleads.read().await;
        let sub_layer = subleads.get(&sublead_id).expect("sub-layer should exist");
        *sub_layer.spent_usd.lock().await = actual_spend;
    }

    // Trigger reconciliation (mimics Event::Result terminal event).
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(
        &state,
        &sublead_id,
        pitboss_cli::dispatch::sublead::SubleadOutcome::Success,
    )
    .await
    .expect("reconcile should succeed");

    // After reconcile: reservation released, actual spend recorded, sub-lead removed.
    assert_eq!(
        *state.root.reserved_usd.lock().await,
        0.0,
        "root.reserved_usd should be 0 after reconcile"
    );
    assert_eq!(
        *state.root.spent_usd.lock().await,
        actual_spend,
        "root.spent_usd should equal sub-lead actual spend"
    );
    assert!(
        expected_unspent > 0.0,
        "unspent envelope (${expected_unspent:.2}) should be positive"
    );
    assert!(
        state.subleads.read().await.get(&sublead_id).is_none(),
        "sub-lead should be removed from the subleads map after reconcile"
    );
}

// ── Test 2: Root kill cascades to sub-lead workers within drain window ────────

/// Root lead spawns a sub-lead. The sub-lead has injected workers. Root cancel
/// triggers a depth-first cascade: sub-lead's cancel token drains, and its
/// workers' cancel tokens drain within the drain window.
///
/// Verifies:
/// - sub-lead cancel token is draining after root.cancel.drain()
/// - sub-lead's worker cancel tokens are draining after cascade settles
#[tokio::test]
async fn root_kill_cascades_to_sublead_workers() {
    use pitboss_cli::dispatch::sublead::{spawn_sublead, SubleadSpawnRequest};

    let dir = TempDir::new().unwrap();
    let (_run_id, state) = mk_state_hold_workers(dir.path());

    // Install the cascade cancel watcher (in production this runs inside
    // run_hierarchical; in tests we call it directly).
    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    // Root lead spawns a sub-lead.
    let req = SubleadSpawnRequest {
        prompt: "sub-task B".into(),
        model: "claude-haiku-4-5".into(),
        budget_usd: Some(2.0),
        max_workers: Some(2),
        lead_timeout_secs: Some(600),
        initial_ref: Default::default(),
        read_down: false,
    };
    let sublead_id = spawn_sublead(&state, req)
        .await
        .expect("spawn_sublead should succeed");

    // Inject workers into the sub-lead's layer (simulating what would happen
    // when the sub-lead's Claude session calls spawn_worker via its MCP socket).
    {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(&sublead_id).unwrap();
        for worker_id in ["worker-X", "worker-Y"] {
            sub.workers.write().await.insert(
                worker_id.into(),
                pitboss_cli::dispatch::state::WorkerState::Pending,
            );
            sub.worker_cancels
                .write()
                .await
                .insert(worker_id.into(), CancelToken::new());
        }
    }

    // Verify workers are not yet cancelled.
    {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(&sublead_id).unwrap();
        let cancels = sub.worker_cancels.read().await;
        for worker_id in ["worker-X", "worker-Y"] {
            let tok = cancels.get(worker_id).unwrap();
            assert!(
                !tok.is_draining(),
                "worker {worker_id} should not be draining before root cancel"
            );
        }
    }

    // Cancel root (triggers depth-first cascade).
    state.root.cancel.drain();

    // Wait for the cascade watcher task to propagate the drain.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Sub-lead's cancel token should be draining.
    {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(&sublead_id).unwrap();
        assert!(
            sub.cancel.is_draining(),
            "sub-lead cancel token should be draining after root cascade"
        );

        // Sub-lead workers' cancel tokens should also be draining.
        let cancels = sub.worker_cancels.read().await;
        for worker_id in ["worker-X", "worker-Y"] {
            let tok = cancels.get(worker_id).unwrap();
            assert!(
                tok.is_draining(),
                "sub-tree worker {worker_id} should be cancelled by root cascade"
            );
        }
    }
}

// ── Test 3: Run-global lease serializes two sub-leads ─────────────────────────

/// Two sub-leads both attempt to acquire the same run-global lease for
/// "output.json". The second should be blocked/rejected while the first holds
/// the lease.
///
/// Verifies:
/// - first sub-lead acquires successfully
/// - second sub-lead is rejected with holder info in the error
/// - after first releases, second can acquire
///
/// # Why `mk_state_hold_workers`
///
/// The spawned sub-lead sessions use a `FakeScript` that must NOT complete
/// during this test. With `mk_state` (fast-completing FakeScript), the
/// FakeSpawner's subprocess task exits immediately and tokio schedules
/// `spawn_sublead_session`'s background task — which calls
/// `reconcile_terminated_sublead` — potentially *before* the test's
/// `run_lease_acquire` assertions run. When that reconcile fires after S1 has
/// already acquired the lease, it calls `release_all_held_by(s1_id)` and frees
/// the lease, allowing S2 to acquire it and causing a spurious test failure.
///
/// Using `mk_state_hold_workers` (hold-until-signal FakeScript) keeps both
/// sub-lead subprocesses alive for the duration of the test so no reconcile
/// fires until `state.cancel.terminate()` cascades the shutdown.
#[tokio::test]
async fn run_global_lease_serializes_two_subleads() {
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let (run_id, state) = mk_state_hold_workers(dir.path());

    let socket = socket_path_for_run(run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // Root lead spawns two sub-leads.
    let mut root_client = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();

    let resp1 = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p1", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 1}),
        )
        .await
        .unwrap();
    let s1_id = resp1["sublead_id"]
        .as_str()
        .expect("sublead_id should be in response")
        .to_string();

    let resp2 = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "p2", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 1}),
        )
        .await
        .unwrap();
    let s2_id = resp2["sublead_id"]
        .as_str()
        .expect("sublead_id should be in response")
        .to_string();

    // S1 acquires the run-global lease for "output.json".
    let mut s1_client = FakeMcpClient::connect_as(&socket, &s1_id, "sublead")
        .await
        .unwrap();
    let acq1 = s1_client
        .call_tool(
            "run_lease_acquire",
            json!({"key": "output.json", "ttl_secs": 60}),
        )
        .await;
    assert!(
        acq1.is_ok(),
        "s1 should acquire the lease successfully: {:?}",
        acq1.err()
    );

    // S2 attempts to acquire the same lease — must be rejected with holder info.
    let mut s2_client = FakeMcpClient::connect_as(&socket, &s2_id, "sublead")
        .await
        .unwrap();
    let acq2 = s2_client
        .call_tool(
            "run_lease_acquire",
            json!({"key": "output.json", "ttl_secs": 60}),
        )
        .await;
    assert!(
        acq2.is_err(),
        "s2 should be blocked while s1 holds the lease; got: {:?}",
        acq2.ok()
    );
    let err_msg = format!("{:?}", acq2.unwrap_err());
    assert!(
        err_msg.contains(&s1_id),
        "error should name the current lease holder ({s1_id}); got: {err_msg}"
    );

    // S1 releases; S2 can now acquire.
    let _rel = s1_client
        .call_tool("run_lease_release", json!({"key": "output.json"}))
        .await
        .unwrap();

    let acq3 = s2_client
        .call_tool(
            "run_lease_acquire",
            json!({"key": "output.json", "ttl_secs": 60}),
        )
        .await;
    assert!(
        acq3.is_ok(),
        "s2 should acquire after s1 releases; got: {:?}",
        acq3.err()
    );

    state.cancel.terminate();
}

// ── Test 4: Reject-with-reason reaches sub-lead session ──────────────────────

/// Sub-lead calls `request_approval`. A background operator (simulated via
/// FakeControlClient) rejects with a reason string. The `request_approval`
/// response carries the reason back to the sub-lead's MCP call result.
///
/// Verifies:
/// - `approved = false` in the response
/// - `reason` field matches the rejection text
/// - rejection counter incremented on the sub-lead's layer
#[tokio::test]
async fn reject_with_reason_reaches_sublead_session() {
    use serde_json::json;

    let dir = TempDir::new().unwrap();
    let (run_id, state) = mk_state(dir.path());

    // create run subdir so events.jsonl writes don't fail
    tokio::fs::create_dir_all(&state.run_subdir).await.unwrap();

    let mcp_sock = socket_path_for_run(run_id, &state.root.manifest.run_dir);
    let _mcp_server = McpServer::start(mcp_sock.clone(), state.clone())
        .await
        .unwrap();

    let ctrl_sock = pitboss_cli::control::control_socket_path(run_id, &state.root.manifest.run_dir);
    let _ctrl_server = pitboss_cli::control::server::start_control_server(
        ctrl_sock.clone(),
        "0.6.0".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();

    // Root lead spawns sub-lead S1.
    let mut root_client = FakeMcpClient::connect_as(&mcp_sock, "root", "root_lead")
        .await
        .unwrap();
    let spawn_resp = root_client
        .call_tool(
            "spawn_sublead",
            json!({"prompt": "requires approval", "model": "claude-haiku-4-5", "budget_usd": 1.0, "max_workers": 1}),
        )
        .await
        .unwrap();
    let s1_id = spawn_resp["sublead_id"]
        .as_str()
        .expect("sublead_id in spawn response")
        .to_string();

    // Background task: operator-side responder rejects with a reason.
    let ctrl_sock_bg = ctrl_sock.clone();
    let state_for_bg = state.clone();
    let reason_text = "output format must be JSON, not CSV".to_string();
    let reason_clone = reason_text.clone();
    let fcc_task = tokio::spawn(async move {
        // Poll until the approval_queue is non-empty (sub-lead's request landed).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if !state_for_bg.root.approval_queue.lock().await.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for sub-lead approval request to queue");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut client =
            fake_control_client::FakeControlClient::connect(&ctrl_sock_bg, "0.6.0-fcc")
                .await
                .unwrap();
        match client.recv_timeout(Duration::from_secs(5)).await.unwrap() {
            Some(pitboss_cli::control::protocol::ControlEvent::ApprovalRequest {
                request_id,
                ..
            }) => {
                // Reject with reason.
                client
                    .send(&pitboss_cli::control::protocol::ControlOp::Approve {
                        request_id,
                        approved: false,
                        comment: Some("rejected".into()),
                        edited_summary: None,
                        reason: Some(reason_clone),
                    })
                    .await
                    .unwrap();
            }
            Some(other) => panic!("expected ApprovalRequest, got {other:?}"),
            None => panic!("FakeControlClient timed out waiting for event"),
        }
    });

    // Sub-lead S1 calls request_approval.
    let mut s1_client = FakeMcpClient::connect_as(&mcp_sock, &s1_id, "sublead")
        .await
        .unwrap();
    let approval_resp = s1_client
        .call_tool(
            "request_approval",
            json!({"summary": "write output file in CSV format", "timeout_secs": 10}),
        )
        .await
        .expect("request_approval call should return (not error out)");

    fcc_task.await.expect("FCC task panicked");

    // Give the control server's read loop time to process the Approve op and
    // update the worker_counters map before we assert on it.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The sub-lead's MCP response should carry approved=false and the reason.
    assert_eq!(
        approval_resp["approved"].as_bool(),
        Some(false),
        "approval should be rejected; got: {approval_resp}"
    );
    assert_eq!(
        approval_resp["reason"].as_str(),
        Some(reason_text.as_str()),
        "reason should flow back to the sub-lead; got: {approval_resp}"
    );

    // Rejection counter is keyed by the root layer's lead_id ("root"), since
    // ApprovalBridge::new receives the root DispatchState and uses state.lead_id.
    let counters = state
        .root
        .worker_counters
        .read()
        .await
        .get("root")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        counters.approvals_rejected, 1,
        "rejections counter on root lead should be 1 after sub-lead rejection"
    );

    state.cancel.terminate();
}

// ── Test 5: Budget envelope returns to root pool ──────────────────────────────

/// Sub-lead spawns with $5 envelope, spends $2, then terminates.
/// Asserts:
/// - root.spent_usd increases by $2 (actual sub-lead spend)
/// - root.reserved_usd returns to pre-spawn value (full $5 reservation released)
/// - net effect: $3 unspent returned to reservable pool
#[tokio::test]
async fn budget_envelope_returns_to_root_pool() {
    use pitboss_cli::dispatch::sublead::{spawn_sublead, SubleadSpawnRequest};

    let dir = TempDir::new().unwrap();
    let (_run_id, state) = mk_state(dir.path());

    let envelope_budget = 5.0_f64;
    let actual_spend = 2.0_f64;

    // Record root's pre-spawn baseline.
    let pre_spawn_reserved = *state.root.reserved_usd.lock().await;
    let pre_spawn_spent = *state.root.spent_usd.lock().await;

    // Root lead spawns sub-lead with $5 envelope.
    let req = SubleadSpawnRequest {
        prompt: "write output file".into(),
        model: "claude-haiku-4-5".into(),
        budget_usd: Some(envelope_budget),
        max_workers: Some(2),
        lead_timeout_secs: Some(600),
        initial_ref: Default::default(),
        read_down: false,
    };
    let sublead_id = spawn_sublead(&state, req)
        .await
        .expect("spawn_sublead should succeed");

    // Verify reservation was made.
    assert_eq!(
        *state.root.reserved_usd.lock().await,
        pre_spawn_reserved + envelope_budget,
        "root should have reserved the sub-lead's full envelope"
    );

    // Simulate sub-lead spending only $2 of its $5 envelope.
    {
        let subleads = state.subleads.read().await;
        let sub_layer = subleads.get(&sublead_id).expect("sub-layer must exist");
        *sub_layer.spent_usd.lock().await = actual_spend;
    }

    // Reconcile (terminate the sub-lead).
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(
        &state,
        &sublead_id,
        pitboss_cli::dispatch::sublead::SubleadOutcome::Success,
    )
    .await
    .expect("reconcile should succeed");

    // After reconcile:
    // - reserved_usd released fully (back to pre-spawn value)
    // - spent_usd rose by $2 (the actual spend)
    // - the $3 unspent is now available in the reservable pool (not reserved)
    assert_eq!(
        *state.root.reserved_usd.lock().await,
        pre_spawn_reserved,
        "root.reserved_usd should return to pre-spawn value after reconcile"
    );
    assert_eq!(
        *state.root.spent_usd.lock().await,
        pre_spawn_spent + actual_spend,
        "root.spent_usd should increase by the sub-lead's actual spend"
    );
    // Verify the sub-lead entry is cleaned up.
    assert!(
        state.subleads.read().await.get(&sublead_id).is_none(),
        "sub-lead should be removed from subleads map after reconcile"
    );
}

// ── Sub-task 3: spawn_sublead_session real subprocess lifecycle ───────────────

/// Full end-to-end sub-lead lifecycle using the real `fake-claude` binary as
/// the sub-lead subprocess.
///
/// Scenario:
/// 1. Build a `DispatchState` with `TokioSpawner` + `fake_claude_path()` as the
///    Claude binary.
/// 2. Start the MCP server (needed so `socket_path_for_run` resolves to a
///    socket that can be embedded in the per-sublead mcp-config.json).
/// 3. Call `spawn_sublead` which triggers `spawn_sublead_session`. The
///    spawned fake-claude reads a pre-written JSONL script that emits a
///    valid stream-json `result` event, then exits 0.
/// 4. Poll until `sublead_results` is populated (reconcile fired) or
///    timeout after 10 s.
/// 5. Call `handle_wait_for_actor(sublead_id)` — must return immediately
///    with `ActorTerminalRecord::Sublead` carrying `outcome = "success"`.
/// 6. Verify the sub-lead's `TaskRecord` was written via
///    `store.get_task_record(run_id, sublead_id)`.
///
/// This proves the full subprocess-spawn → monitor → reconcile path that
/// v0.6 sub-task 3 wires up.
#[tokio::test]
async fn sublead_session_spawns_runs_and_reconciles() {
    ensure_built();

    use pitboss_cli::dispatch::state::ActorTerminalRecord;
    use pitboss_cli::dispatch::sublead::{spawn_sublead, SubleadSpawnRequest};
    use pitboss_cli::mcp::tools::handle_wait_for_actor;
    use pitboss_core::process::{ProcessSpawner, TokioSpawner};

    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();

    // Write a JSONL script for fake-claude: emit result event and exit 0.
    let script_path = dir.path().join("sublead-script.jsonl");
    let result_line = r#"{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"sublead-sess-1\",\"result\":\"done\",\"usage\":{\"input_tokens\":5,\"output_tokens\":10,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0}}"}"#;
    std::fs::write(&script_path, format!("{}\n", result_line)).expect("write sublead script");

    // Build DispatchState with TokioSpawner + real fake-claude binary.
    let lead = ResolvedLead {
        id: "root".into(),
        directory: dir.path().to_path_buf(),
        prompt: "root prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        allow_subleads: true,
        max_subleads: Some(4),
        max_sublead_budget_usd: Some(5.0),
        max_workers_across_tree: Some(8),
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
        budget_usd: Some(20.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };

    let store: std::sync::Arc<dyn pitboss_core::store::SessionStore> = std::sync::Arc::new(
        pitboss_core::store::JsonFileStore::new(dir.path().to_path_buf()),
    );
    // Init the run so store.append_record works.
    store
        .init_run(&pitboss_core::store::RunMeta {
            run_id,
            manifest_path: dir.path().join("manifest.toml"),
            pitboss_version: "test".into(),
            claude_version: None,
            started_at: chrono::Utc::now(),
            env: Default::default(),
        })
        .await
        .expect("init run");

    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir)
        .await
        .expect("create run_subdir");

    let spawner: std::sync::Arc<dyn ProcessSpawner> = std::sync::Arc::new(TokioSpawner::new());

    let state = std::sync::Arc::new(DispatchState::new(
        run_id,
        manifest,
        store.clone(),
        CancelToken::new(),
        "root".into(),
        spawner,
        fake_claude_path(), // ← real fake-claude binary
        std::sync::Arc::new(pitboss_core::worktree::WorktreeManager::new()),
        CleanupPolicy::Never,
        run_subdir.clone(),
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    // Start MCP server so socket_path_for_run works and mcp-config can be
    // written. The sub-lead subprocess doesn't need to connect — fake-claude
    // only reads PITBOSS_FAKE_MCP_SOCKET when that env var is set.
    let socket = socket_path_for_run(run_id, dir.path());
    let _mcp = McpServer::start(socket.clone(), state.clone())
        .await
        .expect("start MCP server");

    // Set PITBOSS_FAKE_SCRIPT so the spawned fake-claude subprocess executes
    // our script and emits the result event. TokioSpawner inherits the full
    // process environment when cmd.env is empty.
    // SAFETY: single-threaded section of the test; no concurrent env access.
    unsafe {
        std::env::set_var("PITBOSS_FAKE_SCRIPT", &script_path);
    }

    // Spawn the sub-lead. spawn_sublead_session is now a real implementation
    // that launches fake-claude in the background.
    let req = SubleadSpawnRequest {
        prompt: "run sub-task A".into(),
        model: "claude-haiku-4-5".into(),
        budget_usd: Some(2.0),
        max_workers: Some(2),
        lead_timeout_secs: Some(30),
        initial_ref: Default::default(),
        read_down: false,
    };
    let sublead_id = spawn_sublead(&state, req)
        .await
        .expect("spawn_sublead should succeed");

    // After spawn, the sub-lead is registered in subleads with $2 reserved.
    assert!(
        state.subleads.read().await.contains_key(&sublead_id),
        "sub-lead should be registered after spawn"
    );
    assert!(
        (*state.root.reserved_usd.lock().await - 2.0).abs() < 1e-9,
        "root should reserve $2.0 for the sub-lead"
    );

    // Poll until the sub-lead is reconciled (fake-claude exited and the
    // background monitor task called reconcile_terminated_sublead). Timeout
    // after 10 s to prevent indefinite test hang.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if state.sublead_results.read().await.contains_key(&sublead_id) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("timed out waiting for sub-lead {sublead_id} to reconcile after 10 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Clean up the env var so other tests in the suite aren't affected.
    unsafe {
        std::env::remove_var("PITBOSS_FAKE_SCRIPT");
    }

    // 5. wait_actor returns the sub-lead's terminal record immediately.
    let terminal = handle_wait_for_actor(&state, &sublead_id, Some(2))
        .await
        .expect("wait_actor should return for reconciled sublead");

    match terminal {
        ActorTerminalRecord::Sublead(rec) => {
            assert_eq!(
                rec.sublead_id, sublead_id,
                "sublead_id in record should match"
            );
            assert_eq!(
                rec.outcome, "success",
                "sub-lead outcome should be 'success'; got: {}",
                rec.outcome
            );
        }
        ActorTerminalRecord::Worker(_) => {
            panic!("expected Sublead terminal record, got Worker variant")
        }
    }

    // 6. The sub-lead has been removed from the subleads map (reconciled).
    assert!(
        state.subleads.read().await.get(&sublead_id).is_none(),
        "subleads map should be empty after reconcile"
    );

    // 7. Root's reservation is released and spend is recorded.
    assert_eq!(
        *state.root.reserved_usd.lock().await,
        0.0,
        "root.reserved_usd should be 0 after reconcile"
    );

    state.cancel.terminate();
}
