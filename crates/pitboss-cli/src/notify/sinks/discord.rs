#[cfg(test)]
use crate::notify::config::resolve_request_timeout;
use crate::notify::config::{build_pinned_client, pre_request_ssrf_check};
use crate::notify::{NotificationEnvelope, NotificationSink, PitbossEvent, Severity};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Emits notifications via Discord webhook.
pub struct DiscordSink {
    id: String,
    url: String,
    http: Arc<reqwest::Client>,
    request_timeout: Duration,
    bypass_ssrf: bool,
}

impl DiscordSink {
    pub fn new(
        idx: usize,
        url: String,
        http: Arc<reqwest::Client>,
        request_timeout: Duration,
    ) -> Self {
        let id = if idx == 0 {
            "discord".to_string()
        } else {
            format!("discord:{}", idx)
        };
        Self {
            id,
            url,
            http,
            request_timeout,
            bypass_ssrf: false,
        }
    }

    /// Test-only constructor that skips the per-request SSRF guard so a
    /// `wiremock::MockServer` (which always binds 127.0.0.1) can be used
    /// as the destination. Production paths must use [`Self::new`].
    #[cfg(test)]
    fn new_unchecked(url: String, http: Arc<reqwest::Client>) -> Self {
        Self {
            id: "discord".to_string(),
            url,
            http,
            request_timeout: resolve_request_timeout(None),
            bypass_ssrf: true,
        }
    }

    fn color(sev: Severity) -> u32 {
        match sev {
            Severity::Info => 0x3498db,     // blue
            Severity::Warning => 0xf1c40f,  // yellow
            Severity::Error => 0xe67e22,    // orange
            Severity::Critical => 0xe74c3c, // red
        }
    }

    fn build_body(&self, env: &NotificationEnvelope) -> Value {
        let (title, description) = match &env.event {
            PitbossEvent::ApprovalRequest {
                request_id,
                task_id,
                summary,
            } => {
                let desc = format!(
                    "**Request:** {}\n**Task:** {}\n**Summary:** {}",
                    escape_discord_md(request_id),
                    escape_discord_md(task_id),
                    escape_discord_md(summary),
                );
                ("🟡 Pitboss approval requested".to_string(), desc)
            }
            PitbossEvent::ApprovalPending {
                request_id,
                task_id,
                summary,
            } => {
                let desc = format!(
                    "**Request:** {}\n**Task:** {}\n**Summary:** {}",
                    escape_discord_md(request_id),
                    escape_discord_md(task_id),
                    escape_discord_md(summary),
                );
                (
                    "⏳ Pitboss approval pending operator action".to_string(),
                    desc,
                )
            }
            PitbossEvent::RunDispatched {
                run_id,
                parent_run_id,
                manifest_path,
                mode,
                survive_parent,
            } => {
                let parent_line = parent_run_id.as_deref().map_or(String::new(), |p| {
                    format!("\n**Parent:** {}", escape_discord_md(p))
                });
                let survive_line = if *survive_parent {
                    "\n**Survives parent:** yes".to_string()
                } else {
                    String::new()
                };
                let desc = format!(
                    "**Run:** {}\n**Manifest:** {}\n**Mode:** {}{}{}",
                    escape_discord_md(run_id),
                    escape_discord_md(manifest_path),
                    escape_discord_md(mode),
                    parent_line,
                    survive_line,
                );
                ("🚀 Pitboss run dispatched".to_string(), desc)
            }
            PitbossEvent::RunFinished {
                run_id,
                tasks_total,
                tasks_failed,
                duration_ms,
                spent_usd,
            } => {
                let icon = if *tasks_failed > 0 { "⚠️" } else { "✅" };
                let title = format!("{} Pitboss run finished", icon);
                let duration_sec = duration_ms / 1000;
                let desc = format!(
                    "**Run:** {}\n**Tasks:** {} / {}\n**Duration:** {}s\n**Cost:** ${:.2}",
                    escape_discord_md(run_id),
                    tasks_total.saturating_sub(*tasks_failed),
                    tasks_total,
                    duration_sec,
                    spent_usd
                );
                (title, desc)
            }
            PitbossEvent::BudgetExceeded {
                run_id,
                spent_usd,
                budget_usd,
            } => {
                let desc = format!(
                    "**Run:** {}\n**Spent:** ${:.2}\n**Budget:** ${:.2}",
                    escape_discord_md(run_id),
                    spent_usd,
                    budget_usd
                );
                ("🛑 Pitboss budget exceeded".to_string(), desc)
            }
        };

        json!({
            "content": null,
            // Defense-in-depth: even with mentions escaped in the description,
            // tell Discord not to resolve any @everyone / @here / user /
            // role mentions the payload might contain.
            "allowed_mentions": { "parse": [] },
            "embeds": [
                {
                    "title": title,
                    "description": description,
                    "color": Self::color(env.severity),
                    "timestamp": env.ts.to_rfc3339(),
                    "footer": {
                        "text": format!("Source: {}", escape_discord_md(&env.source))
                    }
                }
            ]
        })
    }
}

/// Escape characters that would otherwise be interpreted as Discord markdown
/// or as a mention trigger in untrusted fields. Backslash-escapes `* _ ~ \` ``
/// `| > # [ ]` and the mention sigils `@`, `<`, `:`. Newlines are preserved.
fn escape_discord_md(s: &str) -> String {
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
impl NotificationSink for DiscordSink {
    fn id(&self) -> &str {
        &self.id
    }

    async fn emit(&self, env: &NotificationEnvelope) -> Result<()> {
        let body = self.build_body(env);
        // Strip the URL from any reqwest error before it bubbles up — the
        // path carries the bot token (`/api/webhooks/<id>/<token>`) and
        // would otherwise land verbatim in tracing output. See
        // notify::config::redact_webhook_url for the formatted-string variant.
        let response = if self.bypass_ssrf {
            self.http
                .post(&self.url)
                .json(&body)
                .timeout(self.request_timeout)
                .send()
                .await
                .map_err(|e| e.without_url())?
        } else {
            // DNS-rebinding-resistant path; see WebhookSink::emit for the
            // identical pattern. Issue #156 (M2).
            let preflight = pre_request_ssrf_check(&self.url).await?;
            let client = build_pinned_client(&preflight, self.request_timeout)?;
            client
                .post(&self.url)
                .json(&body)
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
                "discord POST failed with status {}",
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
    fn escape_neutralises_markdown_and_mention_chars() {
        let out = escape_discord_md("@everyone see [evil](https://x) **bold** `code`");
        // @ / [ / ] / ( / ) / * / ` must all be backslash-escaped.
        assert!(out.contains("\\@everyone"), "got: {out}");
        assert!(out.contains("\\["), "got: {out}");
        assert!(out.contains("\\]"), "got: {out}");
        assert!(out.contains("\\("), "got: {out}");
        assert!(out.contains("\\)"), "got: {out}");
        assert!(out.contains("\\*\\*bold\\*\\*"), "got: {out}");
        assert!(out.contains("\\`code\\`"), "got: {out}");
    }

    #[test]
    fn build_body_sets_allowed_mentions_to_empty() {
        let sink = DiscordSink::new(
            0,
            "https://example.com".into(),
            Arc::new(reqwest::Client::new()),
            Duration::from_secs(30),
        );
        let env = NotificationEnvelope::new(
            "run-1",
            Severity::Warning,
            PitbossEvent::ApprovalRequest {
                request_id: "req".into(),
                task_id: "worker-1".into(),
                summary: "@everyone run this".into(),
            },
            Utc::now(),
        );
        let body = sink.build_body(&env);
        let parse = body
            .get("allowed_mentions")
            .and_then(|v| v.get("parse"))
            .and_then(|v| v.as_array())
            .expect("allowed_mentions.parse should be an array");
        assert!(
            parse.is_empty(),
            "parse array must be empty to disable all mentions"
        );

        let desc = body["embeds"][0]["description"].as_str().unwrap();
        assert!(
            desc.contains("\\@everyone"),
            "untrusted @everyone must be escaped: {desc}"
        );
    }

    #[tokio::test]
    async fn discord_sink_posts_valid_body() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&mock_server)
            .await;

        let sink = DiscordSink::new_unchecked(url, Arc::new(reqwest::Client::new()));

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
    async fn discord_sink_returns_error_on_4xx() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&mock_server)
            .await;

        let sink = DiscordSink::new_unchecked(url, Arc::new(reqwest::Client::new()));

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
    async fn discord_sink_returns_error_on_5xx() {
        let mock_server = MockServer::start().await;
        let url = mock_server.uri();

        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&mock_server)
            .await;

        let sink = DiscordSink::new_unchecked(url, Arc::new(reqwest::Client::new()));

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
