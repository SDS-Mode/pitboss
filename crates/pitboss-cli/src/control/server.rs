//! Control-socket server. Binds a unix socket, accepts at-most-one active TUI
//! connection, speaks the `ControlOp` / `ControlEvent` protocol one line of
//! JSON per message.
//!
//! Op handlers land across Phase 2 (Tasks 12–17). For Phase 1 the server only
//! accepts a connection, completes the hello handshake, and returns
//! `{event:"op_unknown"}` for every other op — enough to integration-test the
//! framing.

#![allow(dead_code)] // Some fields are set by Phase 2 tasks.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::control::protocol::{ControlEvent, ControlOp};

/// Handle returned from `start_control_server`. Drop terminates the accept loop
/// and removes the socket file.
pub struct ControlServerHandle {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

impl ControlServerHandle {
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for ControlServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.cancel.cancel();
        self.tracker.close();
        if let Some(h) = self.join_handle.take() {
            h.abort();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Start the control server. The returned handle owns the listener's lifetime.
///
/// `server_version`, `run_id`, `run_kind` are embedded in the hello response.
/// `state` is currently unused in Phase 1 — it threads the `DispatchState`
/// reference forward so Phase 2 op handlers can operate on it.
pub async fn start_control_server(
    socket_path: PathBuf,
    server_version: String,
    run_id: String,
    run_kind: String,
    state: Arc<crate::dispatch::state::DispatchState>,
) -> Result<ControlServerHandle> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    let listener = UnixListener::bind(&socket_path)?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let tracker = TaskTracker::new();
    let cancel = CancellationToken::new();
    let tracker_outer = tracker.clone();
    let cancel_outer = cancel.clone();
    let state_outer = state;

    let join_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                _ = cancel_outer.cancelled() => break,
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let cancel_inner = cancel_outer.clone();
                            let server_version = server_version.clone();
                            let run_id = run_id.clone();
                            let run_kind = run_kind.clone();
                            let state_inner = state_outer.clone();
                            let workers_names: Vec<String> = {
                                let guard = state_outer.workers.read().await;
                                guard.keys().cloned().collect()
                            };
                            tracker_outer.spawn(async move {
                                tokio::select! {
                                    _ = cancel_inner.cancelled() => {},
                                    _ = serve_connection(
                                        stream,
                                        server_version,
                                        run_id,
                                        run_kind,
                                        workers_names,
                                        state_inner,
                                    ) => {},
                                }
                            });
                        }
                        Err(e) => {
                            tracing::debug!("control accept error: {e}");
                        }
                    }
                }
            }
        }
    });

    Ok(ControlServerHandle {
        socket_path,
        shutdown_tx: Some(shutdown_tx),
        join_handle: Some(join_handle),
        tracker,
        cancel,
    })
}

/// Serve one client: complete hello handshake, install the control_writer,
/// drain any queued approvals, then concurrently pump outbound events and read
/// ops from the client. On disconnect clear the control_writer and abort the
/// pump task.
async fn serve_connection(
    stream: UnixStream,
    server_version: String,
    run_id: String,
    run_kind: String,
    workers_names: Vec<String>,
    state: Arc<crate::dispatch::state::DispatchState>,
) {
    let (read_half, write_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(write_half));
    let mut reader = BufReader::new(read_half).lines();

    // Hello handshake.
    let first = match reader.next_line().await {
        Ok(Some(line)) => line,
        _ => return,
    };
    match serde_json::from_str::<ControlOp>(&first) {
        Ok(ControlOp::Hello { .. }) => {}
        Ok(other) => {
            let _ = send_event(
                &writer,
                &ControlEvent::OpFailed {
                    op: format!("{other:?}"),
                    task_id: None,
                    error: "expected hello as first message".into(),
                },
            )
            .await;
            return;
        }
        Err(e) => {
            let _ = send_event(
                &writer,
                &ControlEvent::OpFailed {
                    op: "hello".into(),
                    task_id: None,
                    error: format!("parse error: {e}"),
                },
            )
            .await;
            return;
        }
    }

    // Send server hello.
    let _ = send_event(
        &writer,
        &ControlEvent::Hello {
            server_version,
            run_id,
            run_kind,
            workers: workers_names,
        },
    )
    .await;

    // Install this connection as the control_writer (displace any prior).
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<ControlEvent>();
    {
        let mut cw = state.control_writer.lock().await;
        if let Some(old) = cw.take() {
            let _ = old.send(ControlEvent::Superseded);
        }
        *cw = Some(ev_tx.clone());
    }

    // Drain any queued approvals now that a TUI is connected.
    {
        let mut queue = state.approval_queue.lock().await;
        while let Some(q) = queue.pop_front() {
            // Transfer responder into the bridge map.
            state
                .approval_bridge
                .lock()
                .await
                .insert(q.request_id.clone(), q.responder);
            // And push the event.
            let _ = ev_tx.send(ControlEvent::ApprovalRequest {
                request_id: q.request_id,
                task_id: q.task_id,
                summary: q.summary,
            });
        }
    }

    // Concurrent outbound pump: forward events from the mpsc to the socket.
    let writer_for_pump = writer.clone();
    let pump = tokio::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            if send_event(&writer_for_pump, &ev).await.is_err() {
                break;
            }
        }
    });

    // Read loop.
    while let Ok(Some(line)) = reader.next_line().await {
        let reply = match serde_json::from_str::<ControlOp>(&line) {
            Ok(op) => dispatch_op(&state, op).await,
            Err(e) => ControlEvent::OpFailed {
                op: String::new(),
                task_id: None,
                error: format!("parse error: {e}"),
            },
        };
        if send_event(&writer, &reply).await.is_err() {
            break;
        }
    }

    // Clear control_writer on disconnect.
    {
        let mut cw = state.control_writer.lock().await;
        *cw = None;
    }
    pump.abort();
}

async fn send_event(
    writer: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    ev: &ControlEvent,
) -> Result<()> {
    let mut line = serde_json::to_string(ev)?;
    line.push('\n');
    let mut guard = writer.lock().await;
    guard.write_all(line.as_bytes()).await?;
    guard.flush().await?;
    Ok(())
}

fn op_tag(op: &ControlOp) -> &'static str {
    match op {
        ControlOp::Hello { .. } => "hello",
        ControlOp::CancelWorker { .. } => "cancel_worker",
        ControlOp::CancelRun => "cancel_run",
        ControlOp::PauseWorker { .. } => "pause_worker",
        ControlOp::ContinueWorker { .. } => "continue_worker",
        ControlOp::RepromptWorker { .. } => "reprompt_worker",
        ControlOp::Approve { .. } => "approve",
        ControlOp::ListWorkers => "list_workers",
    }
}

async fn dispatch_op(
    state: &Arc<crate::dispatch::state::DispatchState>,
    op: ControlOp,
) -> ControlEvent {
    #[allow(unreachable_patterns)]
    match op {
        ControlOp::Hello { .. } => ControlEvent::OpAcked {
            op: "hello".into(),
            task_id: None,
        },
        ControlOp::CancelWorker { task_id } => {
            let cancels = state.worker_cancels.read().await;
            if let Some(tok) = cancels.get(&task_id) {
                tok.terminate();
                ControlEvent::OpAcked {
                    op: "cancel_worker".into(),
                    task_id: Some(task_id),
                }
            } else {
                ControlEvent::OpFailed {
                    op: "cancel_worker".into(),
                    task_id: Some(task_id.clone()),
                    error: format!("unknown task_id: {task_id}"),
                }
            }
        }
        ControlOp::CancelRun => {
            // Cascade the cancel into every live worker's own token FIRST, then
            // flip the run-level flag. The run-level `state.cancel` is only
            // observed by the lead's SessionHandle; workers have independent
            // per-task tokens and would otherwise keep running after the lead
            // dies. Without this cascade, users saw "kill run" ack but ps
            // still showed live claude workers.
            {
                let cancels = state.worker_cancels.read().await;
                for tok in cancels.values() {
                    tok.terminate();
                }
            }
            state.cancel.terminate();
            ControlEvent::OpAcked {
                op: "cancel_run".into(),
                task_id: None,
            }
        }
        ControlOp::PauseWorker { task_id } => {
            let mut workers = state.workers.write().await;
            let Some(entry) = workers.get(&task_id).cloned() else {
                return ControlEvent::OpFailed {
                    op: "pause_worker".into(),
                    task_id: Some(task_id),
                    error: "unknown task_id".into(),
                };
            };
            match entry {
                crate::dispatch::state::WorkerState::Running {
                    session_id: Some(sid),
                    ..
                } => {
                    let cancels = state.worker_cancels.read().await;
                    if let Some(tok) = cancels.get(&task_id) {
                        tok.terminate();
                    }
                    workers.insert(
                        task_id.clone(),
                        crate::dispatch::state::WorkerState::Paused {
                            session_id: sid,
                            paused_at: chrono::Utc::now(),
                            prior_token_usage: Default::default(),
                        },
                    );
                    let _ = crate::dispatch::events::append_event(
                        &state.run_subdir,
                        &task_id,
                        &crate::dispatch::events::TaskEvent::Pause {
                            at: chrono::Utc::now(),
                            reason: None,
                        },
                    )
                    .await;
                    state
                        .worker_counters
                        .write()
                        .await
                        .entry(task_id.clone())
                        .or_default()
                        .pause_count += 1;
                    ControlEvent::OpAcked {
                        op: "pause_worker".into(),
                        task_id: Some(task_id),
                    }
                }
                crate::dispatch::state::WorkerState::Running {
                    session_id: None, ..
                } => ControlEvent::OpUnknownState {
                    op: "pause_worker".into(),
                    task_id,
                    current_state: "spawning".into(),
                },
                crate::dispatch::state::WorkerState::Paused { .. } => {
                    ControlEvent::OpUnknownState {
                        op: "pause_worker".into(),
                        task_id,
                        current_state: "paused".into(),
                    }
                }
                crate::dispatch::state::WorkerState::Pending => ControlEvent::OpUnknownState {
                    op: "pause_worker".into(),
                    task_id,
                    current_state: "pending".into(),
                },
                crate::dispatch::state::WorkerState::Done(_) => ControlEvent::OpUnknownState {
                    op: "pause_worker".into(),
                    task_id,
                    current_state: "done".into(),
                },
            }
        }
        ControlOp::ContinueWorker { task_id, prompt } => {
            let current = state.workers.read().await.get(&task_id).cloned();
            match current {
                Some(crate::dispatch::state::WorkerState::Paused { session_id, .. }) => {
                    let prompt_text = prompt.unwrap_or_else(|| "continue".into());
                    let session_id_for_event = session_id.clone();
                    let prompt_for_event = prompt_text.clone();
                    match crate::mcp::tools::spawn_resume_worker(
                        state,
                        task_id.clone(),
                        prompt_text,
                        session_id,
                    )
                    .await
                    {
                        Ok(()) => {
                            let _ = crate::dispatch::events::append_event(
                                &state.run_subdir,
                                &task_id,
                                &crate::dispatch::events::TaskEvent::Continue {
                                    at: chrono::Utc::now(),
                                    new_session_id: session_id_for_event,
                                    prompt_preview: prompt_for_event.chars().take(80).collect(),
                                },
                            )
                            .await;
                            ControlEvent::OpAcked {
                                op: "continue_worker".into(),
                                task_id: Some(task_id),
                            }
                        }
                        Err(e) => ControlEvent::OpFailed {
                            op: "continue_worker".into(),
                            task_id: Some(task_id),
                            error: e.to_string(),
                        },
                    }
                }
                Some(_) => ControlEvent::OpUnknownState {
                    op: "continue_worker".into(),
                    task_id,
                    current_state: "not_paused".into(),
                },
                None => ControlEvent::OpFailed {
                    op: "continue_worker".into(),
                    task_id: Some(task_id),
                    error: "unknown task_id".into(),
                },
            }
        }
        ControlOp::RepromptWorker { task_id, prompt } => {
            let current = state.workers.read().await.get(&task_id).cloned();
            let session_id = match current {
                Some(crate::dispatch::state::WorkerState::Running {
                    session_id: Some(sid),
                    ..
                }) => {
                    let cancels = state.worker_cancels.read().await;
                    if let Some(tok) = cancels.get(&task_id) {
                        tok.terminate();
                    }
                    // Brief grace so the prior subprocess exits.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    sid
                }
                Some(crate::dispatch::state::WorkerState::Paused { session_id, .. }) => session_id,
                Some(_) => {
                    return ControlEvent::OpUnknownState {
                        op: "reprompt_worker".into(),
                        task_id,
                        current_state: "invalid".into(),
                    }
                }
                None => {
                    return ControlEvent::OpFailed {
                        op: "reprompt_worker".into(),
                        task_id: Some(task_id),
                        error: "unknown task_id".into(),
                    }
                }
            };
            let _ = crate::dispatch::events::append_event(
                &state.run_subdir,
                &task_id,
                &crate::dispatch::events::TaskEvent::Reprompt {
                    at: chrono::Utc::now(),
                    prompt_preview: prompt.chars().take(80).collect(),
                    prior_session_id: session_id.clone(),
                },
            )
            .await;
            match crate::mcp::tools::spawn_resume_worker(state, task_id.clone(), prompt, session_id)
                .await
            {
                Ok(()) => {
                    state
                        .worker_counters
                        .write()
                        .await
                        .entry(task_id.clone())
                        .or_default()
                        .reprompt_count += 1;
                    ControlEvent::OpAcked {
                        op: "reprompt_worker".into(),
                        task_id: Some(task_id),
                    }
                }
                Err(e) => ControlEvent::OpFailed {
                    op: "reprompt_worker".into(),
                    task_id: Some(task_id),
                    error: e.to_string(),
                },
            }
        }
        ControlOp::ListWorkers => {
            let workers = state.workers.read().await;
            let prompts = state.worker_prompts.read().await;
            let entries = workers
                .iter()
                .map(|(id, w)| {
                    let (state_str, started_at, session_id) = match w {
                        crate::dispatch::state::WorkerState::Pending => {
                            ("pending".to_string(), None, None)
                        }
                        crate::dispatch::state::WorkerState::Running {
                            started_at,
                            session_id,
                        } => (
                            "running".to_string(),
                            Some(started_at.to_rfc3339()),
                            session_id.clone(),
                        ),
                        crate::dispatch::state::WorkerState::Paused {
                            paused_at,
                            session_id,
                            ..
                        } => (
                            "paused".to_string(),
                            Some(paused_at.to_rfc3339()),
                            Some(session_id.clone()),
                        ),
                        crate::dispatch::state::WorkerState::Done(rec) => (
                            match rec.status {
                                pitboss_core::store::TaskStatus::Success => "done_success",
                                pitboss_core::store::TaskStatus::Failed => "done_failed",
                                pitboss_core::store::TaskStatus::TimedOut => "done_timed_out",
                                pitboss_core::store::TaskStatus::Cancelled => "done_cancelled",
                                pitboss_core::store::TaskStatus::SpawnFailed => "done_spawn_failed",
                            }
                            .to_string(),
                            Some(rec.started_at.to_rfc3339()),
                            rec.claude_session_id.clone(),
                        ),
                    };
                    crate::control::protocol::WorkerSnapshotEntry {
                        task_id: id.clone(),
                        state: state_str,
                        prompt_preview: prompts.get(id).cloned().unwrap_or_default(),
                        started_at,
                        parent_task_id: None,
                        session_id,
                    }
                })
                .collect();
            ControlEvent::WorkersSnapshot { workers: entries }
        }
        ControlOp::Approve {
            request_id,
            approved,
            comment,
            edited_summary,
        } => {
            let tx = state.approval_bridge.lock().await.remove(&request_id);
            if let Some(tx) = tx {
                let edited = edited_summary.is_some();
                let _ = tx.send(crate::dispatch::state::ApprovalResponse {
                    approved,
                    comment,
                    edited_summary,
                });
                // Write an approval_response event + bump counters so the
                // control-socket path produces the same audit trail as
                // ApprovalBridge::respond would. Matters when the approval
                // was drained from the queue (no TUI at request time).
                let _ = crate::dispatch::events::append_event(
                    &state.run_subdir,
                    &state.lead_id,
                    &crate::dispatch::events::TaskEvent::ApprovalResponse {
                        at: chrono::Utc::now(),
                        request_id: request_id.clone(),
                        approved,
                        edited,
                    },
                )
                .await;
                {
                    let mut guard = state.worker_counters.write().await;
                    let entry = guard.entry(state.lead_id.clone()).or_default();
                    if approved {
                        entry.approvals_approved += 1;
                    } else {
                        entry.approvals_rejected += 1;
                    }
                }
                ControlEvent::OpAcked {
                    op: "approve".into(),
                    task_id: None,
                }
            } else {
                ControlEvent::OpFailed {
                    op: "approve".into(),
                    task_id: None,
                    error: format!("unknown request_id: {request_id}"),
                }
            }
        }
        other => ControlEvent::OpUnknown {
            op: op_tag(&other).into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::{ApprovalPolicy, DispatchState};
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use pitboss_core::process::{ProcessSpawner, TokioSpawner};
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use uuid::Uuid;

    fn mk_state(dir: &Path, run_id: Uuid) -> Arc<DispatchState> {
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.to_path_buf()));
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            dir.join(run_id.to_string()),
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ))
    }

    #[tokio::test]
    async fn hello_handshake_roundtrips() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        let sock = dir.path().join("control.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "flat".into(),
            state,
        )
        .await
        .unwrap();
        assert!(sock.exists());

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let hello_line = lines.next_line().await.unwrap().expect("hello line");
        let ev: ControlEvent = serde_json::from_str(&hello_line).unwrap();
        match ev {
            ControlEvent::Hello {
                server_version,
                run_id: rid,
                run_kind,
                ..
            } => {
                assert_eq!(server_version, "0.4.0");
                assert_eq!(rid, run_id.to_string());
                assert_eq!(run_kind, "flat");
            }
            other => panic!("expected Hello, got {other:?}"),
        }
        drop(handle);
    }

    #[tokio::test]
    async fn unknown_op_returns_parse_error() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        let sock = dir.path().join("unknown.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "flat".into(),
            state,
        )
        .await
        .unwrap();
        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream.write_all(b"{\"op\":\"wibble\"}\n").await.unwrap();
        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpFailed { op, .. } if op.is_empty()
        ));
        drop(handle);
    }

    #[tokio::test]
    async fn approve_op_completes_pending_request() {
        use crate::mcp::approval::ApprovalBridge;
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);

        // Pre-seed the bridge map with a request_id + oneshot sender.
        let (tx, rx) = tokio::sync::oneshot::channel();
        state
            .approval_bridge
            .lock()
            .await
            .insert("req-1".into(), tx);

        let sock = dir.path().join("approve.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "hierarchical".into(),
            state.clone(),
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(
                b"{\"op\":\"approve\",\"request_id\":\"req-1\",\"approved\":true,\"edited_summary\":\"go\"}\n",
            )
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, .. } if op == "approve"
        ));

        let resp = tokio::time::timeout(Duration::from_millis(500), rx)
            .await
            .unwrap()
            .unwrap();
        assert!(resp.approved);
        assert_eq!(resp.edited_summary.as_deref(), Some("go"));

        // Silence unused warnings on ApprovalBridge import.
        let _ = ApprovalBridge::new(state);
        drop(handle);
    }

    #[tokio::test]
    async fn cancel_worker_op_terminates_worker_token() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        let worker_token = CancelToken::new();
        state
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state
            .workers
            .write()
            .await
            .insert("w-1".into(), crate::dispatch::state::WorkerState::Pending);

        let sock = dir.path().join("cancel.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "flat".into(),
            state,
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(b"{\"op\":\"cancel_worker\",\"task_id\":\"w-1\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, task_id: Some(ref tid) }
                if op == "cancel_worker" && tid == "w-1"
        ));
        assert!(worker_token.is_terminated());
        drop(handle);
    }

    #[tokio::test]
    async fn cancel_run_op_cascades_to_every_worker_token() {
        // Regression: `CancelRun` used to only fire state.cancel (lead-only
        // token). Workers have per-task tokens in state.worker_cancels;
        // without cascading, workers stayed alive after a kill-run and the
        // TUI showed the run as dead while `ps` showed live claude procs.
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);

        let w1 = pitboss_core::session::CancelToken::new();
        let w2 = pitboss_core::session::CancelToken::new();
        state
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), w1.clone());
        state
            .worker_cancels
            .write()
            .await
            .insert("w-2".into(), w2.clone());

        let sock = dir.path().join("cancel-run-cascade.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "flat".into(),
            state,
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(b"{\"op\":\"cancel_run\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, .. } if op == "cancel_run"
        ));
        assert!(
            w1.is_terminated(),
            "worker 1 cancel token must be terminated"
        );
        assert!(
            w2.is_terminated(),
            "worker 2 cancel token must be terminated"
        );
        drop(handle);
    }

    #[tokio::test]
    async fn cancel_run_op_terminates_run_cancel_token() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        let run_cancel = state.cancel.clone();

        let sock = dir.path().join("cancel-run.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "flat".into(),
            state,
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(b"{\"op\":\"cancel_run\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, .. } if op == "cancel_run"
        ));
        assert!(run_cancel.is_terminated());
        drop(handle);
    }

    #[tokio::test]
    async fn pause_worker_transitions_running_to_paused() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);

        let worker_token = CancelToken::new();
        state
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess-xyz".into()),
            },
        );

        let sock = dir.path().join("pause.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "hierarchical".into(),
            state.clone(),
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(b"{\"op\":\"pause_worker\",\"task_id\":\"w-1\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, .. } if op == "pause_worker"
        ));
        assert!(worker_token.is_terminated());
        let workers = state.workers.read().await;
        match workers.get("w-1").unwrap() {
            crate::dispatch::state::WorkerState::Paused { session_id, .. } => {
                assert_eq!(session_id, "sess-xyz");
            }
            other => panic!("expected Paused, got {other:?}"),
        }
        drop(handle);
    }

    #[tokio::test]
    async fn continue_worker_from_paused_transitions_running() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        state.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Paused {
                session_id: "sess-xyz".into(),
                paused_at: chrono::Utc::now(),
                prior_token_usage: Default::default(),
            },
        );
        state
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "hi".into());
        state
            .worker_models
            .write()
            .await
            .insert("w-1".into(), "claude-haiku-4-5".into());

        let sock = dir.path().join("continue.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "hierarchical".into(),
            state.clone(),
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(b"{\"op\":\"continue_worker\",\"task_id\":\"w-1\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, .. } if op == "continue_worker"
        ));
        let workers = state.workers.read().await;
        assert!(matches!(
            workers.get("w-1").unwrap(),
            crate::dispatch::state::WorkerState::Running { .. }
        ));
        drop(handle);
    }

    #[tokio::test]
    async fn reprompt_worker_from_running_terminates_and_respawns() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        let worker_token = CancelToken::new();
        state
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess-xyz".into()),
            },
        );
        state
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "hi".into());
        state
            .worker_models
            .write()
            .await
            .insert("w-1".into(), "claude-haiku-4-5".into());

        let sock = dir.path().join("reprompt.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "hierarchical".into(),
            state.clone(),
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(
                b"{\"op\":\"reprompt_worker\",\"task_id\":\"w-1\",\"prompt\":\"new plan\"}\n",
            )
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        assert!(matches!(
            reply,
            ControlEvent::OpAcked { ref op, .. } if op == "reprompt_worker"
        ));
        assert!(worker_token.is_terminated());
        drop(handle);
    }

    #[tokio::test]
    async fn list_workers_op_returns_workers_snapshot() {
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);
        state.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess".into()),
            },
        );
        state
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "investigate bug".into());

        let sock = dir.path().join("list.sock");
        let handle = start_control_server(
            sock.clone(),
            "0.4.0".into(),
            run_id.to_string(),
            "flat".into(),
            state,
        )
        .await
        .unwrap();

        let mut stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
        stream
            .write_all(b"{\"op\":\"hello\",\"client_version\":\"0.4.0\"}\n")
            .await
            .unwrap();
        stream
            .write_all(b"{\"op\":\"list_workers\"}\n")
            .await
            .unwrap();

        let (r, _w) = stream.split();
        let mut lines = BufReader::new(r).lines();
        let _hello = lines.next_line().await.unwrap();
        let reply_line = lines.next_line().await.unwrap().unwrap();
        let reply: ControlEvent = serde_json::from_str(&reply_line).unwrap();
        match reply {
            ControlEvent::WorkersSnapshot { workers } => {
                assert_eq!(workers.len(), 1);
                assert_eq!(workers[0].task_id, "w-1");
                assert_eq!(workers[0].state, "running");
                assert_eq!(workers[0].session_id.as_deref(), Some("sess"));
            }
            other => panic!("expected WorkersSnapshot, got {other:?}"),
        }
        drop(handle);
    }
}
