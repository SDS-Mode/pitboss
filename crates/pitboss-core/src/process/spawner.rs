use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitStatus;

use async_trait::async_trait;
use tokio::io::AsyncRead;

use crate::error::SpawnError;

/// Command to spawn. Pure data — no I/O.
#[derive(Debug, Clone)]
pub struct SpawnCmd {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
}

/// A running child process. Object-safe: callers hold `Box<dyn ChildProcess>`.
#[async_trait]
pub trait ChildProcess: Send {
    /// Take stdout (consuming — may only be called once).
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>>;
    /// Take stderr (consuming — may only be called once).
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>>;
    /// Non-blocking check: return `Ok(Some(status))` if the child has
    /// already exited, `Ok(None)` if it's still running.
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>>;
    /// Wait for the child to exit. Must be called exactly once.
    async fn wait(&mut self) -> std::io::Result<ExitStatus>;
    /// Send SIGTERM (best effort).
    fn terminate(&mut self) -> std::io::Result<()>;
    /// Send SIGKILL (best effort).
    fn kill(&mut self) -> std::io::Result<()>;
    /// OS-level process id, if available.
    fn pid(&self) -> Option<u32>;
}

/// Produces [`ChildProcess`] values from a [`SpawnCmd`].
#[async_trait]
pub trait ProcessSpawner: Send + Sync + 'static {
    async fn spawn(&self, cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError>;
}
