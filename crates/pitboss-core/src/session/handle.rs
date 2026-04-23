use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::parser::{parse_line, Event, TokenUsage};
use crate::process::{ProcessSpawner, SpawnCmd};

use super::{CancelToken, SessionOutcome, SessionState};

pub struct SessionHandle {
    task_id: String,
    spawner: Arc<dyn ProcessSpawner>,
    cmd: SpawnCmd,
    log_path: Option<PathBuf>,
    stderr_log_path: Option<PathBuf>,
    session_id_tx: Option<tokio::sync::mpsc::Sender<String>>,
    pid_slot: Option<Arc<std::sync::atomic::AtomicU32>>,
}

impl SessionHandle {
    pub fn new(
        task_id: impl Into<String>,
        spawner: Arc<dyn ProcessSpawner>,
        cmd: SpawnCmd,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            spawner,
            cmd,
            log_path: None,
            stderr_log_path: None,
            session_id_tx: None,
            pid_slot: None,
        }
    }

    #[must_use]
    pub fn with_log_path(mut self, p: PathBuf) -> Self {
        self.log_path = Some(p);
        self
    }

    #[must_use]
    pub fn with_stderr_log_path(mut self, p: PathBuf) -> Self {
        self.stderr_log_path = Some(p);
        self
    }

    #[must_use]
    pub fn with_session_id_tx(mut self, tx: tokio::sync::mpsc::Sender<String>) -> Self {
        self.session_id_tx = Some(tx);
        self
    }

    /// Provide a shared atomic slot that `run_to_completion` will populate
    /// with the child's OS pid immediately after spawn. Used by the
    /// dispatcher's SIGSTOP freeze-pause path — it needs the raw pid
    /// to send signals without going through the `ChildProcess`
    /// interface (which takes `&mut self`, and we've already moved the
    /// handle into the session task). Slot value of 0 means "not yet
    /// spawned"; any non-zero value is the real pid.
    #[must_use]
    pub fn with_pid_slot(mut self, slot: Arc<std::sync::atomic::AtomicU32>) -> Self {
        self.pid_slot = Some(slot);
        self
    }

    /// # Panics
    ///
    /// Panics if the spawner does not attach stdout to the child process. This
    /// is a programming error — all `ProcessSpawner` implementations must pipe
    /// stdout.
    #[allow(clippy::too_many_lines)]
    pub async fn run_to_completion(self, cancel: CancelToken, timeout: Duration) -> SessionOutcome {
        const STREAM_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
        let _ = self.task_id; // kept for future logging
        let started_at = Utc::now();

        let mut child = match self.spawner.spawn(self.cmd.clone()).await {
            Ok(c) => c,
            Err(e) => {
                return SessionOutcome {
                    final_state: SessionState::SpawnFailed {
                        message: e.to_string(),
                    },
                    exit_code: None,
                    token_usage: TokenUsage::default(),
                    claude_session_id: None,
                    final_message_preview: None,
                    started_at,
                    ended_at: Utc::now(),
                };
            }
        };

        // Publish the child pid so the dispatcher's SIGSTOP freeze-pause
        // path can signal it directly. No-op if the caller didn't install
        // a slot (tests, flat mode without pause support).
        if let Some(slot) = &self.pid_slot {
            if let Some(pid) = child.pid() {
                slot.store(pid, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let stdout = child.take_stdout().expect("stdout piped");
        let reader = BufReader::new(stdout).lines();

        let log_writer = if let Some(path) = &self.log_path {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .ok()
        } else {
            None
        };

        // Shared accumulator — stream_loop writes, we read post-session.
        let accum = Arc::new(Mutex::new(StreamAccum::default()));
        let accum_stream = accum.clone();

        let stream_task = tokio::spawn(stream_loop(
            reader,
            log_writer,
            accum_stream,
            self.session_id_tx.clone(),
        ));

        // Drain stderr into a separate log file if requested. Many subprocess errors
        // (including claude's "--verbose required" rejection) only surface on stderr.
        let stderr_task = if let Some(stderr) = child.take_stderr() {
            let stderr_log = if let Some(path) = &self.stderr_log_path {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .await
                    .ok()
            } else {
                None
            };
            Some(tokio::spawn(stderr_drain(
                BufReader::new(stderr).lines(),
                stderr_log,
            )))
        } else {
            None
        };

        // Shortcut: if the child already exited before we reached this
        // point (e.g. very fast subprocess, or a CancelToken that was
        // pre-terminated before the session even entered the select), take
        // that exit directly. Without this check the select below —
        // especially with `biased` — can classify a cleanly-finished child
        // as `Terminated`, discarding the exit code, token usage and
        // claude_session_id.
        let end_reason = if let Ok(Some(status)) = child.try_wait() {
            EndReason::Exited(Some(status))
        } else {
            let terminate_fut = cancel.await_terminate();
            tokio::pin!(terminate_fut);

            // Primary race: child exit, terminate signal, or overall timeout.
            // No `biased` — if both the child's exit and terminate are ready in
            // the same poll, fair selection avoids systematically preferring
            // cancellation over a clean completion.
            tokio::select! {
                () = &mut terminate_fut => EndReason::Terminated,
                () = tokio::time::sleep(timeout) => EndReason::TimedOut,
                status = child.wait() => EndReason::Exited(status.ok()),
            }
        };

        // If we need to stop the child, send SIGTERM and wait up to TERMINATE_GRACE.
        // After grace, send SIGKILL. This wait also serves as the SIGTERM → exit window.
        let exit_status = match end_reason {
            EndReason::Exited(s) => s,
            EndReason::Terminated | EndReason::TimedOut => {
                let _ = child.terminate();
                match tokio::time::timeout(super::TERMINATE_GRACE, child.wait()).await {
                    Ok(Ok(s)) => Some(s),
                    Ok(Err(_)) => None,
                    Err(_) => {
                        // Grace expired — force kill.
                        let _ = child.kill();
                        tokio::time::timeout(Duration::from_secs(1), child.wait())
                            .await
                            .ok()
                            .and_then(Result::ok)
                    }
                }
            }
        };

        // Let the stream + stderr drain tasks finish. Stdout/stderr EOF is
        // guaranteed once the child is reaped, so we can wait generously
        // — a premature timeout here would throw away the final
        // Event::Result (which carries token usage, cost, and the claude
        // session id) and misclassify the task as `Failed { "no result
        // event" }`. A multi-second ceiling is kept as a safety net so a
        // hung log writer can't wedge the dispatcher indefinitely.
        if tokio::time::timeout(STREAM_DRAIN_TIMEOUT, stream_task)
            .await
            .is_err()
        {
            tracing::warn!(
                "stream drain exceeded {}s after child exit; final Event::Result \
                 may be lost",
                STREAM_DRAIN_TIMEOUT.as_secs()
            );
        }
        if let Some(t) = stderr_task {
            let _ = tokio::time::timeout(STREAM_DRAIN_TIMEOUT, t).await;
        }

        let exit_code = exit_status
            .as_ref()
            .and_then(std::process::ExitStatus::code);
        let ended_at = Utc::now();

        let accum = accum.lock().await;
        let saw_result = accum.saw_result;

        let final_state = match &end_reason {
            EndReason::TimedOut => SessionState::TimedOut,
            EndReason::Terminated => SessionState::Cancelled,
            EndReason::Exited(_) => match exit_code {
                Some(0) if saw_result => SessionState::Completed,
                Some(c) if c != 0 => SessionState::Failed {
                    message: format!("exit code {c}"),
                },
                Some(_) => SessionState::Failed {
                    message: "no result event".into(),
                },
                None => SessionState::Failed {
                    message: "child did not exit cleanly".into(),
                },
            },
        };

        SessionOutcome {
            final_state,
            exit_code,
            token_usage: accum.usage,
            claude_session_id: accum.session_id.clone(),
            final_message_preview: accum.last_text.clone(),
            started_at,
            ended_at,
        }
    }
}

enum EndReason {
    Terminated,
    TimedOut,
    Exited(Option<std::process::ExitStatus>),
}

#[derive(Default)]
struct StreamAccum {
    usage: TokenUsage,
    session_id: Option<String>,
    last_text: Option<String>,
    saw_result: bool,
}

async fn stream_loop(
    mut reader: tokio::io::Lines<BufReader<Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>>>,
    mut log: Option<tokio::fs::File>,
    accum: Arc<Mutex<StreamAccum>>,
    session_id_tx: Option<tokio::sync::mpsc::Sender<String>>,
) {
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if let Some(w) = log.as_mut() {
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                match parse_line(line.as_bytes()) {
                    Ok(Event::AssistantText { text }) => {
                        let mut a = accum.lock().await;
                        // Prefer the longest nontrivial assistant text as the preview.
                        // Rationale: claude often appends a short confirmation
                        // ("Done.", "OK") after the real output; taking the last text
                        // buries the real content. A length-keyed winner avoids that.
                        let trimmed_len = text.trim().len();
                        let current_len = a
                            .last_text
                            .as_deref()
                            .map_or(0, |t| t.trim_end_matches('…').trim().len());
                        if trimmed_len >= current_len {
                            a.last_text = Some(truncate_preview(&text));
                        }
                    }
                    Ok(Event::Result {
                        session_id: sid,
                        usage: u,
                        ..
                    }) => {
                        let mut a = accum.lock().await;
                        if let Some(tx) = &session_id_tx {
                            // Best-effort: if receiver is closed or full, drop the send.
                            let _ = tx.try_send(sid.clone());
                        }
                        a.session_id = Some(sid);
                        a.usage.add(&u);
                        a.saw_result = true;
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

async fn stderr_drain(
    mut reader: tokio::io::Lines<BufReader<Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>>>,
    mut log: Option<tokio::fs::File>,
) {
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if let Some(w) = log.as_mut() {
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

fn truncate_preview(s: &str) -> String {
    // Truncate by character (not byte) to avoid panicking on multi-byte
    // boundaries — claude's output routinely contains emoji.
    const MAX_CHARS: usize = 200;
    let mut chars = s.chars();
    let prefix: String = (&mut chars).take(MAX_CHARS).collect();
    if chars.next().is_some() {
        let mut out = prefix;
        out.push('…');
        out
    } else {
        prefix
    }
}

#[cfg(test)]
mod preview_tests {
    use super::truncate_preview;

    #[test]
    fn short_ascii_passes_through() {
        assert_eq!(truncate_preview("hello"), "hello");
    }

    #[test]
    fn long_ascii_truncates_with_ellipsis() {
        let s = "a".repeat(250);
        let out = truncate_preview(&s);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 201); // 200 chars + ellipsis
    }

    #[test]
    fn emoji_at_boundary_does_not_panic() {
        // 198 chars of 'a', then 4 emoji (each 4 bytes), then more content —
        // byte index 200 lands in the middle of an emoji. Old impl panicked.
        let mut s = "a".repeat(198);
        s.push_str("🦀🦀🦀🦀 tail");
        let out = truncate_preview(&s);
        assert!(out.ends_with('…'));
        // First 200 chars, then ellipsis.
        assert_eq!(out.chars().count(), 201);
    }

    #[test]
    fn exactly_max_chars_no_ellipsis() {
        let s = "🦀".repeat(200);
        let out = truncate_preview(&s);
        assert!(!out.ends_with('…'));
        assert_eq!(out.chars().count(), 200);
    }
}

#[cfg(test)]
mod session_id_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::ProcessSpawner;
    use crate::session::CancelToken;
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn session_id_fires_on_init_event() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init","session_id":"sess-xyz"}"#)
            .stdout_line(r#"{"type":"result","session_id":"sess-xyz","usage":{"input_tokens":1,"output_tokens":1}}"#)
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cmd = crate::process::SpawnCmd {
            program: std::path::PathBuf::from("fake-claude"),
            args: vec![],
            cwd: std::path::PathBuf::from("/tmp"),
            env: std::collections::HashMap::new(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
        let handle = SessionHandle::new("t", spawner, cmd).with_session_id_tx(tx);
        let _outcome = handle
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        let got = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("channel fires before deadline")
            .expect("sender not dropped");
        assert_eq!(got, "sess-xyz");
    }
}
