//! Minimal line-JSON client for the pitboss control socket. Mirrors the shape
//! of `fake-mcp-client`: connect, handshake, send ops, read events. Used by
//! `crates/pitboss-cli/tests/control_flows.rs`.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use pitboss_cli::control::protocol::{ClientMode, ControlEvent, ControlOp, EventEnvelope};

pub struct FakeControlClient {
    writer: OwnedWriteHalf,
    reader: BufReader<OwnedReadHalf>,
}

impl FakeControlClient {
    /// Connect to `socket` as a writer client, send `hello`, read the
    /// server hello, return a ready client.
    pub async fn connect(socket: &Path, client_version: &str) -> Result<Self> {
        Self::connect_with_mode(socket, client_version, ClientMode::Writer).await
    }

    /// PR-P of #438: connect as a subscriber-mode client. The dispatcher
    /// skips the writer-slot install + approval-drain / bridge-replay
    /// and rejects writer ops with `OpFailed`. Tests use this to assert
    /// the subscriber path without displacing a concurrent writer.
    pub async fn connect_subscriber(socket: &Path, client_version: &str) -> Result<Self> {
        Self::connect_with_mode(socket, client_version, ClientMode::Subscriber).await
    }

    async fn connect_with_mode(
        socket: &Path,
        client_version: &str,
        mode: ClientMode,
    ) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connect {}", socket.display()))?;
        let (r, w) = stream.into_split();
        let mut c = Self {
            writer: w,
            reader: BufReader::new(r),
        };
        c.send(&ControlOp::Hello {
            client_version: client_version.into(),
            mode,
        })
        .await?;
        // Wait for the server hello so the connection is fully established.
        let hello = c.recv().await?;
        if !matches!(hello, ControlEvent::Hello { .. }) {
            anyhow::bail!("expected Hello from control server, got {:?}", hello);
        }
        Ok(c)
    }

    /// Send a single op as one line of JSON.
    pub async fn send(&mut self, op: &ControlOp) -> Result<()> {
        let mut line = serde_json::to_string(op)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Read one event (blocks indefinitely).
    pub async fn recv(&mut self) -> Result<ControlEvent> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("control socket EOF");
        }
        Ok(serde_json::from_str(line.trim_end_matches('\n'))?)
    }

    /// Read events with a deadline; `None` on timeout.
    pub async fn recv_timeout(&mut self, d: Duration) -> Result<Option<ControlEvent>> {
        match tokio::time::timeout(d, self.recv()).await {
            Ok(ev) => Ok(Some(ev?)),
            Err(_) => Ok(None),
        }
    }

    /// Read one full envelope (with `actor_path` and `seq` fields). Use
    /// when a test needs to inspect the envelope wrapper; otherwise
    /// `recv` is the lighter-weight path that only decodes the inner
    /// `ControlEvent`. PR-B of #438.
    pub async fn recv_envelope(&mut self) -> Result<EventEnvelope> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("control socket EOF");
        }
        Ok(serde_json::from_str(line.trim_end_matches('\n'))?)
    }

    pub async fn recv_envelope_timeout(&mut self, d: Duration) -> Result<Option<EventEnvelope>> {
        match tokio::time::timeout(d, self.recv_envelope()).await {
            Ok(env) => Ok(Some(env?)),
            Err(_) => Ok(None),
        }
    }
}
