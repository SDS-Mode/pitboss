//! TUI-side client for the per-run control socket. Handles connect,
//! handshake, reader task that forwards events via `mpsc::Sender`, and a
//! `send_op` entry point for keypress handlers.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use pitboss_cli::control::protocol::{ControlEvent, ControlOp};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};

pub struct ControlClient {
    writer: Arc<Mutex<Option<OwnedWriteHalf>>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

impl ControlClient {
    /// Connect to `socket`, send hello, spawn a reader task that forwards
    /// events onto `events_tx`. Returns immediately even on connection failure
    /// — the client enters disconnected state and `send_op` no-ops with Err.
    pub async fn connect(socket: PathBuf, events_tx: mpsc::Sender<ControlEvent>) -> Result<Self> {
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer_holder: Arc<Mutex<Option<OwnedWriteHalf>>> = Arc::new(Mutex::new(None));

        if let Ok(stream) = UnixStream::connect(&socket).await {
            let (r, mut w) = stream.into_split();
            let hello = ControlOp::Hello {
                client_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            let mut line = serde_json::to_string(&hello)?;
            line.push('\n');
            w.write_all(line.as_bytes()).await.context("send hello")?;
            w.flush().await?;

            *writer_holder.lock().await = Some(w);
            connected.store(true, std::sync::atomic::Ordering::Relaxed);

            let events_tx_bg = events_tx.clone();
            let connected_bg = connected.clone();
            tokio::spawn(read_loop(r, events_tx_bg, connected_bg));
        }
        // Socket doesn't exist (run already finished) or dispatcher not ready.
        // Remain disconnected; the TUI stays observe-only.

        Ok(Self {
            writer: writer_holder,
            connected,
        })
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Send a single control op. Returns Err if disconnected or write fails.
    pub async fn send_op(&self, op: ControlOp) -> Result<()> {
        let mut guard = self.writer.lock().await;
        let Some(w) = guard.as_mut() else {
            anyhow::bail!("control client is disconnected");
        };
        let mut line = serde_json::to_string(&op)?;
        line.push('\n');
        w.write_all(line.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }
}

async fn read_loop(
    r: OwnedReadHalf,
    events_tx: mpsc::Sender<ControlEvent>,
    connected: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Ok(ev) = serde_json::from_str::<ControlEvent>(&line) {
            if events_tx.send(ev).await.is_err() {
                break;
            }
        }
    }
    connected.store(false, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn connect_to_nonexistent_socket_returns_disconnected_client() {
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("missing.sock");
        let (tx, _rx) = mpsc::channel(16);
        let client = ControlClient::connect(sock, tx).await.unwrap();
        assert!(!client.is_connected());
    }
}
