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
        }
    }

    #[must_use]
    pub fn with_log_path(mut self, p: PathBuf) -> Self {
        self.log_path = Some(p);
        self
    }

    /// # Panics
    ///
    /// Panics if the spawner does not attach stdout to the child process. This
    /// is a programming error — all `ProcessSpawner` implementations must pipe
    /// stdout.
    pub async fn run_to_completion(self, cancel: CancelToken, timeout: Duration) -> SessionOutcome {
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

        let stream_task = tokio::spawn(stream_loop(reader, log_writer, accum_stream));

        let terminate_fut = cancel.await_terminate();
        tokio::pin!(terminate_fut);

        // Primary race: child exit, terminate signal, or overall timeout.
        let end_reason = tokio::select! {
            biased;
            () = &mut terminate_fut => EndReason::Terminated,
            () = tokio::time::sleep(timeout) => EndReason::TimedOut,
            status = child.wait() => EndReason::Exited(status.ok()),
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

        // Let stream_task wrap up briefly (it will see EOF when child's stdout closes).
        let _ = tokio::time::timeout(Duration::from_secs(1), stream_task).await;

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
                        a.last_text = Some(truncate_preview(&text));
                    }
                    Ok(Event::Result {
                        session_id: sid,
                        usage: u,
                        ..
                    }) => {
                        let mut a = accum.lock().await;
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

fn truncate_preview(s: &str) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut out = s[..MAX].to_string();
        out.push('…');
        out
    }
}
