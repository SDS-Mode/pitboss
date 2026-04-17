//! Lifecycle of the shire MCP server (unix socket transport).

#![allow(dead_code)]

use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Compute the socket path for a given run. Falls back to the run_dir if
/// $XDG_RUNTIME_DIR is unset or non-writable.
pub fn socket_path_for_run(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("shire");
        if std::fs::create_dir_all(&p).is_ok() {
            return p.join(format!("{}.sock", run_id));
        }
    }
    // Fallback: alongside the run artifacts.
    let p = run_dir.join(run_id.to_string());
    let _ = std::fs::create_dir_all(&p);
    p.join("mcp.sock")
}

pub struct McpServer {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

impl McpServer {
    /// Start serving on the given socket path. Binds to the unix socket,
    /// spawns an accept loop in a dedicated tokio task, returns a handle.
    pub async fn start(socket_path: PathBuf) -> Result<Self> {
        // If the socket file already exists (stale), remove it.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        if let Ok((_stream, _addr)) = accept {
                            // Real tool dispatch comes in Task 10+.
                            // For now, dropping the stream is fine: the
                            // skeleton exists so we can test connect()
                            // succeeds without hanging.
                        }
                    }
                }
            }
        });

        Ok(Self {
            socket_path,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.join_handle.take() {
            h.abort();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use uuid::Uuid;

    // Serializes tests that mutate XDG_RUNTIME_DIR, since env vars are
    // process-global and cargo runs tests in parallel by default.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn socket_path_uses_xdg_runtime_dir_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, Path::new("/tmp"));
        assert!(p.starts_with(dir.path()));
        assert!(p.to_string_lossy().ends_with(".sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn socket_path_falls_back_to_run_dir_when_xdg_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XDG_RUNTIME_DIR");
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, dir.path());
        assert!(p.starts_with(dir.path()));
    }

    #[tokio::test]
    async fn server_starts_and_accepts_connection() {
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("test.sock");
        let server = McpServer::start(sock.clone()).await.unwrap();
        assert!(sock.exists(), "socket file should exist after start");
        assert_eq!(server.socket_path(), sock.as_path());

        // Connect a raw unix stream to verify the server is listening.
        let stream = tokio::net::UnixStream::connect(&sock).await;
        assert!(stream.is_ok(), "server should accept connections");

        drop(server);
        // Socket is cleaned up on drop.
    }
}
