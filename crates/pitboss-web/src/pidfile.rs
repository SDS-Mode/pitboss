//! PID-file lifecycle for the web server. The `serve` path writes one
//! at startup; the `stop` subcommand reads it to send SIGTERM. Lives
//! in `$XDG_RUNTIME_DIR/pitboss/pitboss-web.pid` when that's available
//! (Linux with logind), otherwise falls back to `dirs::cache_dir()`,
//! then `/tmp` as a last resort.
//!
//! Stale-PID detection: if the file points at a process that's no longer
//! alive (or was reused by an unrelated PID), we treat the file as
//! orphaned and overwrite it. This is a best-effort check via `kill(0)`
//! — race-free against another live `pitboss-web` of the same user is
//! good enough; we don't need full cgroup-level identity verification.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Resolve the PID-file path. Pure — does not create the parent dir.
#[must_use]
pub fn pidfile_path() -> PathBuf {
    let dir = dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("pitboss").join("pitboss-web.pid")
}

/// Write the current process's PID to the canonical pidfile path.
/// Creates parent directories as needed. If a stale PID file is
/// already present, overwrites it; if a *live* PID is present,
/// returns `Err(io::ErrorKind::AlreadyExists)` so the caller can
/// surface a friendly "another instance is running" message instead
/// of clobbering it.
pub fn write_self() -> io::Result<PathBuf> {
    let path = pidfile_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(existing) = read_pid(&path) {
        if pid_alive(existing) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "pitboss-web already running with pid {existing} (pidfile: {})",
                    path.display()
                ),
            ));
        }
    }
    let pid = std::process::id();
    fs::write(&path, format!("{pid}\n"))?;
    Ok(path)
}

/// Remove the pidfile if it exists and points at us. No-op when the
/// file is gone. Logs (not errors) on mismatch to avoid spurious
/// failures during shutdown.
pub fn remove_if_self() {
    let path = pidfile_path();
    let Ok(existing) = read_pid(&path) else {
        return;
    };
    if existing == std::process::id() {
        let _ = fs::remove_file(&path);
    } else {
        tracing::warn!(
            existing,
            our_pid = std::process::id(),
            "pidfile points at another process; leaving it alone"
        );
    }
}

/// Read the PID from a file. Trims surrounding whitespace; returns
/// `InvalidData` on parse failure.
pub fn read_pid(path: &Path) -> io::Result<u32> {
    let s = fs::read_to_string(path)?;
    s.trim()
        .parse::<u32>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// Best-effort liveness check via `kill(pid, 0)`. Returns `false` for
/// PID 0 (would broadcast to the whole process group), and for any
/// non-Unix target.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) is a pure liveness check; no signal is
    // delivered. The pid is converted to i32; on platforms where
    // pid_t is wider this is still fine because process IDs fit
    // comfortably in u32.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to
    // signal it — still alive from our perspective.
    let err = io::Error::last_os_error();
    err.raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

/// Send SIGTERM to a PID. Returns `Ok(())` on successful signal,
/// `Err` with the OS error on failure.
#[cfg(unix)]
pub fn send_sigterm(pid: u32) -> io::Result<()> {
    // SAFETY: kill is safe for any pid; the kernel rejects invalid
    // ones with ESRCH. We translate the C-style return into a Rust
    // io::Error.
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
pub fn send_sigterm(_pid: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "pitboss-web stop is only implemented on Unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn read_pid_parses_trailing_newline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("p");
        fs::write(&path, "12345\n").unwrap();
        assert_eq!(read_pid(&path).unwrap(), 12345);
    }

    #[test]
    fn read_pid_rejects_garbage() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("p");
        fs::write(&path, "not-a-pid").unwrap();
        let err = read_pid(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_reports_self() {
        assert!(pid_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn pid_alive_rejects_pid_zero() {
        assert!(!pid_alive(0));
    }

    /// PID 1 (init / systemd) almost always exists on a real Linux box.
    /// We expect EPERM, which the helper translates to "alive" — covers
    /// the EPERM branch that is otherwise unreachable from a unit test.
    #[cfg(target_os = "linux")]
    #[test]
    fn pid_alive_reports_pid_one_via_eperm_branch() {
        // If the test harness happens to run as root this still passes
        // (kill(1, 0) returns 0 instead of EPERM); both paths classify
        // pid 1 as alive.
        assert!(pid_alive(1));
    }
}
