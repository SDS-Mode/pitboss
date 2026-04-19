use std::time::Duration;

use anyhow::{bail, Result};
use pitboss_core::session::CancelToken;

const SECOND_SIGINT_WINDOW: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------
// SIGSTOP / SIGCONT for freeze-pause
// ---------------------------------------------------------------------
//
// `pause_worker` supports two modes: the classic `cancel` (which
// terminates the claude subprocess and snapshots the session so
// `continue_worker` can re-spawn via `claude --resume`) and the newer
// `freeze` (which SIGSTOP's the process in place and SIGCONT's it
// back). Freeze preserves in-flight state (no token replay, no session
// re-init) but risks Anthropic dropping the HTTP session if the pause
// runs past their server-side idle window. Use cancel for long pauses,
// freeze for quick ones.
//
// We use raw libc::kill rather than the `nix` crate — libc is already a
// transitive dep and this is a two-function use case. Pitboss is
// Linux/macOS only so POSIX signal semantics are available
// unconditionally.

/// Suspend the process with `pid` using SIGSTOP. Returns an error for
/// `pid == 0` (slot not yet populated) or if the syscall fails
/// (process already exited, permission denied, etc).
pub fn freeze(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGSTOP, "SIGSTOP")
}

/// Resume a previously-frozen process with SIGCONT.
pub fn resume_stopped(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGCONT, "SIGCONT")
}

fn send_signal(pid: u32, sig: libc::c_int, name: &'static str) -> Result<()> {
    if pid == 0 {
        bail!("{name}: pid is 0 (worker not yet spawned?)");
    }
    // SAFETY: `kill(2)` accepts any pid_t + valid signo; no memory is
    // dereferenced. The cast is libc's expected pid_t shape.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    bail!("{name} to pid {pid} failed: {err}")
}

/// Spawn a task that watches for Ctrl-C in two phases:
///   1st SIGINT within window → drain
///   2nd SIGINT within window → terminate
/// After the window, re-armed: a single later SIGINT is treated as a fresh first.
pub fn install_ctrl_c_watcher(cancel: CancelToken) {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            cancel.drain();
            tracing::warn!("received Ctrl-C — draining; send another within 5s to terminate");
            match tokio::time::timeout(SECOND_SIGINT_WINDOW, tokio::signal::ctrl_c()).await {
                Ok(Ok(_)) => {
                    cancel.terminate();
                    tracing::warn!("received second Ctrl-C — terminating subprocesses");
                    return;
                }
                _ => {
                    tracing::info!("drain window expired; continuing in drain mode");
                    // Loop again: if another Ctrl-C arrives later, start a new window.
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_rejects_pid_zero() {
        let err = freeze(0).unwrap_err();
        assert!(err.to_string().contains("pid is 0"));
    }

    #[test]
    fn resume_rejects_pid_zero() {
        let err = resume_stopped(0).unwrap_err();
        assert!(err.to_string().contains("pid is 0"));
    }

    /// End-to-end: spawn a sleeping child, SIGSTOP it, confirm
    /// /proc reports stopped state, SIGCONT, confirm runnable,
    /// then clean up. Linux-only because /proc isn't available on
    /// macOS CI.
    ///
    /// Polls `/proc/<pid>/status` instead of a fixed sleep: the
    /// kernel can briefly report `D` (uninterruptible disk sleep)
    /// or `R` (running) between signal delivery and the process
    /// actually being descheduled, especially on slow CI runners.
    /// We only care that the state *eventually* reaches the
    /// expected one within a generous window.
    #[cfg(target_os = "linux")]
    #[test]
    fn freeze_then_resume_flips_proc_state() {
        use std::process::Command;

        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();

        freeze(pid).unwrap();
        assert!(
            wait_for_state(pid, &['T', 't'], Duration::from_secs(2)),
            "expected stopped state within 2s, final state: {:?}",
            read_proc_state(pid)
        );

        resume_stopped(pid).unwrap();
        assert!(
            wait_for_state(pid, &['S', 'R'], Duration::from_secs(2)),
            "expected sleeping/running state within 2s, final state: {:?}",
            read_proc_state(pid)
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    fn read_proc_state(pid: u32) -> Option<char> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("State:") {
                return rest.trim().chars().next();
            }
        }
        None
    }

    /// Poll `/proc/<pid>/status` every 10ms until the State field is
    /// one of `expected`, or `timeout` elapses. Returns true on match.
    #[cfg(target_os = "linux")]
    fn wait_for_state(pid: u32, expected: &[char], timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(c) = read_proc_state(pid) {
                if expected.contains(&c) {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
