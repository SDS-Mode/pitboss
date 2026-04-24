//! Notification sink system. See
//! docs/superpowers/specs/2026-04-17-pitboss-v041-notifications-design.md
//! for the full design.

#![allow(dead_code)] // Wired up gradually across Tasks 2-21.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod config;
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
            PitbossEvent::RunFinished { .. } => None,
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
use std::sync::{Arc, Mutex};

/// Router fans envelopes to multiple sinks with LRU dedup and retry.
pub struct NotificationRouter {
    sinks: Vec<(Arc<dyn NotificationSink>, SinkFilter)>,
    dedup_cache: Mutex<LruCache<String, ()>>,
}

impl NotificationRouter {
    /// Create a router with the given sinks and filters.
    pub fn new(sinks: Vec<(Arc<dyn NotificationSink>, SinkFilter)>) -> Self {
        Self {
            sinks,
            dedup_cache: Mutex::new(LruCache::new(NonZeroUsize::new(64).unwrap())),
        }
    }

    /// Dispatch envelope to all matching sinks. Deduplicates by dedup_key
    /// and spawns fire-and-forget tasks with retry.
    pub async fn dispatch(&self, env: NotificationEnvelope) -> Result<()> {
        {
            let mut cache = self.dedup_cache.lock().unwrap();
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
            tokio::spawn(async move {
                if let Err(e) = emit_with_retry(&sink, &env).await {
                    tracing::error!(
                        sink_id = %sink.id(),
                        dedup_key = %env.dedup_key,
                        error = %e,
                        "notification emit failed after retries"
                    );
                }
            });
        }
        Ok(())
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
            Err(e) if attempt < 2 => {
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
}
