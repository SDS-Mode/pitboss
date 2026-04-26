use crate::notify::{NotificationEnvelope, NotificationSink};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Emits notifications via HTTP POST to a webhook URL.
pub struct WebhookSink {
    id: String,
    url: String,
    http: Arc<reqwest::Client>,
    /// Skip the per-request SSRF guard. Set only for sinks built from the
    /// `PITBOSS_PARENT_NOTIFY_URL` env var, since the canonical use case for
    /// that var is "POST to my local orchestrator on `http://localhost:N`"
    /// — which the manifest-author SSRF check would refuse. The env var is
    /// operator-trusted (a hostile manifest can't set the parent process's
    /// env), so loopback / private targets are safe by definition.
    bypass_ssrf: bool,
}

impl WebhookSink {
    pub fn new(idx: usize, url: String, http: Arc<reqwest::Client>) -> Self {
        let id = if idx == 0 {
            "webhook".to_string()
        } else {
            format!("webhook:{}", idx)
        };
        Self {
            id,
            url,
            http,
            bypass_ssrf: false,
        }
    }

    /// Like [`WebhookSink::new`] but tags the sink as operator-trusted so
    /// emit-time SSRF checks are skipped. Reserved for the
    /// `PITBOSS_PARENT_NOTIFY_URL` ingest path.
    pub fn new_trusted(id: String, url: String, http: Arc<reqwest::Client>) -> Self {
        Self {
            id,
            url,
            http,
            bypass_ssrf: true,
        }
    }
}

#[async_trait]
impl NotificationSink for WebhookSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, env: &NotificationEnvelope) -> Result<()> {
        if !self.bypass_ssrf {
            // DNS rebinding / mutable-CNAME SSRF guard: re-validate the URL's
            // currently-resolved IP against the private-range blocklist
            // before sending.
            crate::notify::config::pre_request_ssrf_check(&self.url).await?;
        }

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
                "webhook POST failed with status {}",
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
    async fn webhook_sink_success_post() {
        let mock_server = MockServer::start().await;
        let server_url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/notify"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

        let http = Arc::new(reqwest::Client::new());
        let sink = WebhookSink::new(0, format!("{}/notify", server_url), http);

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

        assert!(sink.emit(&env).await.is_ok());
    }

    #[tokio::test]
    async fn webhook_sink_4xx_client_error() {
        let mock_server = MockServer::start().await;
        let server_url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/notify"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&mock_server)
            .await;

        let http = Arc::new(reqwest::Client::new());
        let sink = WebhookSink::new(0, format!("{}/notify", server_url), http);

        let env = NotificationEnvelope::new(
            "run-2",
            Severity::Error,
            PitbossEvent::RunFinished {
                run_id: "run-2".into(),
                tasks_total: 3,
                tasks_failed: 3,
                duration_ms: 15_000,
                spent_usd: 0.75,
            },
            Utc::now(),
        );

        assert!(sink.emit(&env).await.is_err());
    }

    #[tokio::test]
    async fn webhook_sink_5xx_server_error() {
        let mock_server = MockServer::start().await;
        let server_url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/notify"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let http = Arc::new(reqwest::Client::new());
        let sink = WebhookSink::new(0, format!("{}/notify", server_url), http);

        let env = NotificationEnvelope::new(
            "run-3",
            Severity::Critical,
            PitbossEvent::BudgetExceeded {
                run_id: "run-3".into(),
                spent_usd: 2.50,
                budget_usd: 2.00,
            },
            Utc::now(),
        );

        assert!(sink.emit(&env).await.is_err());
    }
}
