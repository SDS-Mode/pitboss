use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
        let mut reader = BufReader::new(stdout).lines();

        let mut log_writer = if let Some(path) = &self.log_path {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .ok()
        } else {
            None
        };

        let mut usage = TokenUsage::default();
        let mut session_id: Option<String> = None;
        let mut last_text: Option<String> = None;
        let mut saw_result = false;

        let terminate_fut = cancel.await_terminate();
        tokio::pin!(terminate_fut);

        let stream_result = {
            let stream_fut = stream_loop(
                &mut reader,
                &mut log_writer,
                &mut usage,
                &mut session_id,
                &mut last_text,
                &mut saw_result,
            );
            tokio::pin!(stream_fut);

            tokio::select! {
                biased;
                () = &mut terminate_fut => StreamEnd::Terminated,
                () = tokio::time::sleep(timeout) => StreamEnd::TimedOut,
                end = &mut stream_fut => end,
            }
        };

        if matches!(stream_result, StreamEnd::Terminated | StreamEnd::TimedOut) {
            let _ = child.terminate();
            tokio::time::sleep(super::TERMINATE_GRACE).await;
            let _ = child.kill();
        }

        let status = child.wait().await.ok();
        let exit_code = status.as_ref().and_then(std::process::ExitStatus::code);
        let ended_at = Utc::now();

        let final_state = match &stream_result {
            StreamEnd::TimedOut => SessionState::TimedOut,
            StreamEnd::Terminated => SessionState::Cancelled,
            StreamEnd::Eof | StreamEnd::ReadError => match exit_code {
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
            token_usage: usage,
            claude_session_id: session_id,
            final_message_preview: last_text,
            started_at,
            ended_at,
        }
    }
}

enum StreamEnd {
    Eof,
    ReadError,
    Terminated,
    TimedOut,
}

async fn stream_loop<R: AsyncBufReadExt + Unpin>(
    reader: &mut tokio::io::Lines<R>,
    log: &mut Option<tokio::fs::File>,
    usage: &mut TokenUsage,
    session_id: &mut Option<String>,
    last_text: &mut Option<String>,
    saw_result: &mut bool,
) -> StreamEnd {
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => {
                if let Some(w) = log.as_mut() {
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                }
                match parse_line(line.as_bytes()) {
                    Ok(Event::AssistantText { text }) => {
                        *last_text = Some(truncate_preview(&text));
                    }
                    Ok(Event::Result {
                        session_id: sid,
                        usage: u,
                        ..
                    }) => {
                        *session_id = Some(sid);
                        usage.add(&u);
                        *saw_result = true;
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            Ok(None) => return StreamEnd::Eof,
            Err(_) => return StreamEnd::ReadError,
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
