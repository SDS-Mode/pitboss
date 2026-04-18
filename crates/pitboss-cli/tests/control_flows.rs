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
        .worker_cancels
        .write()
        .await
        .insert("w-1".into(), worker_token);
    state.workers.write().await.insert(
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
            .request("lead".into(), "spawn 3".into(), Duration::from_secs(5))
            .await
    });

    // Give the request a moment to queue.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(state.approval_queue.lock().await.len(), 1);

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
        .request("lead".into(), "spawn".into(), Duration::from_millis(200))
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
        .request("lead".into(), "spawn".into(), Duration::from_millis(200))
        .await
        .unwrap();
    assert!(!resp.approved);
    assert_eq!(resp.comment.as_deref(), Some("no operator available"));
}
