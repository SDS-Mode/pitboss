use crate::notify::config::{build_pinned_client, pre_request_ssrf_check, resolve_request_timeout};
use crate::notify::{NotificationEnvelope, NotificationSink};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Emits notifications via HTTP POST to a webhook URL.
pub struct WebhookSink {
    id: String,
    url: String,
    /// Shared client used only on the SSRF-bypass path
    /// (`PITBOSS_PARENT_NOTIFY_URL`). The DNS-pinned path builds a fresh
    /// client per emit so [`reqwest::ClientBuilder::resolve_to_addrs`] can
    /// override DNS for the destination host — kept here to preserve the
    /// connection pool for trusted sinks.
    http: Arc<reqwest::Client>,
    request_timeout: Duration,
    /// Skip the per-request SSRF guard. Set only for sinks built from the
    /// `PITBOSS_PARENT_NOTIFY_URL` env var, since the canonical use case for
    /// that var is "POST to my local orchestrator on `http://localhost:N`"
    /// — which the manifest-author SSRF check would refuse. The env var is
    /// operator-trusted (a hostile manifest can't set the parent process's
    /// env), so loopback / private targets are safe by definition.
    bypass_ssrf: bool,
}

impl WebhookSink {
    pub fn new(
        idx: usize,
        url: String,
        http: Arc<reqwest::Client>,
        request_timeout: Duration,
    ) -> Self {
        let id = if idx == 0 {
            "webhook".to_string()
        } else {
            format!("webhook:{}", idx)
        };
        Self {
            id,
            url,
            http,
            request_timeout,
            bypass_ssrf: false,
        }
    }

    /// Like [`WebhookSink::new`] but tags the sink as operator-trusted so
    /// emit-time SSRF checks are skipped. Reserved for the
    /// `PITBOSS_PARENT_NOTIFY_URL` ingest path. Uses the
    /// [`crate::notify::config::DEFAULT_REQUEST_TIMEOUT_SECS`] timeout.
    pub fn new_trusted(id: String, url: String, http: Arc<reqwest::Client>) -> Self {
        Self {
            id,
            url,
            http,
            request_timeout: resolve_request_timeout(None),
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
        // `.without_url()` strips the request URL from the error chain
        // before it bubbles up: reqwest's Display impl includes the full
        // URL by default, which would leak Slack/Discord webhook tokens
        // (in path or query) into tracing output. See
        // notify::config::redact_webhook_url for the matching helper used
        // on string-formatted error messages.
        let response = if self.bypass_ssrf {
            self.http
                .post(&self.url)
                .json(env)
                .timeout(self.request_timeout)
                .send()
                .await
                .map_err(|e| e.without_url())?
        } else {
            // DNS-rebinding-resistant path: the SSRF check returns the exact
            // SocketAddrs it validated, and we build a one-shot client whose
            // DNS resolver is pinned to those addrs so reqwest's own internal
            // lookup at send() time can't be steered to a private IP between
            // the check and the POST. Issue #156 (M2).
            let preflight = pre_request_ssrf_check(&self.url).await?;
            let client = build_pinned_client(&preflight, self.request_timeout)?;
            client
                .post(&self.url)
                .json(env)
                .send()
                .await
                .map_err(|e| e.without_url())?
        };

        match response.status() {
            status if status.is_success() => Ok(()),
            status if status.is_client_error() => Err(response
                .error_for_status()
                .unwrap_err()
                .without_url()
                .into()),
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
        let sink = WebhookSink::new_trusted(
            "webhook-test".into(),
            format!("{}/notify", server_url),
            http,
        );

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
        let sink = WebhookSink::new_trusted(
            "webhook-test".into(),
            format!("{}/notify", server_url),
            http,
        );

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
        let sink = WebhookSink::new_trusted(
            "webhook-test".into(),
            format!("{}/notify", server_url),
            http,
        );

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
