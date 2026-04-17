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

/// Serve one client: complete hello handshake, then loop reading ops and
/// replying. Phase 1 implementation: every non-hello op yields `op_unknown`.
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

    // Read the client hello.
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

    // Read subsequent ops; every one returns OpUnknown until Phase 2.
    while let Ok(Some(line)) = reader.next_line().await {
        let reply = match serde_json::from_str::<ControlOp>(&line) {
            Ok(op) => dispatch_op(&state, op).await,
            Err(e) => ControlEvent::OpFailed {
                op: "".into(),
                task_id: None,
                error: format!("parse error: {e}"),
            },
        };
        if send_event(&writer, &reply).await.is_err() {
            break;
        }
    }
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
    async fn unknown_op_returns_op_unknown() {
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
        assert!(matches!(
            reply,
            ControlEvent::OpUnknown { op } if op == "list_workers"
        ));
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
}
