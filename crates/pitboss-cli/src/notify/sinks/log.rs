use crate::notify::{NotificationEnvelope, NotificationSink};
use anyhow::Result;
use async_trait::async_trait;

/// Emits notifications to tracing logs as JSON.
pub struct LogSink {
    id: String,
}

impl LogSink {
    pub fn new(idx: usize) -> Self {
        let id = if idx == 0 {
            "log".to_string()
        } else {
            format!("log:{}", idx)
        };
        Self { id }
    }
}

#[async_trait]
impl NotificationSink for LogSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, env: &NotificationEnvelope) -> Result<()> {
        let json = serde_json::to_string(&env)?;
        tracing::info!(envelope = %json, "notification emitted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{NotificationEnvelope, PitbossEvent, Severity};
    use chrono::Utc;

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn log_sink_writes_tracing_line() {
        let sink = LogSink::new(0);
        assert_eq!(sink.id(), "log");

        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::RunFinished {
                run_id: "run-1".into(),
                tasks_total: 5,
                tasks_failed: 1,
                duration_ms: 30_000,
                spent_usd: 1.25,
            },
            Utc::now(),
        );

        sink.emit(&env).await.unwrap();

        assert!(logs_contain("notification emitted"));
    }
}
