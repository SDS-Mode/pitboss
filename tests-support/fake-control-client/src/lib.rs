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

use pitboss_cli::control::protocol::{ControlEvent, ControlOp};

pub struct FakeControlClient {
    writer: OwnedWriteHalf,
    reader: BufReader<OwnedReadHalf>,
}

impl FakeControlClient {
    /// Connect to `socket`, send `hello`, read the server hello, return a ready
    /// client.
    pub async fn connect(socket: &Path, client_version: &str) -> Result<Self> {
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
        })
        .await?;
        // Wait for the server hello so the connection is fully established.
        let _hello = c.recv().await?;
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
}
