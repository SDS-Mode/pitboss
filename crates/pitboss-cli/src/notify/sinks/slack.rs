use crate::notify::{NotificationEnvelope, NotificationSink, PitbossEvent, Severity};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Emits notifications via Slack incoming webhook using Block Kit layout.
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

    fn severity_emoji(sev: Severity) -> &'static str {
        match sev {
            Severity::Info => ":information_source:",
            Severity::Warning => ":warning:",
            Severity::Error => ":x:",
            Severity::Critical => ":rotating_light:",
        }
    }

    fn build_body(&self, env: &NotificationEnvelope) -> Value {
        let emoji = Self::severity_emoji(env.severity);
        let (header, detail) = match &env.event {
            PitbossEvent::ApprovalRequest {
                request_id,
                task_id,
                summary,
            } => {
                let header = format!("{} Pitboss approval requested", emoji);
                let detail = format!(
                    "*Request:* {}\n*Task:* {}\n*Summary:* {}",
                    escape_slack_mrkdwn(request_id),
                    escape_slack_mrkdwn(task_id),
                    escape_slack_mrkdwn(summary),
                );
                (header, detail)
            }
            PitbossEvent::ApprovalPending {
                request_id,
                task_id,
                summary,
            } => {
                let header = format!("{} Pitboss approval pending operator action", emoji);
                let detail = format!(
                    "*Request:* {}\n*Task:* {}\n*Summary:* {}",
                    escape_slack_mrkdwn(request_id),
                    escape_slack_mrkdwn(task_id),
                    escape_slack_mrkdwn(summary),
                );
                (header, detail)
            }
            PitbossEvent::RunDispatched {
                run_id,
                parent_run_id,
                manifest_path,
                mode,
                survive_parent,
            } => {
                let header = format!("{} Pitboss run dispatched", emoji);
                let parent_line = parent_run_id.as_deref().map_or(String::new(), |p| {
                    format!("\n*Parent:* {}", escape_slack_mrkdwn(p))
                });
                let survive_line = if *survive_parent {
                    "\n*Survives parent:* yes".to_string()
                } else {
                    String::new()
                };
                let detail = format!(
                    "*Run:* {}\n*Manifest:* {}\n*Mode:* {}{}{}",
                    escape_slack_mrkdwn(run_id),
                    escape_slack_mrkdwn(manifest_path),
                    escape_slack_mrkdwn(mode),
                    parent_line,
                    survive_line,
                );
                (header, detail)
            }
            PitbossEvent::RunFinished {
                run_id,
                tasks_total,
                tasks_failed,
                duration_ms,
                spent_usd,
            } => {
                let icon = if *tasks_failed > 0 {
                    ":warning:"
                } else {
                    ":white_check_mark:"
                };
                let header = format!("{} Pitboss run finished", icon);
                let duration_sec = duration_ms / 1000;
                let detail = format!(
                    "*Run:* {}\n*Tasks:* {} / {}\n*Duration:* {}s\n*Cost:* ${:.2}",
                    escape_slack_mrkdwn(run_id),
                    tasks_total.saturating_sub(*tasks_failed),
                    tasks_total,
                    duration_sec,
                    spent_usd,
                );
                (header, detail)
            }
            PitbossEvent::BudgetExceeded {
                run_id,
                spent_usd,
                budget_usd,
            } => {
                let header = format!("{} Pitboss budget exceeded", emoji);
                let detail = format!(
                    "*Run:* {}\n*Spent:* ${:.2}\n*Budget:* ${:.2}",
                    escape_slack_mrkdwn(run_id),
                    spent_usd,
                    budget_usd,
                );
                (header, detail)
            }
        };

        json!({
            "blocks": [
                {
                    "type": "header",
                    "text": { "type": "plain_text", "text": header, "emoji": true }
                },
                {
                    "type": "section",
                    "text": { "type": "mrkdwn", "text": detail }
                }
            ]
        })
    }
}

/// Escape characters that Slack mrkdwn interprets as formatting or mention
/// triggers in untrusted fields. Backslash-escapes `* _ ~ ` ` | > # [ ] ( ) @ < :`.
/// Newlines are preserved.
fn escape_slack_mrkdwn(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '(' | ')' | '@' | '<'
            | ':' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[async_trait]
impl NotificationSink for SlackSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, env: &NotificationEnvelope) -> Result<()> {
        crate::notify::config::pre_request_ssrf_check(&self.url).await?;

        let body = self.build_body(env);
        let response = self
            .http
            .post(&self.url)
            .json(&body)
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

    #[test]
    fn escape_neutralises_mrkdwn_injection() {
        let out = escape_slack_mrkdwn("@here see [evil](https://x) *bold* `code`");
        assert!(out.contains("\\@here"), "got: {out}");
        assert!(out.contains("\\["), "got: {out}");
        assert!(out.contains("\\]"), "got: {out}");
        assert!(out.contains("\\("), "got: {out}");
        assert!(out.contains("\\)"), "got: {out}");
        assert!(out.contains("\\*bold\\*"), "got: {out}");
        assert!(out.contains("\\`code\\`"), "got: {out}");
    }

    #[test]
    fn build_body_escapes_untrusted_fields() {
        let sink = SlackSink::new(
            0,
            "https://example.com".into(),
            Arc::new(reqwest::Client::new()),
        );
        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::ApprovalRequest {
                request_id: "req".into(),
                task_id: "worker-1".into(),
                summary: "@here run this *now*".into(),
            },
            Utc::now(),
        );
        let body = sink.build_body(&env);
        let detail = body["blocks"][1]["text"]["text"].as_str().unwrap();
        assert!(
            detail.contains("\\@here"),
            "untrusted @here must be escaped: {detail}"
        );
        assert!(
            detail.contains("\\*now\\*"),
            "untrusted *now* must be escaped: {detail}"
        );
    }

    #[tokio::test]
    async fn slack_sink_posts_valid_body_on_approval_request() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&mock_server)
            .await;

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
