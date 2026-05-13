//! TUI-side control-socket client.
//!
//! Wraps the writer half of a [`pitboss_cli::stream::RunStreamSession`]
//! so keypress handlers can issue control ops while the read half feeds
//! the unified stream consumed by the main event loop. Updated for
//! PR-D of #438 (TUI cutover to unified consumer).

#![allow(dead_code)]

use std::sync::Arc;

use anyhow::Result;
use pitboss_cli::control::protocol::ControlOp;
use pitboss_cli::stream::RunStreamWriter;

#[derive(Debug)]
pub struct ControlClient {
    writer: Arc<RunStreamWriter>,
}

impl ControlClient {
    /// Wrap a session writer. The writer is produced by
    /// [`pitboss_cli::stream::open_run_session`] when a live socket
    /// connection succeeds; otherwise no client is constructed and the
    /// TUI stays observe-only.
    #[must_use]
    pub fn new(writer: Arc<RunStreamWriter>) -> Self {
        Self { writer }
    }

    /// PR-D: the client exists iff a writer exists, so this is always
    /// true once constructed. Kept for back-compat with call sites
    /// that previously distinguished connect-failed from disconnect.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        true
    }

    /// Send a single control op as one JSON line. Returns Err if the
    /// socket has closed or the write fails.
    pub async fn send_op(&self, op: ControlOp) -> Result<()> {
        self.writer.send_op(&op).await
    }
}
