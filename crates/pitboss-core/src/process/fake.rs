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

    /// Emit a stream-json `assistant` message line whose `content` carries
    /// one `text` block and whose `message.usage` matches `usage`. This is
    /// what triggers [`crate::parser::Event::AssistantUsage`] in the
    /// parser (#253), letting callers exercise the per-turn usage path.
    pub fn assistant_usage(mut self, text: &str, usage: crate::parser::TokenUsage) -> Self {
        let line = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{ "type": "text", "text": text }],
                "usage": {
                    "input_tokens": usage.input,
                    "output_tokens": usage.output,
                    "cache_read_input_tokens": usage.cache_read,
                    "cache_creation_input_tokens": usage.cache_creation,
                }
            }
        });
        self.actions.push(Action::StdoutLine(line.to_string()));
        self
    }

    /// Emit a terminal stream-json `result` line so callers can drive the
    /// budget-watch finalization path. `session_id` is required by real
    /// claude; pass any unique string for tests.
    pub fn result_event(mut self, session_id: &str, usage: crate::parser::TokenUsage) -> Self {
        let line = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "session_id": session_id,
            "result": "done",
            "usage": {
                "input_tokens": usage.input,
                "output_tokens": usage.output,
                "cache_read_input_tokens": usage.cache_read,
                "cache_creation_input_tokens": usage.cache_creation,
            }
        });
        self.actions.push(Action::StdoutLine(line.to_string()));
        self
    }

    /// Emit a stream-json `rate_limit_event` line. `resets_at` is a Unix
    /// epoch-seconds timestamp.
    pub fn rate_limit_event(
        mut self,
        status: &str,
        rate_limit_type: Option<&str>,
        resets_at: Option<u64>,
    ) -> Self {
        let mut info = serde_json::json!({ "status": status });
        if let Some(kind) = rate_limit_type {
            info["rateLimitType"] = serde_json::Value::String(kind.to_string());
        }
        if let Some(ts) = resets_at {
            info["resetsAt"] = serde_json::Value::from(ts);
        }
        let line = serde_json::json!({
            "type": "rate_limit_event",
            "rate_limit_info": info,
        });
        self.actions.push(Action::StdoutLine(line.to_string()));
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
    /// Populated if `try_wait` observed a ready exit code before `wait`
    /// was called, so `wait` still returns the correct status instead of
    /// the -1 fallback.
    cached_exit_code: Option<i32>,
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
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if let Some(code) = self.cached_exit_code {
            return Ok(Some(exit_status_from_code(code)));
        }
        if let Some(rx) = self.exit_rx.as_mut() {
            match rx.try_recv() {
                Ok(code) => {
                    self.cached_exit_code = Some(code);
                    self.exit_rx = None;
                    Ok(Some(exit_status_from_code(code)))
                }
                Err(oneshot::error::TryRecvError::Empty) => Ok(None),
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.exit_rx = None;
                    Ok(Some(exit_status_from_code(-1)))
                }
            }
        } else {
            Ok(None)
        }
    }
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(code) = self.cached_exit_code.take() {
            return Ok(exit_status_from_code(code));
        }
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
            cached_exit_code: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_line_all, Event, TokenUsage};

    fn emitted_lines(script: &FakeScript) -> Vec<String> {
        script
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::StdoutLine(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn assistant_usage_helper_emits_a_parseable_assistant_with_usage() {
        let usage = TokenUsage {
            input: 100,
            output: 200,
            cache_read: 50,
            cache_creation: 25,
        };
        let script = FakeScript::new().assistant_usage("hello", usage);
        let line = emitted_lines(&script).pop().expect("one line");
        let events = parse_line_all(line.as_bytes()).expect("parse");
        // Expect AssistantText + AssistantUsage in order.
        assert!(matches!(events[0], Event::AssistantText { .. }));
        match &events[1] {
            Event::AssistantUsage { usage: u } => assert_eq!(*u, usage),
            other => panic!("expected AssistantUsage, got {other:?}"),
        }
    }

    #[test]
    fn result_event_helper_emits_a_parseable_result_line() {
        let usage = TokenUsage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_creation: 4,
        };
        let script = FakeScript::new().result_event("sess_xyz", usage);
        let line = emitted_lines(&script).pop().expect("one line");
        let events = parse_line_all(line.as_bytes()).expect("parse");
        match &events[0] {
            Event::Result {
                session_id,
                usage: u,
                ..
            } => {
                assert_eq!(session_id, "sess_xyz");
                assert_eq!(*u, usage);
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_event_helper_emits_a_parseable_rate_limit_line() {
        let script =
            FakeScript::new().rate_limit_event("rejected", Some("five_hour"), Some(1_776_402_000));
        let line = emitted_lines(&script).pop().expect("one line");
        let events = parse_line_all(line.as_bytes()).expect("parse");
        match &events[0] {
            Event::RateLimit {
                status,
                rate_limit_type,
                resets_at,
            } => {
                assert_eq!(status, "rejected");
                assert_eq!(rate_limit_type.as_deref(), Some("five_hour"));
                assert_eq!(*resets_at, Some(1_776_402_000));
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }
}
