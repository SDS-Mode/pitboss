#![allow(clippy::must_use_candidate, clippy::return_self_not_must_use)]

use std::pin::Pin;
use std::process::ExitStatus;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{duplex, AsyncRead, AsyncWriteExt, DuplexStream};
use tokio::sync::oneshot;

use crate::error::SpawnError;

use super::spawner::{ChildProcess, ProcessSpawner, SpawnCmd};

#[derive(Debug, Clone)]
enum Action {
    StdoutLine(String),
    StderrLine(String),
    Sleep(Duration),
}

/// A script of events a [`FakeSpawner`]-produced child will play back.
#[derive(Debug, Clone, Default)]
pub struct FakeScript {
    actions: Vec<Action>,
    exit_code: i32,
    spawn_delay: Option<Duration>,
    fail_on_spawn: Option<String>,
    hold_until_signal: bool,
}

impl FakeScript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stdout_line<S: Into<String>>(mut self, s: S) -> Self {
        self.actions.push(Action::StdoutLine(s.into()));
        self
    }

    pub fn stderr_line<S: Into<String>>(mut self, s: S) -> Self {
        self.actions.push(Action::StderrLine(s.into()));
        self
    }

    pub fn sleep(mut self, d: Duration) -> Self {
        self.actions.push(Action::Sleep(d));
        self
    }

    pub fn exit_code(mut self, code: i32) -> Self {
        self.exit_code = code;
        self
    }

    pub fn fail_spawn<S: Into<String>>(mut self, reason: S) -> Self {
        self.fail_on_spawn = Some(reason.into());
        self
    }

    /// Child never exits on its own; must be terminated.
    pub fn hold_until_signal(mut self) -> Self {
        self.hold_until_signal = true;
        self
    }
}

#[derive(Clone)]
pub struct FakeSpawner {
    script: FakeScript,
}

impl FakeSpawner {
    pub fn new(script: FakeScript) -> Self {
        Self { script }
    }
}

struct FakeChild {
    stdout: Option<Pin<Box<DuplexStream>>>,
    stderr: Option<Pin<Box<DuplexStream>>>,
    exit_rx: Option<oneshot::Receiver<i32>>,
    kill_tx: Option<oneshot::Sender<()>>,
    pid: u32,
}

#[async_trait]
impl ChildProcess for FakeChild {
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.stdout.take().map(|s| s as _)
    }
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send + Unpin>>> {
        self.stderr.take().map(|s| s as _)
    }
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let code = if let Some(rx) = self.exit_rx.take() {
            rx.await.unwrap_or(-1)
        } else {
            -1
        };
        Ok(exit_status_from_code(code))
    }
    fn terminate(&mut self) -> std::io::Result<()> {
        if let Some(tx) = self.kill_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
    fn kill(&mut self) -> std::io::Result<()> {
        if let Some(tx) = self.kill_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
}

#[async_trait]
impl ProcessSpawner for FakeSpawner {
    async fn spawn(&self, _cmd: SpawnCmd) -> Result<Box<dyn ChildProcess>, SpawnError> {
        if let Some(delay) = self.script.spawn_delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(reason) = &self.script.fail_on_spawn {
            return Err(SpawnError::Rejected {
                reason: reason.clone(),
            });
        }
        let (mut stdout_w, stdout_r) = duplex(4096);
        let (mut stderr_w, stderr_r) = duplex(4096);
        let (exit_tx, exit_rx) = oneshot::channel();
        let (kill_tx, mut kill_rx) = oneshot::channel();

        let actions = self.script.actions.clone();
        let exit_code = self.script.exit_code;
        let hold = self.script.hold_until_signal;

        tokio::spawn(async move {
            for a in actions {
                match a {
                    Action::StdoutLine(s) => {
                        let _ = stdout_w.write_all(s.as_bytes()).await;
                        let _ = stdout_w.write_all(b"\n").await;
                    }
                    Action::StderrLine(s) => {
                        let _ = stderr_w.write_all(s.as_bytes()).await;
                        let _ = stderr_w.write_all(b"\n").await;
                    }
                    Action::Sleep(d) => tokio::time::sleep(d).await,
                }
            }
            drop(stdout_w);
            drop(stderr_w);
            if hold {
                let _ = (&mut kill_rx).await;
                let _ = exit_tx.send(143);
            } else {
                let _ = exit_tx.send(exit_code);
            }
        });

        Ok(Box::new(FakeChild {
            stdout: Some(Box::pin(stdout_r)),
            stderr: Some(Box::pin(stderr_r)),
            exit_rx: Some(exit_rx),
            kill_tx: Some(kill_tx),
            pid: 1,
        }))
    }
}

#[cfg(unix)]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn exit_status_from_code(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}

#[cfg(not(unix))]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn exit_status_from_code(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code as u32)
}
