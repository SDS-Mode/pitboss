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

/// Sockets older than this are considered orphaned and swept by
/// [`control_socket_path`] when allocating a new one. Pegged at 24 h —
/// long enough that any legitimately-running dispatcher would have
/// rebound the socket by now (the socket is removed on clean shutdown
/// via `ControlServerHandle::Drop`), short enough that crashed-run
/// detritus doesn't accumulate indefinitely in `$XDG_RUNTIME_DIR`. The
/// sweep is best-effort: stat / unlink failures are silently ignored
/// so a permission-denied or in-use socket never blocks startup.
/// (#152 L3)
const STALE_SOCKET_TTL_SECS: u64 = 24 * 60 * 60;

/// Compute the control socket path for `run_id`. Mirrors the MCP
/// `socket_path_for_run` convention: prefers `$XDG_RUNTIME_DIR/pitboss/` if it
/// exists and is writable, otherwise falls back to `<run_dir>/<run_id>/control.sock`.
///
/// Side effect: opportunistically removes stale `*.control.sock` files in
/// the chosen XDG directory whose mtime is older than
/// [`STALE_SOCKET_TTL_SECS`]. Pitboss removes its own socket on clean
/// shutdown (`ControlServerHandle::Drop`), so anything older than the
/// TTL came from a crashed run and would otherwise accumulate forever
/// under `$XDG_RUNTIME_DIR/pitboss/`.
pub fn control_socket_path(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("pitboss");
        if std::fs::create_dir_all(&p).is_ok() {
            sweep_stale_sockets(&p);
            return p.join(format!("{}.control.sock", run_id));
        }
    }
    let p = run_dir.join(run_id.to_string());
    let _ = std::fs::create_dir_all(&p);
    p.join("control.sock")
}

/// Walk `dir` and unlink any `*.control.sock` file with mtime older than
/// [`STALE_SOCKET_TTL_SECS`]. Best-effort: any I/O error during stat or
/// unlink is silently ignored — startup is preferred over completeness.
fn sweep_stale_sockets(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    let ttl = std::time::Duration::from_secs(STALE_SOCKET_TTL_SECS);
    for entry in read.flatten() {
        let path = entry.path();
        let is_control_sock = path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with(".control.sock"));
        if !is_control_sock {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let Ok(age) = now.duration_since(mtime) else {
            continue;
        };
        if age > ttl {
            let _ = std::fs::remove_file(&path);
        }
    }
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

    /// #152 L3 regression: stale `.control.sock` files older than the TTL
    /// must be unlinked when a new socket path is allocated. Files newer
    /// than the TTL and non-`.control.sock` files in the same dir must
    /// be left alone.
    #[test]
    fn sweep_unlinks_only_stale_control_sockets() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let xdg = TempDir::new().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", xdg.path());

        let pitboss_dir = xdg.path().join("pitboss");
        std::fs::create_dir_all(&pitboss_dir).unwrap();

        // Stale: old `.control.sock` — should be unlinked.
        let stale = pitboss_dir.join("01900000-0000-0000-0000-000000000000.control.sock");
        std::fs::write(&stale, b"").unwrap();
        // Backdate mtime past the TTL using `filetime`-equivalent shell
        // call — keep the test dependency-free by spawning `touch -t`.
        // The `-d` form is a GNU coreutils extension; on macOS use
        // `-A`. Skip if neither is available (CI is Linux).
        let touch = std::process::Command::new("touch")
            .args(["-d", "1970-01-02T00:00:00", stale.to_str().unwrap()])
            .status();
        if !touch.is_ok_and(|s| s.success()) {
            // Can't backdate — skip rather than false-positive.
            std::env::remove_var("XDG_RUNTIME_DIR");
            return;
        }

        // Fresh: just-created `.control.sock` — should survive.
        let fresh = pitboss_dir.join("01a00000-0000-0000-0000-000000000000.control.sock");
        std::fs::write(&fresh, b"").unwrap();

        // Unrelated file in the same dir — must be untouched even if old.
        let other = pitboss_dir.join("README");
        std::fs::write(&other, b"hi").unwrap();
        let _ = std::process::Command::new("touch")
            .args(["-d", "1970-01-02T00:00:00", other.to_str().unwrap()])
            .status();

        // Trigger the sweep by allocating a new socket path.
        let _ = control_socket_path(Uuid::now_v7(), xdg.path());

        std::env::remove_var("XDG_RUNTIME_DIR");

        assert!(!stale.exists(), "stale .control.sock should be swept");
        assert!(fresh.exists(), "fresh .control.sock must survive sweep");
        assert!(other.exists(), "non-.control.sock files must be untouched");
    }
}
