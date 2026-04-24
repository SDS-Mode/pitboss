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

    #[allow(unsafe_code)]
    fn terminate(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            if let Some(pid) = self.pid() {
                // SAFETY: libc::kill is a well-defined POSIX call; pid comes from a
                // subprocess we spawned via tokio::process::Command, so it's a valid
                // process id for the current process group.
                #[allow(clippy::cast_possible_wrap)]
                let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                if rc != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.inner.start_kill()
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.inner.start_kill()
    }

    fn pid(&self) -> Option<u32> {
        self.inner.id()
    }
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
