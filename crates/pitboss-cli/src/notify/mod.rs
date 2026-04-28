//! Notification sink system. See
//! docs/superpowers/specs/2026-04-17-pitboss-v041-notifications-design.md
//! for the full design.

#![allow(dead_code)] // Wired up gradually across Tasks 2-21.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod config;
pub mod parent;
pub mod sinks;

/// Severity levels for `NotificationEnvelope`. Matches syslog heritage +
/// PagerDuty/Opsgenie conventions. Ordered so filters can say
/// `severity_min = "warning"` and include Error + Critical.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Lifecycle events pitboss can emit to notification sinks. Typed
/// enum, not a context dict — sinks `match` exhaustively. Each
/// variant carries its own specific fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PitbossEvent {
    ApprovalRequest {
        request_id: String,
        task_id: String,
        summary: String,
    },
    ApprovalPending {
        request_id: String,
        task_id: String,
        summary: String,
    },
    /// Fires once at run start, immediately after the dispatcher has
    /// minted a run id and created the run directory. Lets a parent
    /// orchestrator register the run in its own bookkeeping (`runs` table,
    /// budget gate, retention sweep) before any tokens land. The
    /// `parent_run_id` is populated from `PITBOSS_RUN_ID` in the env when a
    /// pitboss dispatch is itself launched from inside another pitboss-
    /// spawned actor (i.e. an agent with `pitboss` on its PATH calling
    /// `pitboss dispatch <child.toml>`); `None` for top-level dispatches.
    RunDispatched {
        run_id: String,
        parent_run_id: Option<String>,
        manifest_path: String,
        /// "flat" or "hierarchical".
        mode: String,
        /// Resolved value of `[lifecycle].survive_parent` from the manifest.
        /// `false` for manifests without a `[lifecycle]` section. Lets the
        /// orchestrator decide whether to include this run's process group
        /// in any cancel-tree-walk it performs (issue #133-A).
        #[serde(default)]
        survive_parent: bool,
    },
    RunFinished {
        run_id: String,
        tasks_total: usize,
        tasks_failed: usize,
        duration_ms: u64,
        spent_usd: f64,
    },
    BudgetExceeded {
        run_id: String,
        spent_usd: f64,
        budget_usd: f64,
    },
}

impl PitbossEvent {
    /// Short string identifier used for filter lists (`events = [...]`)
    /// and dedup_key construction.
    pub fn kind(&self) -> &'static str {
        match self {
            PitbossEvent::ApprovalRequest { .. } => "approval_request",
            PitbossEvent::ApprovalPending { .. } => "approval_pending",
            PitbossEvent::RunDispatched { .. } => "run_dispatched",
            PitbossEvent::RunFinished { .. } => "run_finished",
            PitbossEvent::BudgetExceeded { .. } => "budget_exceeded",
        }
    }
}

/// Carried to every sink on every emit. Typed + correlated.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationEnvelope {
    /// "{run_id}:{event_kind}[:{discriminator}]" — PagerDuty/Opsgenie
    /// style correlation ID for retry coalescing + downstream grouping.
    pub dedup_key: String,
    pub severity: Severity,
    pub event: PitbossEvent,
    pub ts: DateTime<Utc>,
    /// run_id (or task_id where event is scoped to one worker).
    pub source: String,
}

impl NotificationEnvelope {
    /// Build an envelope with auto-derived dedup_key from (run_id, event kind,
    /// and event-specific discriminator).
    pub fn new(run_id: &str, severity: Severity, event: PitbossEvent, ts: DateTime<Utc>) -> Self {
        let discriminator = match &event {
            PitbossEvent::ApprovalRequest { request_id, .. } => Some(request_id.as_str()),
            PitbossEvent::ApprovalPending { request_id, .. } => Some(request_id.as_str()),
            PitbossEvent::RunDispatched { .. } | PitbossEvent::RunFinished { .. } => None,
            PitbossEvent::BudgetExceeded { .. } => Some("first"),
        };
        let dedup_key = match discriminator {
            Some(d) => format!("{run_id}:{}:{d}", event.kind()),
            None => format!("{run_id}:{}", event.kind()),
        };
        Self {
            dedup_key,
            severity,
            event,
            ts,
            source: run_id.to_string(),
        }
    }
}

/// Transport abstraction: given an envelope, put it somewhere operator-visible.
/// Implemented by LogSink, WebhookSink, SlackSink, DiscordSink.
#[async_trait]
pub trait NotificationSink: Send + Sync {
    /// Unique stable identifier used for log/audit lines — e.g.
    /// "log", "webhook:1", "slack:prod-alerts".
    fn id(&self) -> &str;

    /// Emit a single envelope. Fire-and-forget semantics: the router
    /// calls this inside a `tokio::spawn`. Errors are logged and
    /// recorded as `TaskEvent::NotificationFailed`; they never
    /// propagate to the dispatcher.
    async fn emit(&self, env: &NotificationEnvelope) -> Result<()>;
}

/// Filter for sink-specific event + severity matching.
#[derive(Debug, Clone)]
pub struct SinkFilter {
    /// If Some, only emit events in this list. If None, emit all.
    pub events: Option<Vec<String>>,
    /// Minimum severity to emit (info, warning, error, critical).
    pub severity_min: Severity,
}

impl SinkFilter {
    /// Check if event + severity should be emitted to this sink.
    pub fn matches(&self, env: &NotificationEnvelope) -> bool {
        if env.severity < self.severity_min {
            return false;
        }
        if let Some(ref allowed) = self.events {
            if !allowed.contains(&env.event.kind().to_string()) {
                return false;
            }
        }
        true
    }
}

impl From<&crate::notify::config::NotificationConfig> for SinkFilter {
    fn from(cfg: &crate::notify::config::NotificationConfig) -> Self {
        Self {
            events: cfg.events.clone(),
            severity_min: cfg.severity_min,
        }
    }
}

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Default LRU dedup cache size. The router has no persistent state — a
/// process restart inside a long run can re-emit `RunDispatched` /
/// `RunFinished` if the parent never acks them. 64 covers ~32 in-flight
/// approval pairs (`approval_request` + `approval_pending`) plus the four
/// run-scoped events with comfortable headroom; busy multi-task runs that
/// hit the ceiling can raise it via `PITBOSS_NOTIFY_DEDUP_CACHE_SIZE`.
pub const DEFAULT_DEDUP_CACHE_SIZE: usize = 64;

/// Env var operators can set to tune the LRU dedup cache size at process
/// start. Parsed once when [`NotificationRouter::new`] runs; ignored if
/// the value is non-numeric or zero. Issue #156 (L1).
pub const DEDUP_CACHE_SIZE_ENV: &str = "PITBOSS_NOTIFY_DEDUP_CACHE_SIZE";

/// Router fans envelopes to multiple sinks with LRU dedup and retry.
pub struct NotificationRouter {
    sinks: Vec<(Arc<dyn NotificationSink>, SinkFilter)>,
    dedup_cache: Mutex<LruCache<String, ()>>,
    /// Total emit-after-retry failures across the whole router lifetime.
    /// Counts one per `(sink, dedup_key)` exhausted retry — fatal 4xx
    /// short-circuits also count. Wrapped in `Arc` so per-emit
    /// `tokio::spawn` bodies can increment it without borrowing `&self`.
    /// Surfaced for operator dashboards and tests; never reset. #156 (M4).
    failed_emits_total: Arc<AtomicU64>,
    /// Per-run subdir written by the dispatcher right after `pitboss
    /// dispatch` mints a run id; consumed by [`Self::dispatch`] to write
    /// `TaskEvent::NotificationFailed` entries to a per-run JSONL on
    /// terminal failure. `None` for routers built before any run subdir
    /// exists (rare; only the test paths). Issue #156 (M4).
    run_subdir: Mutex<Option<std::path::PathBuf>>,
}

impl NotificationRouter {
    /// Create a router with the given sinks and filters. The LRU cache size
    /// honors `PITBOSS_NOTIFY_DEDUP_CACHE_SIZE` when set to a positive
    /// integer, otherwise falls back to [`DEFAULT_DEDUP_CACHE_SIZE`].
    pub fn new(sinks: Vec<(Arc<dyn NotificationSink>, SinkFilter)>) -> Self {
        let capacity = std::env::var(DEDUP_CACHE_SIZE_ENV)
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_DEDUP_CACHE_SIZE);
        Self::new_with_capacity(sinks, capacity)
    }

    /// Like [`Self::new`] but with an explicit dedup-cache size — used by
    /// tests that need deterministic cache behavior independent of the
    /// process env. `capacity` is clamped to a minimum of 1 (the underlying
    /// `LruCache` cannot be zero-sized).
    pub fn new_with_capacity(
        sinks: Vec<(Arc<dyn NotificationSink>, SinkFilter)>,
        capacity: usize,
    ) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).expect("capacity clamped >= 1");
        Self {
            sinks,
            dedup_cache: Mutex::new(LruCache::new(cap)),
            failed_emits_total: Arc::new(AtomicU64::new(0)),
            run_subdir: Mutex::new(None),
        }
    }

    /// Total terminal emit failures since router start. Exposed for tests
    /// and future operator dashboards. Issue #156 (M4).
    pub fn failed_emits_total(&self) -> u64 {
        self.failed_emits_total.load(Ordering::Relaxed)
    }

    /// Set the per-run subdir used for `TaskEvent::NotificationFailed`
    /// journal writes. Called by the dispatcher right after `run_subdir` is
    /// created so subsequent emit failures land in
    /// `<run_subdir>/notifications.jsonl`. Idempotent; the second call
    /// overwrites the first. Issue #156 (M4).
    pub fn set_run_subdir(&self, run_subdir: std::path::PathBuf) {
        if let Ok(mut slot) = self.run_subdir.lock() {
            *slot = Some(run_subdir);
        }
    }

    fn run_subdir_snapshot(&self) -> Option<std::path::PathBuf> {
        self.run_subdir.lock().ok().and_then(|g| g.clone())
    }

    /// Dispatch envelope to all matching sinks. Deduplicates by dedup_key
    /// and spawns fire-and-forget tasks with retry. Terminal failures are
    /// (a) logged at `error`, (b) counted in [`Self::failed_emits_total`],
    /// and (c) appended to `<run_subdir>/notifications.jsonl` when a run
    /// subdir is bound — so operators get a durable per-run audit trail
    /// instead of having to scrape journald.
    pub async fn dispatch(&self, env: NotificationEnvelope) -> Result<()> {
        {
            // Poison-recover instead of `.unwrap()`: the LRU cache is a
            // dedup *hint*, not an authoritative store. A panic in one
            // emit holding the lock would otherwise brick every future
            // notification with `PoisonError`. Recover the inner cache
            // (worst case: one duplicate emit) and keep going. #187.
            let mut cache = self
                .dedup_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cache.contains(&env.dedup_key) {
                return Ok(());
            }
            cache.put(env.dedup_key.clone(), ());
        }

        for (sink, filter) in &self.sinks {
            if !filter.matches(&env) {
                continue;
            }
            let sink = Arc::clone(sink);
            let env = env.clone();
            let run_subdir = self.run_subdir_snapshot();
            let counter = Arc::clone(&self.failed_emits_total);
            tokio::spawn(async move {
                if let Err(e) = emit_with_retry(&sink, &env).await {
                    tracing::error!(
                        sink_id = %sink.id(),
                        dedup_key = %env.dedup_key,
                        error = %e,
                        "notification emit failed after retries"
                    );
                    // Write the audit-trail line BEFORE bumping the counter.
                    // External observers (tests, status APIs) treat
                    // `failed_emits_total` as a synchronization barrier — a
                    // post-bump read of `notifications.jsonl` must see the
                    // line. With the previous order the counter could
                    // increment while the journal write was still in flight,
                    // producing flaky reads in
                    // `router_records_failed_emit_metric_and_journal_line`
                    // under slow-FS conditions on CI. The journal write is
                    // bounded and swallows its own errors, so this ordering
                    // can't deadlock the spawn.
                    if let Some(subdir) = run_subdir.as_ref() {
                        record_notification_failure(subdir, &sink, &env, &e).await;
                    }
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        Ok(())
    }
}

/// Append a `TaskEvent::NotificationFailed` line to
/// `<run_subdir>/notifications.jsonl`. Best-effort: any I/O error here is
/// only `tracing::warn`'d (failing to write the audit trail must never
/// crash a dispatcher). Issue #156 (M4).
async fn record_notification_failure(
    run_subdir: &std::path::Path,
    sink: &Arc<dyn NotificationSink>,
    env: &NotificationEnvelope,
    err: &anyhow::Error,
) {
    use chrono::Utc;
    use tokio::io::AsyncWriteExt;
    let event = crate::dispatch::events::TaskEvent::NotificationFailed {
        at: Utc::now(),
        sink_id: sink.id().to_string(),
        event_kind: env.event.kind().to_string(),
        error: err.to_string(),
    };
    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "could not serialize NotificationFailed event");
            return;
        }
    };
    let path = run_subdir.join("notifications.jsonl");
    let res = async {
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok::<(), std::io::Error>(())
    }
    .await;
    if let Err(e) = res {
        tracing::warn!(path = %path.display(), error = %e, "could not write notifications.jsonl");
    }
}

/// Try emitting 3 times with exponential backoff: 100ms, 300ms, 900ms.
/// Returns Ok on first success; Err on final failure. Non-retryable 4xx
/// client errors (except 429) short-circuit without further attempts —
/// they will fail identically every time and the delay just postpones the
/// inevitable failure notification.
async fn emit_with_retry(
    sink: &Arc<dyn NotificationSink>,
    env: &NotificationEnvelope,
) -> Result<()> {
    let backoffs = [100, 300, 900];
    for (attempt, &delay_ms) in backoffs.iter().enumerate() {
        match sink.emit(env).await {
            Ok(()) => return Ok(()),
            Err(e) if is_fatal(&e) => {
                tracing::warn!(
                    attempt = attempt + 1,
                    error = %e,
                    "notification emit failed fatally, not retrying"
                );
                return Err(e);
            }
            Err(e) if attempt < backoffs.len() - 1 => {
                tracing::warn!(
                    attempt = attempt + 1,
                    delay_ms,
                    error = %e,
                    "notification emit failed, retrying"
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
    Err(anyhow::anyhow!("emit_with_retry: exhausted attempts"))
}

/// Heuristic for determining if an error is fatal (should not retry).
/// A 4xx response (except 429 Too Many Requests) means the request is
/// malformed or unauthenticated — retrying doesn't help. Everything else
/// (network errors, 5xx, unknown) remains retryable.
fn is_fatal(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(reqwest_err) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = reqwest_err.status() {
                return status.is_client_error()
                    && status != reqwest::StatusCode::TOO_MANY_REQUESTS;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ord_is_info_warning_error_critical() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert!(Severity::Error < Severity::Critical);
    }

    #[test]
    fn severity_serde_roundtrip() {
        let s = serde_json::to_string(&Severity::Warning).unwrap();
        assert_eq!(s, "\"warning\"");
        let back: Severity = serde_json::from_str("\"critical\"").unwrap();
        assert_eq!(back, Severity::Critical);
    }

    #[test]
    fn pitboss_event_approval_request_roundtrip() {
        let ev = PitbossEvent::ApprovalRequest {
            request_id: "req-1".into(),
            task_id: "w-1".into(),
            summary: "spawn 3 workers".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"approval_request\""));
        assert!(s.contains("\"request_id\":\"req-1\""));
    }

    #[test]
    fn pitboss_event_run_finished_roundtrip() {
        let ev = PitbossEvent::RunFinished {
            run_id: "019d...".into(),
            tasks_total: 3,
            tasks_failed: 1,
            duration_ms: 12_345,
            spent_usd: 0.42,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"run_finished\""));
        assert!(s.contains("\"tasks_failed\":1"));
    }

    #[test]
    fn pitboss_event_run_dispatched_roundtrip_carries_survive_parent() {
        // Regression for issue #133-A: the `survive_parent` field must
        // serialize so the orchestrator can decide whether to include this
        // run's process group in any cancel-tree-walk it performs.
        let ev = PitbossEvent::RunDispatched {
            run_id: "019d-child".into(),
            parent_run_id: Some("019c-parent".into()),
            manifest_path: "/work/c.toml".into(),
            mode: "flat".into(),
            survive_parent: true,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"run_dispatched\""));
        assert!(s.contains("\"survive_parent\":true"));
        let back: PitbossEvent = serde_json::from_str(&s).unwrap();
        match back {
            PitbossEvent::RunDispatched { survive_parent, .. } => assert!(survive_parent),
            other => panic!("expected RunDispatched, got {other:?}"),
        }
    }

    #[test]
    fn pitboss_event_run_dispatched_back_compat_default_survive_parent_false() {
        // A pre-#133-A producer that omits `survive_parent` must still
        // deserialize cleanly with the field defaulting to false.
        let json = r#"{
            "kind":"run_dispatched",
            "run_id":"019d-x",
            "parent_run_id":null,
            "manifest_path":"/x",
            "mode":"flat"
        }"#;
        let ev: PitbossEvent = serde_json::from_str(json).unwrap();
        match ev {
            PitbossEvent::RunDispatched { survive_parent, .. } => assert!(!survive_parent),
            other => panic!("expected RunDispatched, got {other:?}"),
        }
    }

    #[test]
    fn pitboss_event_budget_exceeded_roundtrip() {
        let ev = PitbossEvent::BudgetExceeded {
            run_id: "019d...".into(),
            spent_usd: 1.51,
            budget_usd: 1.50,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"budget_exceeded\""));
    }

    #[test]
    fn notification_envelope_constructs() {
        let env = NotificationEnvelope {
            dedup_key: "run-1:run_finished".into(),
            severity: Severity::Info,
            event: PitbossEvent::RunFinished {
                run_id: "run-1".into(),
                tasks_total: 1,
                tasks_failed: 0,
                duration_ms: 100,
                spent_usd: 0.01,
            },
            ts: chrono::Utc::now(),
            source: "run-1".into(),
        };
        assert_eq!(env.event.kind(), "run_finished");
        assert_eq!(env.dedup_key, "run-1:run_finished");
    }

    #[test]
    fn notification_envelope_dedup_key_helper() {
        use chrono::Utc;
        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::ApprovalRequest {
                request_id: "req-9".into(),
                task_id: "lead".into(),
                summary: "s".into(),
            },
            Utc::now(),
        );
        assert_eq!(env.dedup_key, "run-1:approval_request:req-9");
    }

    #[test]
    fn pitboss_event_approval_pending_roundtrip() {
        let ev = PitbossEvent::ApprovalPending {
            request_id: "req-1".into(),
            task_id: "w-1".into(),
            summary: "spawn 3 workers".into(),
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"approval_pending\""));
        assert!(s.contains("\"request_id\":\"req-1\""));
    }

    #[test]
    fn notification_envelope_approval_pending_dedup_key() {
        use chrono::Utc;
        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::ApprovalPending {
                request_id: "req-5".into(),
                task_id: "lead".into(),
                summary: "enqueued approval".into(),
            },
            Utc::now(),
        );
        assert_eq!(env.dedup_key, "run-1:approval_pending:req-5");
    }

    /// #156 (M4) regression: a sink that always returns Err must drive
    /// `failed_emits_total` up by 1 per envelope and append a
    /// `NotificationFailed` line to `<run_subdir>/notifications.jsonl`.
    #[tokio::test]
    async fn router_records_failed_emit_metric_and_journal_line() {
        use chrono::Utc;
        struct AlwaysFails;
        #[async_trait]
        impl NotificationSink for AlwaysFails {
            fn id(&self) -> &str {
                "test-fail"
            }
            async fn emit(&self, _env: &NotificationEnvelope) -> Result<()> {
                Err(anyhow::anyhow!("synthetic emit failure"))
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let router = NotificationRouter::new_with_capacity(
            vec![(
                Arc::new(AlwaysFails),
                SinkFilter {
                    events: None,
                    severity_min: Severity::Info,
                },
            )],
            8,
        );
        router.set_run_subdir(tmp.path().to_path_buf());

        let env = NotificationEnvelope::new(
            "run-x",
            Severity::Info,
            PitbossEvent::RunFinished {
                run_id: "run-x".into(),
                tasks_total: 0,
                tasks_failed: 0,
                duration_ms: 1,
                spent_usd: 0.0,
            },
            Utc::now(),
        );
        router.dispatch(env).await.unwrap();

        // The retry loop runs three attempts with 100/300ms backoffs ⇒
        // ~1.3s total. Poll the counter so we don't depend on a fixed
        // sleep.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if router.failed_emits_total() >= 1 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("failed_emits_total never reached 1");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let log_path = tmp.path().join("notifications.jsonl");
        let body = tokio::fs::read_to_string(&log_path)
            .await
            .expect("notifications.jsonl must exist after emit failure");
        assert!(body.contains("\"kind\":\"notification_failed\""), "{body}");
        assert!(body.contains("\"sink_id\":\"test-fail\""), "{body}");
        assert!(body.contains("\"event_kind\":\"run_finished\""), "{body}");
        assert!(body.contains("synthetic emit failure"), "{body}");
    }

    /// #156 (L1) regression: env-var override of dedup cache size is
    /// honored by `NotificationRouter::new`. We can't observe the cache
    /// size directly, so we verify the constructor doesn't panic for an
    /// extreme value (which a misuse of NonZeroUsize might).
    #[test]
    fn router_new_honors_dedup_cache_size_env() {
        std::env::set_var(DEDUP_CACHE_SIZE_ENV, "1024");
        let _ = NotificationRouter::new(vec![]);
        std::env::set_var(DEDUP_CACHE_SIZE_ENV, "0"); // should fall back to default
        let _ = NotificationRouter::new(vec![]);
        std::env::set_var(DEDUP_CACHE_SIZE_ENV, "not a number");
        let _ = NotificationRouter::new(vec![]);
        std::env::remove_var(DEDUP_CACHE_SIZE_ENV);
    }

    /// #187 regression: a poisoned `dedup_cache` mutex must not panic the
    /// dispatch path. Earlier code used `.unwrap()`, so a panic in any
    /// code path holding the lock would propagate the `PoisonError` to
    /// every subsequent `dispatch` call and silently disable all
    /// notifications. The fix recovers the inner cache (worst case: one
    /// duplicate emit) and keeps going.
    #[tokio::test]
    async fn dispatch_recovers_from_poisoned_dedup_mutex() {
        use std::sync::Arc;
        let router = Arc::new(NotificationRouter::new(vec![]));

        // Poison the mutex by panicking while holding the lock. We have
        // to launch a thread because std::panic in the same task would
        // unwind into the test runner.
        let r = Arc::clone(&router);
        let _ = std::thread::spawn(move || {
            let _g = r.dedup_cache.lock().unwrap();
            panic!("synthetic poison");
        })
        .join();
        assert!(
            router.dedup_cache.is_poisoned(),
            "test setup: mutex must be poisoned",
        );

        // Dispatch must still succeed against a poisoned mutex.
        let env = NotificationEnvelope::new(
            "r1",
            Severity::Info,
            PitbossEvent::RunFinished {
                run_id: "r1".to_string(),
                tasks_total: 0,
                tasks_failed: 0,
                duration_ms: 0,
                spent_usd: 0.0,
            },
            chrono::Utc::now(),
        );
        router
            .dispatch(env)
            .await
            .expect("dispatch must not panic on poisoned dedup mutex");
    }
}
