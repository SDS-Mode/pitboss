//! Stdio <-> unix-socket bridge. When the lead Hobbit launches
//! `pitboss mcp-bridge <socket>`, this process reads MCP-over-stdio from
//! claude and forwards it to the shire MCP server listening on the
//! unix socket.
//!
//! The Claude Code CLI's `--mcp-config` expects MCP servers described with a
//! `command` + `args` (stdio transport) or an SSE/HTTP URL. It does not
//! natively speak to unix sockets as a first-class transport. This bridge
//! closes that gap without requiring any changes to claude itself.

use std::path::Path;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Connect to the shire MCP server on `socket` and bidirectionally copy
/// bytes between claude's stdio pair and that unix socket. Returns when
/// either direction closes.
pub async fn run_bridge(socket: &Path) -> Result<i32> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect to shire mcp socket at {}", socket.display()))?;
    let (mut sr, mut sw) = stream.split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    // Bidirectional copy. If either direction errors (EOF or broken pipe), exit.
    let s2c = async {
        let mut buf = vec![0u8; 8192];
        loop {
            match sr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    if stdout.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    let c2s = async {
        let mut buf = vec![0u8; 8192];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if sw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    if sw.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };

    tokio::select! {
        _ = s2c => {}
        _ = c2s => {}
    }

    Ok(0)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    /// Sanity-check the unix-socket scaffolding the bridge relies on. We spin
    /// up a minimal echo server on a unix socket, connect to it directly, and
    /// verify the bytes round-trip. This does not exercise `run_bridge`
    /// itself (that requires hooking stdin/stdout, which is fiddly in a unit
    /// test) but it proves the transport primitives work as expected.
    #[tokio::test]
    async fn bridge_round_trip_echoes_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("echo.sock");

        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = vec![0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap();
                stream.write_all(&buf[..n]).await.unwrap();
                stream.flush().await.unwrap();
            }
        });

        // Connect directly (not through the bridge) to verify the echo works.
        let mut client = UnixStream::connect(&socket).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        client.flush().await.unwrap();
        let mut buf = [0u8; 5];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");

        // Ensure the server task completes so the test is fully deterministic.
        let _ = server.await;
    }
}
