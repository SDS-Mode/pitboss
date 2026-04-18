use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use pitboss_cli::notify::{
    NotificationEnvelope, NotificationRouter, NotificationSink, PitbossEvent, Severity, SinkFilter,
};
use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc};

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
