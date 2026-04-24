//! Dispatcher-side control-socket integration tests. Uses
//! fake-control-client to drive the flow end-to-end.

use fake_control_client::FakeControlClient;
use pitboss_cli::control::control_socket_path;
use pitboss_cli::control::protocol::{ControlEvent, ControlOp};
use pitboss_cli::control::server::start_control_server;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};
use pitboss_cli::manifest::resolve::ResolvedManifest;
use pitboss_cli::manifest::schema::WorktreeCleanup;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn control_socket_path_uses_xdg_or_run_dir() {
    // Ensure the helper at least produces a valid path (regression guard for
    // Phase 1 wiring).
    std::env::remove_var("XDG_RUNTIME_DIR");
    let dir = TempDir::new().unwrap();
    let p = control_socket_path(Uuid::now_v7(), dir.path());
    assert!(p.starts_with(dir.path()));
    assert_eq!(p.file_name().unwrap(), "control.sock");
}

#[tokio::test]
async fn pause_op_writes_events_jsonl() {
    let dir = TempDir::new().unwrap();
    let run_id = uuid::Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "".into(),
        spawner,
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir.clone(),
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    let worker_token = CancelToken::new();
    state
        .root
        .worker_cancels
        .write()
        .await
        .insert("w-1".into(), worker_token);
    state.root.workers.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    let sock = dir.path().join("events-pause.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let mut client = FakeControlClient::connect(&sock, "0.4.0").await.unwrap();
    client
        .send(&ControlOp::PauseWorker {
            task_id: "w-1".into(),
            mode: pitboss_cli::control::protocol::PauseMode::default(),
        })
        .await
        .unwrap();
    let ev = client
        .recv_timeout(std::time::Duration::from_secs(1))
        .await
        .unwrap()
        .expect("reply");
    assert!(matches!(ev, ControlEvent::OpAcked { .. }));

    // Assert events.jsonl was written.
    let events_path = run_subdir.join("tasks").join("w-1").join("events.jsonl");
    let contents = tokio::fs::read_to_string(&events_path).await.unwrap();
    assert!(contents.contains("\"kind\":\"pause\""));
}

use pitboss_cli::mcp::approval::ApprovalBridge;

#[tokio::test]
async fn control_event_carries_actor_path() {
    use pitboss_cli::control::protocol::{ControlEvent, EventEnvelope};
    use pitboss_cli::dispatch::actor::ActorPath;

    // Build an envelope wrapping an existing event with a deep actor path.
    let envelope = EventEnvelope {
        actor_path: ActorPath::new(["root", "S1", "W3"]),
        event: ControlEvent::Superseded,
    };

    // Serialized form must contain the actor_path field and its segments.
    let s = serde_json::to_string(&envelope).unwrap();
    assert!(
        s.contains("\"actor_path\""),
        "actor_path must be present: {s}"
    );
    assert!(s.contains("root"), "root segment must appear: {s}");
    assert!(s.contains("S1"), "S1 segment must appear: {s}");
    assert!(s.contains("W3"), "W3 segment must appear: {s}");

    // Round-trip must preserve the depth.
    let back: EventEnvelope = serde_json::from_str(&s).unwrap();
    assert_eq!(back.actor_path.depth(), 3);
}

#[tokio::test]
async fn sublead_spawned_event_emitted() {
    use pitboss_cli::control::protocol::ControlEvent;

    let event = ControlEvent::SubleadSpawned {
        sublead_id: "S1".into(),
        budget_usd: Some(5.0),
        max_workers: Some(4),
        read_down: false,
    };
    let s = serde_json::to_string(&event).unwrap();
    // ControlEvent uses tag = "event", so the discriminator key is "event".
    assert!(
        s.contains("\"event\""),
        "discriminator key must be 'event': {s}"
    );
    assert!(
        s.contains("sublead_spawned"),
        "variant name must appear: {s}"
    );

    // Round-trip.
    let back: ControlEvent = serde_json::from_str(&s).unwrap();
    assert!(
        matches!(back, ControlEvent::SubleadSpawned { .. }),
        "round-trip must yield SubleadSpawned"
    );
}

#[tokio::test]
async fn sublead_terminated_event_roundtrips() {
    use pitboss_cli::control::protocol::ControlEvent;

    let event = ControlEvent::SubleadTerminated {
        sublead_id: "S1".into(),
        spent_usd: 2.50,
        unspent_usd: 2.50,
        outcome: "success".into(),
    };
    let s = serde_json::to_string(&event).unwrap();
    assert!(
        s.contains("sublead_terminated"),
        "variant name must appear: {s}"
    );
    assert!(s.contains("\"success\""), "outcome must appear: {s}");

    let back: ControlEvent = serde_json::from_str(&s).unwrap();
    match back {
        ControlEvent::SubleadTerminated {
            sublead_id,
            spent_usd,
            unspent_usd,
            outcome,
        } => {
            assert_eq!(sublead_id, "S1");
            assert!((spent_usd - 2.50).abs() < 1e-9);
            assert!((unspent_usd - 2.50).abs() < 1e-9);
            assert_eq!(outcome, "success");
        }
        other => panic!("expected SubleadTerminated, got {other:?}"),
    }
}

#[tokio::test]
async fn event_envelope_empty_actor_path_omitted_on_wire() {
    use pitboss_cli::control::protocol::{ControlEvent, EventEnvelope};
    use pitboss_cli::dispatch::actor::ActorPath;

    // An envelope with an empty actor_path must serialize without the
    // actor_path key (backward-compat: v0.5 clients parse unmodified events).
    let envelope = EventEnvelope {
        actor_path: ActorPath::default(),
        event: ControlEvent::Superseded,
    };
    let s = serde_json::to_string(&envelope).unwrap();
    assert!(
        !s.contains("actor_path"),
        "empty actor_path must be omitted: {s}"
    );

    // And it must still round-trip correctly.
    let back: EventEnvelope = serde_json::from_str(&s).unwrap();
    assert!(back.actor_path.is_empty());
    assert!(matches!(back.event, ControlEvent::Superseded));
}

#[tokio::test]
async fn block_policy_queue_drains_on_tui_connect() {
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let run_id = uuid::Uuid::now_v7();
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
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
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir.clone(),
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    // Kick off a blocking request on a background task (no TUI attached yet).
    let bridge = ApprovalBridge::new(state.clone());
    let req_handle = tokio::spawn(async move {
        bridge
            .request(
                "lead".into(),
                "spawn 3".into(),
                None,
                pitboss_cli::control::protocol::ApprovalKind::Action,
                Duration::from_secs(5),
                None,
                None,
            )
            .await
    });

    // Give the request a moment to queue.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(state.root.approval_queue.lock().await.len(), 1);

    // Connect a TUI. The server drains the queue on connect.
    let sock = dir.path().join("block-drain.sock");
    let _h = pitboss_cli::control::server::start_control_server(
        sock.clone(),
        "0.4.0".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();

    let mut client = fake_control_client::FakeControlClient::connect(&sock, "0.4.0")
        .await
        .unwrap();

    // Expect an ApprovalRequest event pushed from the drain.
    let ev = client
        .recv_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("drain pushes approval_request");
    let request_id = match ev {
        ControlEvent::ApprovalRequest { request_id, .. } => request_id,
        other => panic!("expected ApprovalRequest, got {other:?}"),
    };

    // Respond.
    client
        .send(&ControlOp::Approve {
            request_id,
            approved: true,
            comment: None,
            edited_summary: None,
            reason: None,
        })
        .await
        .unwrap();

    // Await the original request's resolution.
    let resp = tokio::time::timeout(Duration::from_secs(3), req_handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(resp.approved);
}

#[tokio::test]
async fn auto_approve_policy_responds_without_tui() {
    use std::time::Duration;
    let dir = TempDir::new().unwrap();
    let run_id = uuid::Uuid::now_v7();
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::AutoApprove),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
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
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::AutoApprove,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    let bridge = ApprovalBridge::new(state);
    let resp = bridge
        .request(
            "lead".into(),
            "spawn".into(),
            None,
            pitboss_cli::control::protocol::ApprovalKind::Action,
            Duration::from_millis(200),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(resp.approved);
}

#[tokio::test]
async fn auto_reject_policy_responds_without_tui() {
    use std::time::Duration;
    let dir = TempDir::new().unwrap();
    let run_id = uuid::Uuid::now_v7();
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::AutoReject),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
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
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::AutoReject,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    let bridge = ApprovalBridge::new(state);
    let resp = bridge
        .request(
            "lead".into(),
            "spawn".into(),
            None,
            pitboss_cli::control::protocol::ApprovalKind::Action,
            Duration::from_millis(200),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(!resp.approved);
    assert_eq!(resp.comment.as_deref(), Some("no operator available"));
}

#[tokio::test]
async fn propose_plan_end_to_end_unblocks_spawn_gate() {
    use pitboss_cli::control::protocol::{ApprovalKind, ApprovalPlanWire};
    use pitboss_cli::mcp::tools::{
        handle_propose_plan, handle_spawn_worker, ApprovalPlan, ProposePlanArgs, SpawnWorkerArgs,
    };
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let run_id = uuid::Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: true,
        approval_rules: vec![],
        container: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    // Baseline: spawn_worker is gated.
    let err = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "early work".into(),
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
    assert!(err.to_string().contains("plan approval required"));

    // Boot the control server so an operator can attach.
    let sock = dir.path().join("plan-approval.sock");
    let _h = pitboss_cli::control::server::start_control_server(
        sock.clone(),
        "0.4.5".into(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .unwrap();
    let mut client = fake_control_client::FakeControlClient::connect(&sock, "0.4.5")
        .await
        .unwrap();

    // Drive the lead's propose_plan call on a background task.
    let state_for_plan = state.clone();
    let plan_handle = tokio::spawn(async move {
        handle_propose_plan(
            &state_for_plan,
            ProposePlanArgs {
                plan: ApprovalPlan {
                    summary: "phase-1 migration".into(),
                    rationale: Some("prep worktrees before fan-out".into()),
                    resources: vec!["3 worktrees off main".into()],
                    risks: vec![],
                    rollback: Some("drop worktrees; nothing committed".into()),
                },
                timeout_secs: Some(5),
                ..Default::default()
            },
        )
        .await
    });

    // Expect the TUI to receive an ApprovalRequest with kind=Plan.
    let ev = client
        .recv_timeout(Duration::from_secs(2))
        .await
        .unwrap()
        .expect("approval_request event");
    let (request_id, kind, plan) = match ev {
        ControlEvent::ApprovalRequest {
            request_id,
            kind,
            plan,
            ..
        } => (request_id, kind, plan),
        other => panic!("expected ApprovalRequest, got {other:?}"),
    };
    assert_eq!(kind, ApprovalKind::Plan);
    let plan = plan.expect("plan-kind requests must carry a structured plan");
    assert_eq!(
        plan,
        ApprovalPlanWire {
            summary: "phase-1 migration".into(),
            rationale: Some("prep worktrees before fan-out".into()),
            resources: vec!["3 worktrees off main".into()],
            risks: vec![],
            rollback: Some("drop worktrees; nothing committed".into()),
        }
    );

    // Operator approves.
    client
        .send(&ControlOp::Approve {
            request_id,
            approved: true,
            comment: None,
            edited_summary: None,
            reason: None,
        })
        .await
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(3), plan_handle)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(resp.approved);
    assert!(state
        .root
        .plan_approved
        .load(std::sync::atomic::Ordering::Acquire));

    // Now spawn_worker no longer hits the plan-approval gate. It may
    // fail downstream (no git worktree set up) but the failure must not
    // be the plan-approval one.
    let res = handle_spawn_worker(
        &state,
        SpawnWorkerArgs {
            prompt: "post-approval work".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
            meta: None,
        },
    )
    .await;
    if let Err(e) = &res {
        assert!(
            !e.to_string().contains("plan approval required"),
            "plan-approval gate still firing after approval: {e}"
        );
    }
}
