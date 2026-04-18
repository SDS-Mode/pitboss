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

use super::config::{NotificationConfig, SinkKind};
use super::NotificationSink;

/// Build a concrete sink from a config. Caller provides a shared
/// reqwest::Client so HTTP sinks reuse connections.
pub fn build(
    cfg: &NotificationConfig,
    idx: usize,
    http: &Arc<reqwest::Client>,
) -> Result<Arc<dyn NotificationSink>> {
    match cfg.kind {
        SinkKind::Log => Ok(Arc::new(LogSink::new(idx))),
        SinkKind::Webhook => Ok(Arc::new(WebhookSink::new(
            idx,
            cfg.url.clone().expect("validated earlier"),
            Arc::clone(http),
        ))),
        SinkKind::Slack => Ok(Arc::new(SlackSink::new(
            idx,
            cfg.url.clone().expect("validated earlier"),
            Arc::clone(http),
        ))),
        SinkKind::Discord => Ok(Arc::new(DiscordSink::new(
            idx,
            cfg.url.clone().expect("validated earlier"),
            Arc::clone(http),
        ))),
    }
}
