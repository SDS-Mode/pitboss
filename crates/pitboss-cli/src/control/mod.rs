//! Per-run control socket: TUI ↔ dispatcher operator control plane.
//!
//! Split by file:
//! - `protocol` — serde types for line-based JSON messages.
//! - `server`   — unix-socket accept loop + per-connection op dispatch.
//!
//! See `docs/superpowers/specs/2026-04-17-pitboss-v0.4-live-control-design.md`
//! §4–§6 for the design.

pub mod protocol;
pub mod server;

use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Compute the control socket path for `run_id`. Mirrors the MCP
/// `socket_path_for_run` convention: prefers `$XDG_RUNTIME_DIR/pitboss/` if it
/// exists and is writable, otherwise falls back to `<run_dir>/<run_id>/control.sock`.
pub fn control_socket_path(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("pitboss");
        if std::fs::create_dir_all(&p).is_ok() {
            return p.join(format!("{}.control.sock", run_id));
        }
    }
    let p = run_dir.join(run_id.to_string());
    let _ = std::fs::create_dir_all(&p);
    p.join("control.sock")
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Serializes tests that mutate XDG_RUNTIME_DIR (env vars are process-global).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn uses_xdg_runtime_dir_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        let run_id = Uuid::now_v7();
        let p = control_socket_path(run_id, Path::new("/tmp"));
        assert!(p.starts_with(dir.path()));
        assert!(p.to_string_lossy().ends_with(".control.sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn falls_back_to_run_dir_when_xdg_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XDG_RUNTIME_DIR");
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let p = control_socket_path(run_id, dir.path());
        assert!(p.starts_with(dir.path()));
        assert_eq!(p.file_name().unwrap(), "control.sock");
    }
}
