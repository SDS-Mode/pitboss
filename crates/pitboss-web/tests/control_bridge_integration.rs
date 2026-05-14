//! Integration tests for the control-socket bridge.
//!
//! After PR-Q of #438, the bridge no longer rolls its own UnixStream
//! reader — the read side is driven by
//! `pitboss_core::stream::open_run_stream(ReplayThenLive)` (in subscriber
//! mode), and op replies reach SSE subscribers via the dispatcher's
//! broadcast bus rather than the writer's read half. Mocking that with
//! a fake `UnixListener` is no longer practical, so these tests spin up
//! a real dispatcher via `pitboss_cli::control::server::start_control_server`
//! and exercise the full path end-to-end.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pitboss_cli::control::protocol::{ControlEvent, ControlOp};
use pitboss_cli::control::server::start_control_server;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};
use pitboss_cli::manifest::resolve::ResolvedManifest;
use pitboss_cli::manifest::schema::WorktreeCleanup;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tempfile::TempDir;
use tokio::time::timeout;

#[path = "../src/control_bridge.rs"]
mod control_bridge;

use control_bridge::{BridgeError, ControlBridge};

/// Wait until the dispatcher's broadcast bus has at least `n` receivers
/// attached. Tokio `broadcast::Receiver` only sees messages sent after
/// it subscribed, so tests that broadcast a single envelope after
/// `bridge.subscribe()` must wait for the bridge's subscriber-mode
/// pump to attach via its bus_bridge before sending — otherwise the
/// envelope is delivered only to whoever was attached at send time
/// (the persistence subscriber alone, which silently drops with
/// `emit_event_stream = false`).
///
/// The initial baseline is 1 (the persistence subscriber spawned by
/// `DispatchState::new`). Each connected control client adds one. For
/// the bridge we expect baseline + 1 (the subscriber pump's connection).
async fn wait_for_bus_receivers(state: &Arc<DispatchState>, target: usize, deadline: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if state.root.events_tx.receiver_count() >= target {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "bus receiver count did not reach {target} within {deadline:?} (have {})",
        state.root.events_tx.receiver_count()
    );
}

/// Build a minimal `DispatchState` + run subdir + run id sufficient for
/// the control-socket lifecycle. The bridge talks to it via the same
/// per-run socket path it would use in production.
async fn make_test_state(runs_dir: &TempDir) -> (Arc<DispatchState>, uuid::Uuid, PathBuf) {
    let run_id = uuid::Uuid::now_v7();
    let run_subdir = runs_dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
    let manifest = ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir: runs_dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_budget_usd: None,
        lead_timeout_secs: None,
        default_approval_policy: Some(ApprovalPolicy::Block),
        denial_termination_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        communication: Default::default(),
        lifecycle: None,
        worker_types: vec![],
        sublead_types: vec![],
        require_actor_type: false,
        untyped_actor_policy: Default::default(),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(runs_dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        String::new(),
        spawner,
        PathBuf::from("/bin/true"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir.clone(),
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    (state, run_id, run_subdir)
}

/// PR-Q regression: the bridge's unified-API pump forwards envelopes
/// broadcast by `broadcast_control_event` to SSE subscribers.
#[tokio::test]
async fn bridge_subscribe_returns_dispatcher_events() {
    let runs_dir = TempDir::new().unwrap();
    let (state, run_id, run_subdir) = make_test_state(&runs_dir).await;
    // Park the socket inside the per-run dir so the bridge's
    // run-dir-fallback resolver finds it without an XDG_RUNTIME_DIR
    // shim.
    let sock = run_subdir.join("control.sock");
    let _h = start_control_server(
        sock,
        "test-0.0.1".into(),
        run_id.to_string(),
        "flat".into(),
        state.clone(),
    )
    .await
    .unwrap();

    let bridge = ControlBridge::new(runs_dir.path().to_path_buf());
    let mut rx = bridge
        .subscribe(&run_id.to_string())
        .await
        .expect("subscribe");

    // Wait for the bridge's subscriber-mode pump to attach its
    // bus_bridge inside the dispatcher before we broadcast — otherwise
    // a Tokio broadcast send would only reach the persistence
    // subscriber (which is the baseline).
    // 1 baseline (persistence) + 2 from the bridge's two connections
    // (writer's bus_bridge + subscriber-mode pump's bus_bridge).
    wait_for_bus_receivers(&state, 3, Duration::from_secs(3)).await;

    // Broadcast a recognizable event on the dispatcher's bus. The
    // bridge's unified-API pump receives it via its subscriber-mode
    // socket and re-broadcasts to SSE clients.
    let envelope = pitboss_cli::control::protocol::EventEnvelope {
        actor_path: pitboss_cli::dispatch::actor::ActorPath::default(),
        seq: 0, // overwritten
        event: ControlEvent::RunFinished {
            summary: pitboss_cli::control::protocol::RunFinishedSummary {
                tasks_total: 3,
                tasks_failed: 1,
            },
        },
    };
    state.root.broadcast_control_event(envelope).await;

    // Skim until we find the RunFinished envelope. Tolerate intervening
    // bus traffic (the dispatcher's own Hello can land here via the
    // subscriber socket's bus_bridge, and store-activity ticks at 1 s).
    let mut saw = false;
    for _ in 0..8 {
        let env = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("envelope timeout")
            .expect("recv");
        if let ControlEvent::RunFinished { summary } = &env.event {
            assert_eq!(summary.tasks_total, 3);
            assert_eq!(summary.tasks_failed, 1);
            saw = true;
            break;
        }
    }
    assert!(saw, "subscriber must receive broadcast envelope");
}

/// Cold runs (no live socket) return 404 — the SSE feed is strictly
/// live; disk replay belongs to the dedicated Replay tab.
#[tokio::test]
async fn bridge_returns_not_found_when_no_socket() {
    let tmp = TempDir::new().unwrap();
    let bridge = ControlBridge::new(tmp.path().to_path_buf());
    let err = bridge
        .subscribe("nonexistent-run-id-12345")
        .await
        .expect_err("should fail");
    assert!(matches!(err, BridgeError::NotFound), "got {err:?}");
}

/// Multiple `subscribe()` callers share one `Entry` (one
/// writer-mode + one subscriber-mode dispatcher connection regardless
/// of how many SSE clients attach).
#[tokio::test]
async fn bridge_subscribe_shares_connection_for_multiple_subscribers() {
    let runs_dir = TempDir::new().unwrap();
    let (state, run_id, run_subdir) = make_test_state(&runs_dir).await;
    let sock = run_subdir.join("control.sock");
    let _h = start_control_server(
        sock,
        "test-0.0.1".into(),
        run_id.to_string(),
        "flat".into(),
        state.clone(),
    )
    .await
    .unwrap();

    let bridge = ControlBridge::new(runs_dir.path().to_path_buf());
    let mut rx1 = bridge
        .subscribe(&run_id.to_string())
        .await
        .expect("subscribe 1");
    let mut rx2 = bridge
        .subscribe(&run_id.to_string())
        .await
        .expect("subscribe 2");

    // Same Entry → same bus_bridge → baseline + 1. Two `subscribe()`
    // calls don't open two dispatcher connections.
    // 1 baseline (persistence) + 2 from the bridge's two connections
    // (writer's bus_bridge + subscriber-mode pump's bus_bridge).
    wait_for_bus_receivers(&state, 3, Duration::from_secs(3)).await;
    assert_eq!(
        state.root.events_tx.receiver_count(),
        3,
        "two subscribe() calls must share one dispatcher connection pair \
         (writer + subscriber + baseline persistence = 3 bus receivers)",
    );

    // Broadcast one envelope; both receivers must observe it via the
    // shared Entry's broadcast tx.
    let envelope = pitboss_cli::control::protocol::EventEnvelope {
        actor_path: pitboss_cli::dispatch::actor::ActorPath::default(),
        seq: 0,
        event: ControlEvent::SubleadTerminated {
            sublead_id: "S1".into(),
            spent_usd: 0.5,
            unspent_usd: 0.5,
            outcome: "success".into(),
        },
    };
    state.root.broadcast_control_event(envelope).await;

    let mut saw1 = false;
    for _ in 0..8 {
        let env = timeout(Duration::from_secs(2), rx1.recv())
            .await
            .expect("rx1 timeout")
            .expect("rx1");
        if matches!(env.event, ControlEvent::SubleadTerminated { .. }) {
            saw1 = true;
            break;
        }
    }
    let mut saw2 = false;
    for _ in 0..8 {
        let env = timeout(Duration::from_secs(2), rx2.recv())
            .await
            .expect("rx2 timeout")
            .expect("rx2");
        if matches!(env.event, ControlEvent::SubleadTerminated { .. }) {
            saw2 = true;
            break;
        }
    }
    assert!(saw1, "rx1 must see the broadcast event");
    assert!(saw2, "rx2 must see the broadcast event");
}

/// PR-Q end-to-end: bridge.send_op writes via the writer-mode
/// connection, the dispatcher processes the op, the reply is broadcast
/// via `events_tx`, and the bridge's subscriber-mode pump delivers it
/// to the SSE receiver.
#[tokio::test]
async fn bridge_send_op_round_trips_through_dispatcher() {
    let runs_dir = TempDir::new().unwrap();
    let (state, run_id, run_subdir) = make_test_state(&runs_dir).await;

    // Install a worker so ListWorkers returns a non-empty snapshot —
    // makes the assertion specific.
    state.root.workers.write().await.insert(
        "w-1".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );
    state
        .root
        .worker_prompts
        .write()
        .await
        .insert("w-1".into(), "look into the bug".into());

    let sock = run_subdir.join("control.sock");
    let _h = start_control_server(
        sock,
        "test-0.0.1".into(),
        run_id.to_string(),
        "flat".into(),
        state.clone(),
    )
    .await
    .unwrap();

    let bridge = ControlBridge::new(runs_dir.path().to_path_buf());
    let mut rx = bridge
        .subscribe(&run_id.to_string())
        .await
        .expect("subscribe");

    // Wait for the writer + subscriber connections' bus_bridges to
    // attach before POSTing the op — otherwise the WorkersSnapshot
    // broadcast might fire while the subscriber pump is still mid-handshake.
    wait_for_bus_receivers(&state, 3, Duration::from_secs(3)).await;

    // POST ListWorkers via the bridge's writer connection.
    bridge
        .send_op(&run_id.to_string(), &ControlOp::ListWorkers)
        .await
        .expect("send_op");

    let mut saw = false;
    for _ in 0..8 {
        let env = timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("recv timeout")
            .expect("recv");
        if let ControlEvent::WorkersSnapshot { workers } = &env.event {
            assert!(
                workers.iter().any(|w| w.task_id == "w-1"),
                "snapshot must include w-1: {workers:?}"
            );
            saw = true;
            break;
        }
    }
    assert!(
        saw,
        "WorkersSnapshot must reach the subscriber receiver via the broadcast bus",
    );
}

/// The bridge owns the Hello handshake; clients can't smuggle their
/// own Hello through `send_op`.
#[tokio::test]
async fn bridge_send_op_rejects_client_hello() {
    let runs_dir = TempDir::new().unwrap();
    let run_id = "01950000-0000-7000-8000-000000000004";
    let run_dir = runs_dir.path().join(run_id);
    std::fs::create_dir_all(&run_dir).unwrap();
    // Bind a listener so the path exists but never accept — Rejected
    // must fire on the up-front variant check before any IO.
    let _listener = tokio::net::UnixListener::bind(run_dir.join("control.sock")).unwrap();

    let bridge = ControlBridge::new(runs_dir.path().to_path_buf());
    let err = bridge
        .send_op(
            run_id,
            &ControlOp::Hello {
                client_version: "spoof/0.0.0".into(),
                mode: pitboss_cli::control::protocol::ClientMode::default(),
            },
        )
        .await
        .expect_err("hello must be rejected");
    assert!(matches!(err, BridgeError::Rejected(_)), "got {err:?}");
}
