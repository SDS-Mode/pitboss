use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use pitboss_cli::notify::{
    NotificationEnvelope, NotificationRouter, NotificationSink, PitbossEvent, Severity, SinkFilter,
};
use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc, Mutex};

/// Counting sink for testing — increments a counter on each emit.
struct CountingSink {
    id: String,
    count: Arc<AtomicUsize>,
}

impl CountingSink {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl NotificationSink for CountingSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, _env: &NotificationEnvelope) -> Result<()> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Capturing sink for testing — stores all emitted notifications
struct CapturingSink {
    id: String,
    notifications: Arc<Mutex<Vec<NotificationEnvelope>>>,
}

impl CapturingSink {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            notifications: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn notifications(&self) -> Vec<NotificationEnvelope> {
        self.notifications.lock().unwrap().clone()
    }
}

#[async_trait]
impl NotificationSink for CapturingSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, env: &NotificationEnvelope) -> Result<()> {
        self.notifications.lock().unwrap().push(env.clone());
        Ok(())
    }
}

#[tokio::test]
async fn router_coalesces_repeat_dedup_keys() {
    let sink = Arc::new(CountingSink::new("test-sink"));
    let router = NotificationRouter::new(vec![(
        sink.clone(),
        SinkFilter {
            events: None,
            severity_min: Severity::Info,
        },
    )]);

    let env1 = NotificationEnvelope::new(
        "run-1",
        Severity::Warning,
        PitbossEvent::RunFinished {
            run_id: "run-1".into(),
            tasks_total: 1,
            tasks_failed: 0,
            duration_ms: 100,
            spent_usd: 0.01,
        },
        Utc::now(),
    );

    router.dispatch(env1.clone()).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let after_first = sink.count();

    router.dispatch(env1.clone()).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let after_second = sink.count();

    assert_eq!(after_first, 1, "first dispatch should emit once");
    assert_eq!(
        after_second, 1,
        "second dispatch with same dedup_key should be deduplicated"
    );
}

#[tokio::test]
async fn router_filter_by_event_kind() {
    let sink = Arc::new(CountingSink::new("test-sink"));
    let router = NotificationRouter::new(vec![(
        sink.clone(),
        SinkFilter {
            events: Some(vec!["approval_request".to_string()]),
            severity_min: Severity::Info,
        },
    )]);

    let approval_env = NotificationEnvelope::new(
        "run-1",
        Severity::Warning,
        PitbossEvent::ApprovalRequest {
            request_id: "req-1".into(),
            task_id: "w-1".into(),
            summary: "test".into(),
        },
        Utc::now(),
    );

    let finished_env = NotificationEnvelope::new(
        "run-1",
        Severity::Info,
        PitbossEvent::RunFinished {
            run_id: "run-1".into(),
            tasks_total: 1,
            tasks_failed: 0,
            duration_ms: 100,
            spent_usd: 0.01,
        },
        Utc::now(),
    );

    router.dispatch(approval_env).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let after_approval = sink.count();

    router.dispatch(finished_env).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let after_finished = sink.count();

    assert_eq!(after_approval, 1, "approval_request should be emitted");
    assert_eq!(
        after_finished, 1,
        "run_finished should be filtered out, count unchanged"
    );
}

#[tokio::test]
async fn router_filter_by_severity_min() {
    let sink = Arc::new(CountingSink::new("test-sink"));
    let router = NotificationRouter::new(vec![(
        sink.clone(),
        SinkFilter {
            events: None,
            severity_min: Severity::Warning,
        },
    )]);

    let info_env = NotificationEnvelope::new(
        "run-1",
        Severity::Info,
        PitbossEvent::RunFinished {
            run_id: "run-1".into(),
            tasks_total: 1,
            tasks_failed: 0,
            duration_ms: 100,
            spent_usd: 0.01,
        },
        Utc::now(),
    );

    let warning_env = NotificationEnvelope::new(
        "run-2",
        Severity::Warning,
        PitbossEvent::RunFinished {
            run_id: "run-2".into(),
            tasks_total: 1,
            tasks_failed: 0,
            duration_ms: 100,
            spent_usd: 0.01,
        },
        Utc::now(),
    );

    router.dispatch(info_env).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let after_info = sink.count();

    router.dispatch(warning_env).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let after_warning = sink.count();

    assert_eq!(
        after_info, 0,
        "Info severity below threshold should not be emitted"
    );
    assert_eq!(after_warning, 1, "Warning severity should be emitted");
}

#[tokio::test]
async fn approval_pending_notification_fires_on_enqueue() {
    use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
    use pitboss_cli::manifest::resolve::ResolvedManifest;
    use pitboss_cli::manifest::schema::WorktreeCleanup;
    use pitboss_cli::mcp::approval::ApprovalBridge;
    use pitboss_core::process::{ProcessSpawner, TokioSpawner};
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::TempDir;
    use uuid::Uuid;

    // Set up a fake notifier that captures notifications
    let sink = Arc::new(CapturingSink::new("test-capturing-sink"));
    let router = NotificationRouter::new(vec![(
        sink.clone(),
        SinkFilter {
            events: None,
            severity_min: Severity::Info,
        },
    )]);

    // Set up dispatch state with the notification router
    let dir = TempDir::new().unwrap();
    let manifest = ResolvedManifest {
        name: None,
        max_parallel_tasks: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(1.0),
        lead_timeout_secs: None,
        default_approval_policy: Some(ApprovalPolicy::Block),
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
        container: None,
        mcp_servers: vec![],
        lifecycle: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_id = Uuid::now_v7();
    std::mem::forget(dir);
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
        PathBuf::from("/tmp"),
        ApprovalPolicy::Block,
        Some(Arc::new(router)),
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));

    // Request an approval with Block policy — should enqueue and fire notification
    let bridge = Arc::new(ApprovalBridge::new(state));
    let _request_handle = {
        let bridge = bridge.clone();
        tokio::spawn(async move {
            let _ = bridge
                .request(
                    "lead".into(),
                    "spawn 2 workers".into(),
                    None,
                    pitboss_cli::control::protocol::ApprovalKind::Action,
                    Duration::from_secs(1),
                    None,
                    None,
                )
                .await;
        })
    };

    // Give time for the notification to be dispatched
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Check that exactly one ApprovalPending notification was emitted
    let notifications = sink.notifications();
    assert_eq!(
        notifications.len(),
        2,
        "Expected 2 notifications: ApprovalRequest + ApprovalPending"
    );

    // Check for ApprovalRequest (fired on request)
    let approval_request_notif = notifications
        .iter()
        .find(|n| matches!(n.event, PitbossEvent::ApprovalRequest { .. }))
        .expect("ApprovalRequest notification should be present");
    assert_eq!(approval_request_notif.event.kind(), "approval_request");

    // Check for ApprovalPending (fired on enqueue)
    let approval_pending_notif = notifications
        .iter()
        .find(|n| matches!(n.event, PitbossEvent::ApprovalPending { .. }))
        .expect("ApprovalPending notification should be present");
    assert_eq!(approval_pending_notif.event.kind(), "approval_pending");
    if let PitbossEvent::ApprovalPending {
        summary, task_id, ..
    } = &approval_pending_notif.event
    {
        assert_eq!(summary, "spawn 2 workers");
        assert_eq!(task_id, "lead");
    } else {
        panic!("Expected ApprovalPending variant");
    }
}
