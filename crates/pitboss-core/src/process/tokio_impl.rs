use std::pin::Pin;
use std::process::{ExitStatus, Stdio};

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};

use crate::error::SpawnError;

use super::spawner::{ChildProcess, ProcessSpawner, SpawnCmd};

#[derive(Default, Clone)]
pub struct TokioSpawner;

impl TokioSpawner {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

struct TokioChild {
    inner: Child,
}

#[async_trait]
impl ChildProcess for TokioChild {
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.inner.stdout.take().map(|s| Box::pin(s) as _)
    }

    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.inner.stderr.take().map(|s| Box::pin(s) as _)
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.inner.try_wait()
    }

    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.inner.wait().await
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        signal_group(self.pid(), libc::SIGTERM)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        signal_group(self.pid(), libc::SIGKILL)
    }

    fn pid(&self) -> Option<u32> {
        self.inner.id()
    }
}

/// Send `sig` to the process group whose leader is `pid`. `pid` is also the
/// PGID because we spawn every child with `process_group(0)` (the child
/// becomes its own group leader, PGID == PID). Signaling `-pgid` reaches the
/// child plus every descendant that hasn't called `setsid()` to escape — which
/// covers the entire `claude` subtree (Bash subshells, sub-agents, MCP
/// servers).
///
/// Without this, SIGTERM/SIGKILL only hit the immediate `claude` process and
/// orphan its children to PID 1, where they keep holding worktree file
/// handles and consuming budget.
#[allow(unsafe_code)]
fn signal_group(maybe_pid: Option<u32>, sig: libc::c_int) -> std::io::Result<()> {
    let Some(group_leader) = maybe_pid else {
        return Ok(());
    };
    #[allow(clippy::cast_possible_wrap)]
    let pgid = group_leader as i32;
    // SAFETY: libc::kill with a negative pid sends to the process group;
    // pgid was returned by tokio::process::Child::id() for a child we
    // spawned with process_group(0), so it is a valid PGID we own.
    let rc = unsafe { libc::kill(-pgid, sig) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH = group already gone (every member exited). Treat as success
        // since the desired post-condition (no surviving processes) holds.
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(err);
    }
    Ok(())
}

#[async_trait]
impl ProcessSpawner for TokioSpawner {
    async fn spawn(&self, cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError> {
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .current_dir(&cmd.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(cmd.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));

        // Put the child in its own process group so that SIGTERM/SIGKILL
        // (sent via signal_group) reach the entire claude subtree, not just
        // the immediate child. process_group(0) makes the spawned process a
        // group leader whose PGID equals its PID — see signal_group() for
        // the matching teardown logic.
        #[cfg(unix)]
        command.process_group(0);

        let child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SpawnError::BinaryNotFound {
                    path: cmd.program.display().to_string(),
                }
            } else {
                SpawnError::Io(e)
            }
        })?;

        Ok(Box::new(TokioChild { inner: child }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    /// Spawn a child shell that backgrounds a long-running grandchild,
    /// terminate the parent, and confirm the grandchild was also killed.
    /// Without process-group signaling the grandchild would survive,
    /// re-parented to PID 1.
    #[cfg(unix)]
    #[allow(unsafe_code)]
    #[tokio::test]
    async fn terminate_kills_grandchildren() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let spawner = TokioSpawner::new();
        // sh -c 'sleep 60 & echo $! ; wait' — prints the grandchild's PID
        // on stdout, then waits forever (until the parent itself receives
        // SIGTERM). The parent process is the shell; the grandchild is
        // the backgrounded `sleep 60`.
        let cmd = SpawnCmd {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "sleep 60 & echo $! ; wait".into()],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        };
        let mut child = spawner.spawn(cmd).await.expect("spawn shell");

        // Read the grandchild PID off stdout.
        let stdout = child.take_stdout().expect("stdout piped");
        let mut lines = BufReader::new(stdout).lines();
        let pid_line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("read grandchild pid in time")
            .expect("io ok")
            .expect("got a line");
        let grandchild_pid: i32 = pid_line.trim().parse().expect("pid parses");

        // Terminate the parent — should also reach the grandchild.
        child.terminate().expect("terminate sends SIGTERM");
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("parent reaps in time");

        // Give the kernel a beat to deliver the signal and reap.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // SAFETY: kill(pid, 0) is a no-op probe; returns 0 if the process
        // exists, -1 with ESRCH if not.
        let alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
        assert!(
            !alive,
            "grandchild pid {grandchild_pid} survived parent SIGTERM — process group not wired"
        );
    }

    /// Sanity: terminate on an already-exited child should not return an
    /// error (ESRCH is collapsed to Ok).
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_after_exit_is_ok() {
        let spawner = TokioSpawner::new();
        let cmd = SpawnCmd {
            program: PathBuf::from("/bin/true"),
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: HashMap::new(),
        };
        let mut child = spawner.spawn(cmd).await.expect("spawn /bin/true");
        let _ = child.wait().await.expect("wait ok");
        // Process group is now empty; libc::kill(-pgid, SIGTERM) returns
        // ESRCH which signal_group() collapses.
        child.terminate().expect("terminate after exit is ok");
    }
}
