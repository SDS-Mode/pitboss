//! Concrete `NotificationSink` implementations.

#![allow(dead_code)]

pub mod log;

pub use log::LogSink;

// Other sinks wired up in later tasks:
// pub mod discord;
// pub mod slack;
// pub mod webhook;
// pub use discord::DiscordSink;
// pub use slack::SlackSink;
// pub use webhook::WebhookSink;

use std::sync::Arc;

use anyhow::Result;

use super::config::{NotificationConfig, SinkKind};
use super::NotificationSink;

/// Build a concrete sink from a config. Caller provides a shared
/// reqwest::Client so HTTP sinks reuse connections.
pub fn build(
    cfg: &NotificationConfig,
    idx: usize,
    _http: &Arc<reqwest::Client>,
) -> Result<Arc<dyn NotificationSink>> {
    match cfg.kind {
        SinkKind::Log => Ok(Arc::new(LogSink::new(idx))),
        SinkKind::Webhook => anyhow::bail!("WebhookSink not yet implemented"),
        SinkKind::Slack => anyhow::bail!("SlackSink not yet implemented"),
        SinkKind::Discord => anyhow::bail!("DiscordSink not yet implemented"),
    }
}
