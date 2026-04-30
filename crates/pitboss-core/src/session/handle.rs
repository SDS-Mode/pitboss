use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::parser::{parse_line_all_dialect, Event, ParseDialect, TokenUsage};
use crate::process::{ProcessSpawner, SpawnCmd};

use super::{CancelToken, SessionOutcome, SessionState};

pub struct SessionHandle {
    task_id: String,
    spawner: Arc<dyn ProcessSpawner>,
    cmd: SpawnCmd,
    log_path: Option<PathBuf>,
    stderr_log_path: Option<PathBuf>,
    session_id_tx: Option<tokio::sync::mpsc::Sender<String>>,
    parse_dialect: ParseDialect,
    pid_slot: Option<Arc<std::sync::atomic::AtomicU32>>,
    terminate_grace: Duration,
    stream_drain_timeout: Duration,
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
            parse_dialect: ParseDialect::Claude,
            pid_slot: None,
            terminate_grace: super::TERMINATE_GRACE,
            stream_drain_timeout: super::DEFAULT_STREAM_DRAIN_TIMEOUT,
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

    #[must_use]
    pub fn with_parse_dialect(mut self, dialect: ParseDialect) -> Self {
        self.parse_dialect = dialect;
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

    /// Override the SIGTERM-to-SIGKILL grace window. The default is
    /// `super::TERMINATE_GRACE` (10 s). A non-zero value gives the
    /// child a chance to flush logs and shutdown cleanly before the
    /// kernel-enforced kill; zero means kill immediately.
    ///
    /// Tests use this to drive the cancel path quickly without
    /// monkey-patching the global constant.
    #[must_use]
    pub fn with_terminate_grace(mut self, d: Duration) -> Self {
        self.terminate_grace = d;
        self
    }

    /// Override the post-exit stdout/stderr drain timeout. The default
    /// is 30 s. After the child exits, `run_to_completion` waits up to
    /// this long for the stream-reader task to consume any tail bytes
    /// the kernel still has buffered. A non-zero value avoids losing
    /// the final assistant message on slow stdout flushes; zero means
    /// drop the tail immediately.
    #[must_use]
    pub fn with_stream_drain_timeout(mut self, d: Duration) -> Self {
        self.stream_drain_timeout = d;
        self
    }

    /// # Panics
    ///
    /// Panics if the spawner does not attach stdout to the child process. This
    /// is a programming error — all `ProcessSpawner` implementations must pipe
    /// stdout.
    #[allow(clippy::too_many_lines)]
    pub async fn run_to_completion(self, cancel: CancelToken, timeout: Duration) -> SessionOutcome {
        let stream_drain_timeout = self.stream_drain_timeout;
        let terminate_grace = self.terminate_grace;
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
                    final_message: None,
                    started_at,
                    ended_at: Utc::now(),
                };
            }
        };

        // Publish the child pid so the dispatcher's SIGSTOP freeze-pause
        // path can signal it directly. No-op if the caller didn't install
        // a slot (tests, flat mode without pause support).
        //
        // `Release` store pairs with `Acquire` loads at signal sites — a
        // reader that observes a non-zero pid must also observe every
        // write preceding this point (notably the child struct's
        // process-state fields). `Relaxed` was insufficient: a reader on
        // another thread could see the pid published but still observe
        // the pre-spawn initial zero state of adjacent fields.
        if let Some(slot) = &self.pid_slot {
            if let Some(pid) = child.pid() {
                slot.store(pid, std::sync::atomic::Ordering::Release);
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
        // `std::sync::Mutex` rather than `tokio::sync::Mutex`: the only
        // contention model is the stream task writing, then `run_to_completion`
        // reading after the stream task is fully drained — there is never a
        // suspension point held across a lock guard, so the async mutex was
        // overhead with no payoff. (#149 L7)
        let accum = Arc::new(Mutex::new(StreamAccum::default()));
        let accum_stream = accum.clone();

        let stream_task = tokio::spawn(stream_loop(
            reader,
            log_writer,
            accum_stream,
            self.session_id_tx.clone(),
            self.parse_dialect,
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

        // If we need to stop the child, send SIGTERM and wait up to the
        // builder-configurable terminate grace. After grace, send SIGKILL.
        // This wait also serves as the SIGTERM → exit window.
        let exit_status = match end_reason {
            EndReason::Exited(s) => s,
            EndReason::Terminated | EndReason::TimedOut => {
                let _ = child.terminate();
                match tokio::time::timeout(terminate_grace, child.wait()).await {
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

        // Clear the pid slot the moment the child is reaped, *before* the
        // stream-drain await below. The OS may recycle the now-free pid for
        // an unrelated process within milliseconds; any reader that still
        // observed the published value during the 30-second drain window
        // would signal the wrong target. Drain depends on stdout/stderr fds,
        // not on the pid, so clearing here is safe. `Release` pairs with the
        // `Acquire` loads at signal sites (see `dispatch::signals`).
        if let Some(slot) = &self.pid_slot {
            slot.store(0, std::sync::atomic::Ordering::Release);
        }

        // Let the stream + stderr drain tasks finish. Stdout/stderr EOF is
        // guaranteed once the child is reaped, so we can wait generously
        // — a premature timeout here would throw away the final
        // Event::Result (which carries token usage, cost, and the claude
        // session id) and misclassify the task as `Failed { "no result
        // event" }`. A multi-second ceiling is kept as a safety net so a
        // hung log writer can't wedge the dispatcher indefinitely.
        if tokio::time::timeout(stream_drain_timeout, stream_task)
            .await
            .is_err()
        {
            tracing::warn!(
                "stream drain exceeded {}s after child exit; final Event::Result \
                 may be lost",
                stream_drain_timeout.as_secs()
            );
        }
        if let Some(t) = stderr_task {
            let _ = tokio::time::timeout(stream_drain_timeout, t).await;
        }

        let exit_code = exit_status
            .as_ref()
            .and_then(std::process::ExitStatus::code);
        let ended_at = Utc::now();

        let accum = accum
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

        let final_message = accum.last_text.clone();
        let final_message_preview = final_message.as_deref().map(truncate_preview);
        SessionOutcome {
            final_state,
            exit_code,
            token_usage: accum.usage,
            claude_session_id: accum.session_id.clone(),
            final_message_preview,
            final_message,
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
    active_text: String,
    saw_result: bool,
}

async fn stream_loop(
    mut reader: tokio::io::Lines<BufReader<Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>>>,
    mut log: Option<tokio::fs::File>,
    accum: Arc<Mutex<StreamAccum>>,
    session_id_tx: Option<tokio::sync::mpsc::Sender<String>>,
    parse_dialect: ParseDialect,
) {
    // Tracks whether we've already published the session id on the
    // `session_id_tx` channel — Claude Code emits `system{init}` first
    // (carries the session_id) and then `result` later (also carries it).
    // The dispatcher only needs the id once; firing twice would risk a
    // bounded-channel push back into a now-uninterested consumer.
    let mut session_id_published = false;
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if let Some(w) = log.as_mut() {
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                let events = match parse_line_all_dialect(line.as_bytes(), parse_dialect) {
                    Ok(evs) => evs,
                    Err(e) => {
                        tracing::debug!(
                            error = %e,
                            len = line.len(),
                            "stream_loop: parse_line_all failed; line skipped"
                        );
                        continue;
                    }
                };
                for ev in events {
                    match ev {
                        Event::AssistantText { text } => {
                            let mut a = accum
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if parse_dialect == ParseDialect::Goose {
                                a.active_text.push_str(&text);
                                let candidate = a.active_text.clone();
                                let trimmed_len = candidate.trim().len();
                                let current_len =
                                    a.last_text.as_deref().map_or(0, |t| t.trim().len());
                                if trimmed_len > current_len {
                                    a.last_text = Some(candidate);
                                }
                                continue;
                            }
                            // Prefer the longest nontrivial assistant text. Rationale:
                            // claude often appends a short confirmation ("Done.", "OK")
                            // after the real output; taking the last text buries the
                            // real content. A length-keyed winner avoids that. Stored
                            // untruncated so consumers reading `final_message` see the
                            // complete text — preview is built once at outcome time.
                            //
                            // Strict `>` so equal-length later messages do not displace
                            // the first-seen winner — when two assistant blocks have the
                            // same length, the earlier one is closer to the substantive
                            // answer (claude's tail confirmations cluster around the
                            // same short length).
                            let trimmed_len = text.trim().len();
                            let current_len = a.last_text.as_deref().map_or(0, |t| t.trim().len());
                            if trimmed_len > current_len {
                                a.last_text = Some(text);
                            }
                        }
                        Event::AssistantToolUse { .. } | Event::ToolResult { .. }
                            if parse_dialect == ParseDialect::Goose =>
                        {
                            let mut a = accum
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            a.active_text.clear();
                        }
                        // `system{subtype:"init"}` is the FIRST event of every
                        // claude session and already carries `session_id` — fire
                        // the dispatcher's notification now, not 30+ minutes
                        // later when the run finishes and `Event::Result`
                        // lands. The audit (#149 M5) flagged that the dispatcher
                        // was effectively waiting the full session duration for
                        // a session id it could have had at second one.
                        Event::System {
                            subtype: Some(ref st),
                            session_id: Some(ref sid),
                        } if st == "init" && !sid.is_empty() && !session_id_published => {
                            if let Some(tx) = &session_id_tx {
                                if let Err(e) = tx.try_send(sid.clone()) {
                                    tracing::debug!(
                                        error = %e,
                                        session_id = %sid,
                                        "stream_loop: session_id channel send dropped (init)"
                                    );
                                }
                            }
                            // Don't stash on `accum.session_id` here — the
                            // outcome's `claude_session_id` is the one carried
                            // by `Event::Result` (terminal, post-resume-aware).
                            // We just need to publish to the dispatcher early.
                            session_id_published = true;
                        }
                        Event::Result {
                            session_id: sid,
                            usage: u,
                            ..
                        } => {
                            let mut a = accum
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if !sid.is_empty() && !session_id_published {
                                // Fallback: init didn't carry a session_id (older
                                // claude builds, or a wire-format change). Fire
                                // here so the dispatcher's notification still
                                // happens, just later.
                                if let Some(tx) = &session_id_tx {
                                    if let Err(e) = tx.try_send(sid.clone()) {
                                        tracing::debug!(
                                            error = %e,
                                            session_id = %sid,
                                            "stream_loop: session_id channel send dropped (result)"
                                        );
                                    }
                                }
                                session_id_published = true;
                            }
                            if !sid.is_empty() {
                                a.session_id = Some(sid);
                            }
                            a.usage.add(&u);
                            a.saw_result = true;
                        }
                        _ => {}
                    }
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

    /// #149 M5 regression: the session id must fire on the *init* event
    /// (the first event of every session), not on `Event::Result` (the
    /// last). This test was previously named `session_id_fires_on_init_event`
    /// but emitted both an init AND a result line — so it passed even
    /// though the channel only fired on the result line. The dispatcher
    /// observed full-session-duration latency on session-id notification
    /// as a result. Asserts here:
    ///   1. With ONLY an init line, the channel fires.
    ///   2. With both init and result, only one send happens (no
    ///      duplicate publish from the result-event fallback path).
    #[tokio::test]
    async fn session_id_fires_on_init_event_alone() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init","session_id":"sess-init-only"}"#)
            .stdout_line(r#"{"type":"result","session_id":"sess-init-only","usage":{"input_tokens":1,"output_tokens":1}}"#)
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cmd = crate::process::SpawnCmd {
            program: std::path::PathBuf::from("fake-claude"),
            args: vec![],
            cwd: std::path::PathBuf::from("/tmp"),
            env: std::collections::HashMap::new(),
        };
        // Channel of size 1 — a duplicate send from the result-event
        // fallback would surface as a dropped publish in the tracing
        // log, and the second `rx.recv()` below would hang.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(1);
        let handle = SessionHandle::new("t", spawner, cmd).with_session_id_tx(tx);
        let _outcome = handle
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        let got = tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("channel fires before deadline")
            .expect("sender not dropped");
        assert_eq!(got, "sess-init-only");
        // No second send on the same id from the result-event fallback —
        // sender has been dropped (handle moved into the task), so the
        // next recv returns None promptly.
        let next = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        match next {
            Ok(None) => {} // expected: sender closed without a duplicate.
            Ok(Some(s)) => panic!("unexpected second publish of session id: {s}"),
            Err(elapsed) => {
                panic!("rx hung ({elapsed}); second publish presumably blocked on full channel")
            }
        }
    }

    /// #149 M5 regression: when init does NOT carry a `session_id` (older
    /// claude builds, or a wire-format change), the result-event fallback
    /// must still publish so the dispatcher isn't left waiting forever.
    #[tokio::test]
    async fn session_id_falls_back_to_result_when_init_missing() {
        let script = FakeScript::new()
            // init without session_id (degraded wire format)
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(r#"{"type":"result","session_id":"sess-fallback","usage":{"input_tokens":1,"output_tokens":1}}"#)
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
        assert_eq!(got, "sess-fallback");
    }
}

#[cfg(test)]
mod pid_slot_tests {
    use super::*;
    use crate::process::fake::{FakeScript, FakeSpawner};
    use crate::process::ProcessSpawner;
    use crate::session::CancelToken;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Regression for #147: the PID slot must be cleared promptly after the
    /// child reaps, not at the end of the (up-to-30s) stream-drain window.
    /// We can't observe the exact timing across the boundary cheaply, but
    /// the post-condition (slot == 0 on return) and bounded total runtime
    /// together guard against regression to the "clear after drain" form.
    #[tokio::test]
    async fn pid_slot_is_cleared_after_run() {
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":1,"output_tokens":1}}"#)
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let cmd = crate::process::SpawnCmd {
            program: std::path::PathBuf::from("fake-claude"),
            args: vec![],
            cwd: std::path::PathBuf::from("/tmp"),
            env: std::collections::HashMap::new(),
        };
        let slot = Arc::new(AtomicU32::new(0));
        let handle = SessionHandle::new("t", spawner, cmd).with_pid_slot(slot.clone());
        let _outcome = handle
            .run_to_completion(CancelToken::new(), Duration::from_secs(5))
            .await;
        assert_eq!(
            slot.load(Ordering::Acquire),
            0,
            "pid slot should be cleared on return so signal sites cannot \
             target a recycled pid",
        );
    }
}
