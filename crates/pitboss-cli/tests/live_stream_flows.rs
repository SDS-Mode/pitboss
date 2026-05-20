//! Integration tests for `pitboss_cli::stream::open_live_run_stream`.
//! Stand up a real control server, point the live consumer at it, and
//! assert that disk replay precedes socket envelopes in `ReplayThenLive`
//! mode and that the socket arm produces `Event` items in `LiveOnly`.
//!
//! PR-C of #438.

use futures_util::StreamExt;
use pitboss_cli::control::protocol::{ControlEvent, ControlOp};
use pitboss_cli::control::server::start_control_server;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};
use pitboss_cli::manifest::resolve::ResolvedManifest;
use pitboss_cli::manifest::schema::WorktreeCleanup;
use pitboss_cli::stream::{
    open_live_run_stream, open_run_session, LiveStreamMode, LiveStreamPayload,
};
use pitboss_core::parser::TokenUsage;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::record::{TaskRecord, TaskStatus};
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

fn rec(task_id: &str) -> TaskRecord {
    let t = chrono::Utc::now();
    TaskRecord {
        task_id: task_id.into(),
        status: TaskStatus::Success,
        exit_code: Some(0),
        started_at: t,
        ended_at: t,
        duration_ms: 0,
        worktree_path: None,
        log_path: PathBuf::from("/dev/null"),
        token_usage: TokenUsage::default(),
        claude_session_id: None,
        final_message_preview: None,
        final_message: None,
        parent_task_id: None,
        pause_count: 0,
        reprompt_count: 0,
        approvals_requested: 0,
        approvals_approved: 0,
        approvals_rejected: 0,
        model: None,
        failure_reason: None,
        cost_usd: None,
        actor_type: None,
        terminate_reason: None,
    }
}

fn manifest(run_dir: PathBuf) -> ResolvedManifest {
    ResolvedManifest {
        manifest_schema_version: 0,
        name: None,
        max_parallel_tasks: Some(4),
        halt_on_failure: false,
        run_dir,
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
            resource_sample_secs: 0,
        claude_setting_sources: None,
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
        agent_profiles: ::std::collections::HashMap::new(),
    }
}

/// End-to-end: stand up a control server, point the unified live
/// consumer at it in `LiveOnly` mode, confirm that the server's Hello
/// arrives as a `LiveStreamPayload::Event` carrying a non-zero seq
/// (PR-B's per-connection counter).
/// PR-N of #438: end-to-end smoke for `pitboss-core::stream`'s
/// LiveOnly transport. Connects to a real dispatcher, performs the
/// Hello + Subscribe handshake, and asserts that dispatcher-pushed
/// envelopes surface as `RunStreamPayload::Event` items. The
/// pitboss-core unit tests use a mock dispatcher; this test pins the
/// integration end with the real server handler.
#[tokio::test]
async fn pitboss_core_live_only_against_real_dispatcher() {
    use futures_util::StreamExt as _;
    use pitboss_core::stream::{open_run_stream, RunStreamPayload, StreamMode};
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest(dir.path().to_path_buf()),
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

    // The unified API resolves the socket via `resolve_control_socket`
    // (XDG or run_dir/control.sock). Point both branches at this run's
    // dir so the listener path lines up with what the consumer dials.
    let sock = run_subdir.join("control.sock");
    let _h = start_control_server(
        sock,
        "0.13.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    // Force the XDG branch off so the resolver falls through to
    // run_dir/control.sock — keeps the test hermetic on hosts with a
    // populated XDG_RUNTIME_DIR.
    std::env::set_var("XDG_RUNTIME_DIR", dir.path().join("xdg-empty"));
    let stream = open_run_stream(&run_subdir, StreamMode::LiveOnly);
    let mut stream = std::pin::pin!(stream);

    // The first envelope the dispatcher emits is its server Hello
    // (seq=1 on the wire, surfaced as a `RunStreamPayload::Event` by
    // the unified API). Plus the subscribe-ack and possibly an
    // activity tick; tolerate one or two intervening items.
    let mut saw_hello = false;
    for _ in 0..4 {
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
        let Ok(Some(item)) = next else { break };
        match &item.payload {
            RunStreamPayload::Event(env) => {
                if env.event.get("event").and_then(|v| v.as_str()) == Some("hello") {
                    saw_hello = true;
                    assert_eq!(env.seq, 1, "server hello carries dispatcher seq=1");
                    break;
                }
            }
            other => panic!("LiveOnly must only emit Event items; got {other:?}"),
        }
    }
    std::env::remove_var("XDG_RUNTIME_DIR");
    assert!(
        saw_hello,
        "expected server Hello within first few envelopes"
    );
}

#[tokio::test]
async fn live_only_receives_server_hello_envelope() {
    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest(dir.path().to_path_buf()),
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
    // Seed a worker so the server's WorkersSnapshot has content.
    state.root.workers.states.write().await.insert(
        "w-live".into(),
        WorkerState::Running {
            started_at: chrono::Utc::now(),
            session_id: Some("sess".into()),
        },
    );

    let sock = dir.path().join("live-only.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.13.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    // Open the unified live stream. Take a couple of items with a
    // timeout so the test doesn't hang if anything regresses.
    let stream = open_live_run_stream(run_subdir.clone(), Some(sock), LiveStreamMode::LiveOnly);
    let mut stream = std::pin::pin!(stream);

    let mut hellos = 0usize;
    let mut last_seq = 0u64;
    for _ in 0..3 {
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
        match next {
            Ok(Some(item)) => {
                assert!(
                    item.seq > last_seq,
                    "seqs monotonic: {} > {}",
                    item.seq,
                    last_seq
                );
                last_seq = item.seq;
                if let LiveStreamPayload::Event(env) = &item.payload {
                    if matches!(
                        env.event,
                        pitboss_cli::control::protocol::ControlEvent::Hello { .. }
                    ) {
                        hellos += 1;
                    }
                } else {
                    panic!(
                        "LiveOnly should only emit Event payloads, got {:?}",
                        item.payload
                    );
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(hellos, 1, "exactly one Hello envelope arrived");
    assert!(last_seq >= 1, "at least one envelope with seq >= 1");
}

/// `ReplayThenLive` against a server with prior summary.jsonl rows:
/// disk items arrive first, then live envelopes. The seam is the
/// transition from `LiveStreamPayload::Task` to `LiveStreamPayload::Event`.
#[tokio::test]
async fn replay_then_live_drains_disk_then_emits_socket_events() {
    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();

    // Prime the run dir with one task in summary.jsonl.
    let jsonl_path = run_subdir.join("summary.jsonl");
    let line = serde_json::to_string(&rec("prior-worker")).unwrap();
    tokio::fs::write(&jsonl_path, format!("{line}\n"))
        .await
        .unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest(dir.path().to_path_buf()),
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

    let sock = dir.path().join("rt-live.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.13.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let stream = open_live_run_stream(
        run_subdir.clone(),
        Some(sock),
        LiveStreamMode::ReplayThenLive,
    );
    let mut stream = std::pin::pin!(stream);

    // First item: the prior task from disk.
    let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .expect("first item");
    match &first.payload {
        LiveStreamPayload::Task(t) => {
            assert_eq!(t.task_id, "prior-worker");
        }
        other => panic!("expected Task first, got {other:?}"),
    }

    // Subsequent items should include at least one Event (the server
    // Hello). We poll a few times with timeout to be robust to the
    // store-activity ticker.
    let mut saw_event = false;
    for _ in 0..3 {
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
        if let Ok(Some(item)) = next {
            if matches!(item.payload, LiveStreamPayload::Event(_)) {
                saw_event = true;
                break;
            }
        } else {
            break;
        }
    }
    assert!(
        saw_event,
        "live socket arm should produce at least one Event after disk drain"
    );
}

/// `open_run_session` returns a writer alongside the stream when a
/// live socket is available. Sending an op through the writer must
/// reach the server and produce an `OpAcked` envelope on the stream.
#[tokio::test]
async fn run_session_writer_send_op_round_trips_to_server() {
    let dir = TempDir::new().unwrap();
    let run_id = Uuid::now_v7();
    let run_subdir = dir.path().join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest(dir.path().to_path_buf()),
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

    let sock = dir.path().join("session.sock");
    let _h = start_control_server(
        sock.clone(),
        "0.13.0".into(),
        run_id.to_string(),
        "flat".into(),
        state,
    )
    .await
    .unwrap();

    let session = open_run_session(
        run_subdir.clone(),
        Some(sock.clone()),
        LiveStreamMode::LiveOnly,
    )
    .await;
    let writer = session.writer.expect("writer present when socket exists");
    let mut stream = std::pin::pin!(session.stream);

    // Drain the hello.
    let _hello = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .expect("hello envelope");

    // Send an op via the writer; the server replies with a
    // `WorkersSnapshot` envelope on the read stream. (ListWorkers is
    // the op that exercises the read-back path without requiring
    // worker state — it returns an empty snapshot.)
    writer
        .send_op(&ControlOp::ListWorkers)
        .await
        .expect("send_op succeeds");

    let mut saw_snapshot = false;
    for _ in 0..4 {
        let next = tokio::time::timeout(Duration::from_secs(2), stream.next()).await;
        if let Ok(Some(item)) = next {
            if let LiveStreamPayload::Event(env) = item.payload {
                if matches!(env.event, ControlEvent::WorkersSnapshot { .. }) {
                    saw_snapshot = true;
                    break;
                }
            }
        } else {
            break;
        }
    }
    assert!(
        saw_snapshot,
        "writer.send_op should produce a WorkersSnapshot envelope back on the stream"
    );
}

/// When the socket doesn't exist, `open_run_session` still returns a
/// session but the writer is `None`. The stream completes promptly
/// (no live items, no disk items in `LiveOnly` mode).
#[tokio::test]
async fn run_session_writer_none_when_socket_missing() {
    let dir = TempDir::new().unwrap();
    let run_subdir = dir.path().join("no-run");
    tokio::fs::create_dir_all(&run_subdir).await.unwrap();
    let bogus = dir.path().join("nope.sock");

    let session = open_run_session(run_subdir, Some(bogus), LiveStreamMode::LiveOnly).await;
    assert!(session.writer.is_none());
    let items: Vec<_> = session.stream.collect().await;
    assert!(items.is_empty());
}
