//! Lifecycle of the shire MCP server (unix socket transport).

#![allow(dead_code)]

use anyhow::Result;
use std::path::{Path, PathBuf};
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
    // TODO: fields populated in Task 9
}

impl McpServer {
    /// Start serving on the given socket path. Returns a handle you can drop
    /// to shut down.
    pub async fn start(socket_path: PathBuf) -> Result<Self> {
        let _ = socket_path;
        unimplemented!("covered in Task 9")
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
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
}
