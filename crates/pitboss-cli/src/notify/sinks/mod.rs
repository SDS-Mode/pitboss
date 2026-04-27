//! Concrete `NotificationSink` implementations.

#![allow(dead_code)]

pub mod discord;
pub mod log;
pub mod slack;
pub mod webhook;

pub use discord::DiscordSink;
pub use log::LogSink;
pub use slack::SlackSink;
pub use webhook::WebhookSink;

use std::sync::Arc;

use anyhow::Result;

use super::config::{resolve_request_timeout, NotificationConfig, SinkKind};
use super::NotificationSink;

/// Build a concrete sink from a config. Caller provides a shared
/// reqwest::Client; HTTP sinks use it on the SSRF-bypass path
/// (`PITBOSS_PARENT_NOTIFY_URL`) and build a one-shot DNS-pinned client
/// per emit on the manifest path — see [`super::config::build_pinned_client`].
///
/// Per-request timeout is taken from `cfg.request_timeout_secs` and falls
/// back to [`super::config::DEFAULT_REQUEST_TIMEOUT_SECS`].
pub fn build(
    cfg: &NotificationConfig,
    idx: usize,
    http: &Arc<reqwest::Client>,
) -> Result<Arc<dyn NotificationSink>> {
    let timeout = resolve_request_timeout(cfg.request_timeout_secs);
    match cfg.kind {
        SinkKind::Log => Ok(Arc::new(LogSink::new(idx))),
        SinkKind::Webhook => Ok(Arc::new(WebhookSink::new(
            idx,
            cfg.url.clone().expect("validated earlier"),
            Arc::clone(http),
            timeout,
        ))),
        SinkKind::Slack => Ok(Arc::new(SlackSink::new(
            idx,
            cfg.url.clone().expect("validated earlier"),
            Arc::clone(http),
            timeout,
        ))),
        SinkKind::Discord => Ok(Arc::new(DiscordSink::new(
            idx,
            cfg.url.clone().expect("validated earlier"),
            Arc::clone(http),
            timeout,
        ))),
    }
}
