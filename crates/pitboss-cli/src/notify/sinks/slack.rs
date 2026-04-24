use crate::notify::{NotificationEnvelope, NotificationSink};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Emits notifications via Slack webhook.
pub struct SlackSink {
    id: String,
    url: String,
    http: Arc<reqwest::Client>,
}

impl SlackSink {
    pub fn new(idx: usize, url: String, http: Arc<reqwest::Client>) -> Self {
        let id = if idx == 0 {
            "slack".to_string()
        } else {
            format!("slack:{}", idx)
        };
        Self { id, url, http }
    }
}

#[async_trait]
impl NotificationSink for SlackSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, env: &NotificationEnvelope) -> Result<()> {
        crate::notify::config::pre_request_ssrf_check(&self.url).await?;

        let response = self
            .http
            .post(&self.url)
            .json(env)
            .timeout(Duration::from_secs(30))
            .send()
            .await?;

        match response.status() {
            status if status.is_success() => Ok(()),
            status if status.is_client_error() => {
                Err(response.error_for_status().unwrap_err().into())
            }
            _ => Err(anyhow::anyhow!(
                "slack POST failed with status {}",
                response.status()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{NotificationEnvelope, PitbossEvent, Severity};
    use chrono::Utc;
    use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn slack_sink_posts_valid_body_on_approval_request() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        // Will fail: SlackSink doesn't exist yet
        let sink = super::SlackSink::new(0, url, std::sync::Arc::new(reqwest::Client::new()));

        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::ApprovalRequest {
                request_id: "req-123".into(),
                task_id: "worker-1".into(),
                summary: "spawn 3 workers".into(),
            },
            Utc::now(),
        );

        let result = sink.emit(&env).await;
        assert!(result.is_ok());
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn slack_sink_returns_error_on_4xx() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&mock_server)
            .await;

        let sink = super::SlackSink::new(0, url, std::sync::Arc::new(reqwest::Client::new()));

        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::RunFinished {
                run_id: "run-1".into(),
                tasks_total: 1,
                tasks_failed: 0,
                duration_ms: 1000,
                spent_usd: 0.01,
            },
            Utc::now(),
        );

        let result = sink.emit(&env).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("400"));
    }

    #[tokio::test]
    async fn slack_sink_returns_error_on_5xx() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&mock_server)
            .await;

        let sink = super::SlackSink::new(0, url, std::sync::Arc::new(reqwest::Client::new()));

        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Critical,
            PitbossEvent::BudgetExceeded {
                run_id: "run-1".into(),
                spent_usd: 2.0,
                budget_usd: 1.0,
            },
            Utc::now(),
        );

        let result = sink.emit(&env).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }
}
