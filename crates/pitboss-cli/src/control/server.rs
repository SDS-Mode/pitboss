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
                            // Workers snapshot is deferred to *after* the
                            // client Hello is received (inside
                            // `serve_connection`). Taking it here would
                            // miss any worker that spawned in the window
                            // between accept and Hello arrival, leaving
                            // the TUI with stale tiles until the next
                            // broadcast.
                            tracker_outer.spawn(async move {
                                tokio::select! {
                                    _ = cancel_inner.cancelled() => {},
                                    _ = serve_connection(
                                        stream,
                                        server_version,
                                        run_id,
                                        run_kind,
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

    // Snapshot the current worker set *after* receiving the client Hello,
    // not at accept time. Any worker that spawned between accept and this
    // point is now visible to the TUI on first paint; deferring avoids
    // the previously-observed race where the snapshot was empty but a
    // worker was already registered.
    let workers_names: Vec<String> = {
        let guard = state.root.workers.read().await;
        guard.keys().cloned().collect()
    };

    // Snapshot current policy rules (if any) to send in Hello.
    let policy_rules = {
        let guard = state.root.policy_matcher.lock().await;
        guard
            .as_ref()
            .map(|m| m.rules().to_vec())
            .unwrap_or_default()
    };

    // Send server hello.
    let _ = send_event(
        &writer,
        &ControlEvent::Hello {
            server_version,
            run_id,
            run_kind,
            workers: workers_names,
            policy_rules,
        },
    )
    .await;

    // Install this connection as the control_writer (displace any prior).
    // The connection-unique `writer_id` lets the disconnect cleanup block
    // verify the slot still holds OUR writer before clearing — a racing
    // reconnect that swapped in its own writer between this install and
    // our disconnect has a different id, so our cleanup skips the clear.
    let writer_id = uuid::Uuid::now_v7();
    let (ev_tx, mut ev_rx) =
        tokio::sync::mpsc::channel::<ControlEvent>(crate::dispatch::layer::CONTROL_EVENT_QUEUE_CAP);
    {
        let mut cw = state.root.control_writer.lock().await;
        if let Some(old) = cw.take() {
            let _ = old.sender.try_send(ControlEvent::Superseded);
        }
        *cw = Some(crate::dispatch::layer::ControlWriterSlot {
            id: writer_id,
            sender: ev_tx.clone(),
        });
    }

    // Drain any queued approvals now that a TUI is connected.
    {
        let mut queue = state.root.approval_queue.lock().await;
        while let Some(q) = queue.pop_front() {
            // Transfer responder into the bridge map, preserving TTL metadata
            // so expire_layer_approvals can still expire the entry if the
            // operator doesn't act before the TTL fires.
            state.root.approval_bridge.lock().await.insert(
                q.request_id.clone(),
                crate::dispatch::state::BridgeEntry {
                    responder: q.responder,
                    task_id: q.task_id.clone(),
                    ttl_secs: q.ttl_secs,
                    fallback: q.fallback,
                    created_at: q.created_at,
                },
            );
            // And push the event.
            let _ = ev_tx
                .send(ControlEvent::ApprovalRequest {
                    request_id: q.request_id,
                    task_id: q.task_id,
                    summary: q.summary,
                    plan: q.plan.map(crate::mcp::approval::approval_plan_to_wire),
                    kind: q.kind,
                })
                .await;
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

    // Periodic shared-store activity broadcaster. Each connection gets
    // its own ticker so when the TUI reconnects it starts from the
    // current live counters. 1 s cadence is fast enough for the TUI's
    // 250 ms poll to feel responsive without flooding the socket.
    let ev_tx_activity = ev_tx.clone();
    let state_activity = state.clone();
    let activity_pump = tokio::spawn(async move {
        // Delay first emission by one period. tokio::time::interval's default
        // is to fire immediately on construction, which would race ahead of
        // the first OpAcked in tests that send an op right after hello.
        let period = std::time::Duration::from_millis(STORE_ACTIVITY_INTERVAL_MS);
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let snapshot = state_activity.root.shared_store.activity_snapshot().await;
            let counters: Vec<crate::control::protocol::ActorActivityEntry> = snapshot
                .into_iter()
                .map(
                    |(actor_id, c)| crate::control::protocol::ActorActivityEntry {
                        actor_id,
                        kv_ops: c.kv_ops,
                        lease_ops: c.lease_ops,
                    },
                )
                .collect();
            // `try_send` — if the queue is full, skip this tick rather
            // than block the activity pump; the next tick re-reads
            // fresh counters anyway.
            match ev_tx_activity.try_send(ControlEvent::StoreActivity { counters }) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
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

    // Clear control_writer on disconnect — but ONLY if the slot still
    // holds OUR writer. A reconnecting TUI can install its own writer
    // between our read loop exiting and this cleanup block running; a
    // blind clear would silently disconnect the reconnected client
    // (events go nowhere, no diagnostic surface).
    {
        let mut cw = state.root.control_writer.lock().await;
        if cw.as_ref().is_some_and(|slot| slot.id == writer_id) {
            *cw = None;
        }
    }
    pump.abort();
    activity_pump.abort();
}

/// Best-effort SIGCONT helper: if `task_id` is currently `Frozen`,
/// send SIGCONT so a follow-up SIGTERM / SIGKILL can actually be
/// delivered. No-op if the worker isn't frozen or the pid slot is
/// empty. Errors are logged and swallowed — cancel paths must not
/// fail because of signal cleanup.
async fn thaw_if_frozen(state: &Arc<crate::dispatch::state::DispatchState>, task_id: &str) {
    let is_frozen = matches!(
        state.root.workers.read().await.get(task_id),
        Some(crate::dispatch::state::WorkerState::Frozen { .. })
    );
    if !is_frozen {
        return;
    }
    let pid = state
        .root
        .worker_pids
        .read()
        .await
        .get(task_id)
        .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
        .unwrap_or(0);
    if pid > 0 {
        if let Err(e) = crate::dispatch::signals::resume_stopped(pid) {
            tracing::debug!(task_id, pid, "pre-cancel SIGCONT failed: {e}");
        }
    }
}

/// Period between `StoreActivity` broadcasts on the control socket.
/// Tuned for TUI poll cadence (250 ms) — faster than 1 s is noise,
/// slower feels laggy. Constant so tests can match expected payloads.
const STORE_ACTIVITY_INTERVAL_MS: u64 = 1000;

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
        ControlOp::UpdatePolicy { .. } => "update_policy",
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
            // If this worker is currently Frozen (SIGSTOP'd), we must
            // SIGCONT it first so the subsequent SIGTERM is actually
            // deliverable — a stopped process can't drain signals until
            // it's running again. Harmless for non-frozen workers.
            thaw_if_frozen(state, &task_id).await;
            let cancels = state.root.worker_cancels.read().await;
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
            // Cascade cancel across the whole dispatch tree:
            //   root lead   → state.root.cancel           (terminal)
            //   root workers → state.root.worker_cancels  (per-worker tokens)
            //   sub-leads   → sub_layer.cancel       (bridged to the
            //                                          sub-lead's claude
            //                                          proc_cancel at
            //                                          sublead.rs:566)
            //   sub-lead-owned workers → sub_layer.worker_cancels
            //
            // First phase: DRAIN the root cancel. This flips
            // `state.root.cancel.is_draining()` to true, which
            // `spawn_sublead_session` checks synchronously at sublead.rs
            // :339 — any sublead spawn racing this handler sees the drain
            // and self-cancels before its claude subprocess starts. Must
            // precede the cascade iteration below, otherwise a sublead
            // that appears in state.subleads between our snapshot and
            // state.root.cancel.terminate() would slip through.
            state.root.cancel.drain();

            // SIGCONT any frozen workers (root + every sublead) so they
            // respond to the subsequent terminate. Frozen via SIGSTOP
            // can't act on a cancel token until SIGCONT'd. Mirrors the
            // CancelWorker handler's behavior; now cascades to all layers.
            {
                let pids = state.root.worker_pids.read().await;
                let workers = state.root.workers.read().await;
                for (id, w) in workers.iter() {
                    if matches!(w, crate::dispatch::state::WorkerState::Frozen { .. }) {
                        let pid = pids
                            .get(id)
                            .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
                            .unwrap_or(0);
                        if pid > 0 {
                            let _ = crate::dispatch::signals::resume_stopped(pid);
                        }
                    }
                }
                drop(workers);
                let subleads = state.subleads.read().await;
                for sub_layer in subleads.values() {
                    let sub_workers = sub_layer.workers.read().await;
                    for (id, w) in sub_workers.iter() {
                        if matches!(w, crate::dispatch::state::WorkerState::Frozen { .. }) {
                            let pid = pids
                                .get(id)
                                .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
                                .unwrap_or(0);
                            if pid > 0 {
                                let _ = crate::dispatch::signals::resume_stopped(pid);
                            }
                        }
                    }
                }
            }

            // Root-layer worker tokens.
            {
                let cancels = state.root.worker_cancels.read().await;
                for tok in cancels.values() {
                    tok.terminate();
                }
            }

            // Sub-lead layer cancels + each sub-lead's own workers.
            // Terminating sub_layer.cancel cascades to the sub-lead's
            // claude subprocess (via the tree_cancel → proc_cancel bridge
            // installed in sublead.rs:566). We also explicitly iterate
            // the sub-lead's worker_cancels because those workers have
            // their own sibling tokens that aren't bridged to the
            // sub-lead's cancel.
            {
                let subleads = state.subleads.read().await;
                for sub_layer in subleads.values() {
                    let sub_cancels = sub_layer.worker_cancels.read().await;
                    for tok in sub_cancels.values() {
                        tok.terminate();
                    }
                    drop(sub_cancels);
                    sub_layer.cancel.terminate();
                }
            }

            // Root cancel, last — the lead's SessionHandle observes this
            // via `await_terminate()` and sends SIGTERM → SIGKILL to the
            // root claude subprocess.
            state.root.cancel.terminate();
            ControlEvent::OpAcked {
                op: "cancel_run".into(),
                task_id: None,
            }
        }
        ControlOp::PauseWorker { task_id, mode } => {
            let mut workers = state.root.workers.write().await;
            let Some(entry) = workers.get(&task_id).cloned() else {
                return ControlEvent::OpFailed {
                    op: "pause_worker".into(),
                    task_id: Some(task_id),
                    error: "unknown task_id".into(),
                };
            };
            match entry {
                crate::dispatch::state::WorkerState::Running {
                    started_at,
                    session_id: Some(sid),
                } => {
                    match mode {
                        crate::control::protocol::PauseMode::Cancel => {
                            let cancels = state.root.worker_cancels.read().await;
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
                        }
                        crate::control::protocol::PauseMode::Freeze => {
                            let pid = state
                                .root
                                .worker_pids
                                .read()
                                .await
                                .get(&task_id)
                                .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
                                .unwrap_or(0);
                            if pid == 0 {
                                return ControlEvent::OpFailed {
                                    op: "pause_worker".into(),
                                    task_id: Some(task_id),
                                    error: "pid slot empty; cannot freeze".into(),
                                };
                            }
                            if let Err(e) = crate::dispatch::signals::freeze(pid) {
                                return ControlEvent::OpFailed {
                                    op: "pause_worker".into(),
                                    task_id: Some(task_id),
                                    error: format!("freeze failed: {e}"),
                                };
                            }
                            workers.insert(
                                task_id.clone(),
                                crate::dispatch::state::WorkerState::Frozen {
                                    session_id: sid,
                                    frozen_at: chrono::Utc::now(),
                                    started_at,
                                },
                            );
                        }
                    }
                    let _ = crate::dispatch::events::append_event(
                        &state.root.run_subdir,
                        &task_id,
                        &crate::dispatch::events::TaskEvent::Pause {
                            at: chrono::Utc::now(),
                            reason: None,
                        },
                    )
                    .await;
                    state
                        .root
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
                crate::dispatch::state::WorkerState::Frozen { .. } => {
                    ControlEvent::OpUnknownState {
                        op: "pause_worker".into(),
                        task_id,
                        current_state: "frozen".into(),
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
            let current = state.root.workers.read().await.get(&task_id).cloned();
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
                                &state.root.run_subdir,
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
                Some(crate::dispatch::state::WorkerState::Frozen {
                    session_id,
                    started_at,
                    ..
                }) => {
                    // SIGCONT the frozen process. `prompt` is ignored —
                    // freeze-mode preserves state and has no resume point.
                    let pid = state
                        .root
                        .worker_pids
                        .read()
                        .await
                        .get(&task_id)
                        .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
                        .unwrap_or(0);
                    if pid == 0 {
                        return ControlEvent::OpFailed {
                            op: "continue_worker".into(),
                            task_id: Some(task_id),
                            error: "pid slot empty; cannot thaw".into(),
                        };
                    }
                    if let Err(e) = crate::dispatch::signals::resume_stopped(pid) {
                        return ControlEvent::OpFailed {
                            op: "continue_worker".into(),
                            task_id: Some(task_id),
                            error: format!("SIGCONT failed: {e}"),
                        };
                    }
                    state.root.workers.write().await.insert(
                        task_id.clone(),
                        crate::dispatch::state::WorkerState::Running {
                            started_at,
                            session_id: Some(session_id),
                        },
                    );
                    ControlEvent::OpAcked {
                        op: "continue_worker".into(),
                        task_id: Some(task_id),
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
            // Route by task_id:
            // - Sub-lead id → deliver via the sub-lead's own
            //   reprompt_tx channel (sub_layer.send_synthetic_reprompt).
            //   The sub-lead's background kill+resume loop (see
            //   dispatch/sublead.rs:642) consumes reprompt_rx and
            //   re-launches its claude subprocess with `--resume
            //   <session_id>` and the new prompt.
            // - Root-layer worker id → existing spawn_resume_worker
            //   flow (below).
            // - Everything else → unknown task_id.
            //
            // Pre-fix, sub-lead reprompts returned "unknown task_id"
            // because the handler only looked at `state.root.workers`
            // (= root layer via Deref). Sub-leads live in
            // `state.subleads`, not `state.root.workers`, so the check
            // always missed.
            {
                let subleads = state.subleads.read().await;
                if let Some(sub_layer) = subleads.get(&task_id).cloned() {
                    drop(subleads);
                    let _ = crate::dispatch::events::append_event(
                        &state.root.run_subdir,
                        &task_id,
                        &crate::dispatch::events::TaskEvent::Reprompt {
                            at: chrono::Utc::now(),
                            prompt_preview: prompt.chars().take(80).collect(),
                            prior_session_id: String::new(),
                        },
                    )
                    .await;
                    sub_layer.send_synthetic_reprompt(&prompt).await;
                    return ControlEvent::OpAcked {
                        op: "reprompt_worker".into(),
                        task_id: Some(task_id),
                    };
                }
            }

            let current = state.root.workers.read().await.get(&task_id).cloned();
            let session_id = match current {
                Some(crate::dispatch::state::WorkerState::Running {
                    session_id: Some(sid),
                    ..
                }) => {
                    let cancels = state.root.worker_cancels.read().await;
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
                &state.root.run_subdir,
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
                        .root
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
            let workers = state.root.workers.read().await;
            let prompts = state.root.worker_prompts.read().await;
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
                        crate::dispatch::state::WorkerState::Frozen {
                            frozen_at,
                            session_id,
                            ..
                        } => (
                            "frozen".to_string(),
                            Some(frozen_at.to_rfc3339()),
                            Some(session_id.clone()),
                        ),
                        crate::dispatch::state::WorkerState::Done(rec) => (
                            match rec.status {
                                pitboss_core::store::TaskStatus::Success => "done_success",
                                pitboss_core::store::TaskStatus::Failed => "done_failed",
                                pitboss_core::store::TaskStatus::TimedOut => "done_timed_out",
                                pitboss_core::store::TaskStatus::Cancelled => "done_cancelled",
                                pitboss_core::store::TaskStatus::SpawnFailed => "done_spawn_failed",
                                pitboss_core::store::TaskStatus::ApprovalRejected => {
                                    "done_approval_rejected"
                                }
                                pitboss_core::store::TaskStatus::ApprovalTimedOut => {
                                    "done_approval_timed_out"
                                }
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
            reason,
        } => {
            let bridge_entry = state.root.approval_bridge.lock().await.remove(&request_id);
            if let Some(bridge_entry) = bridge_entry {
                let caller_id = bridge_entry.task_id.clone();
                let edited = edited_summary.is_some();
                let _ = bridge_entry
                    .responder
                    .send(crate::dispatch::state::ApprovalResponse {
                        approved,
                        comment,
                        edited_summary,
                        reason,
                        from_ttl: false,
                    });
                // Write an approval_response event + bump counters so the
                // control-socket path produces the same audit trail as
                // ApprovalBridge::respond would. Matters when the approval
                // was drained from the queue (no TUI at request time).
                let _ = crate::dispatch::events::append_event(
                    &state.root.run_subdir,
                    &state.root.lead_id,
                    &crate::dispatch::events::TaskEvent::ApprovalResponse {
                        at: chrono::Utc::now(),
                        request_id: request_id.clone(),
                        approved,
                        edited,
                    },
                )
                .await;
                {
                    let mut guard = state.root.worker_counters.write().await;
                    let entry = guard.entry(caller_id).or_default();
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
        ControlOp::UpdatePolicy { rules } => {
            let matcher = crate::mcp::policy::PolicyMatcher::new(rules);
            state.root.set_policy_matcher(matcher).await;
            ControlEvent::OpAcked {
                op: "update_policy".into(),
                task_id: None,
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
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
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

        // Pre-seed the bridge map with a request_id + BridgeEntry.
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.root.approval_bridge.lock().await.insert(
            "req-1".into(),
            crate::dispatch::state::BridgeEntry {
                responder: tx,
                task_id: "test-task".into(),
                ttl_secs: None,
                fallback: None,
                created_at: chrono::Utc::now(),
            },
        );

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
            .root
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state
            .root
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
    async fn cancel_run_op_cascades_to_sublead_layers() {
        // Regression coverage for task #60: CancelRun used to fire only
        // root.cancel + state.root.worker_cancels. Sub-lead claude
        // subprocesses, plus any workers the sub-lead had spawned,
        // stayed alive — their cancel tokens live on the sub-layer,
        // not on root. Observable pre-fix: operator hits cancel, root
        // lead dies, `ps aux | grep claude` shows orphans for up to
        // lead_timeout_secs per sub-lead. Cascade must reach:
        //   - sub_layer.cancel (bridged to the sub-lead's claude proc
        //     via sublead.rs:566)
        //   - every token in sub_layer.worker_cancels
        //
        // Also: root.cancel.drain() must fire BEFORE the iteration so
        // any sub-lead being spawned synchronously (racing the cancel)
        // sees is_draining()=true and self-cancels at sublead.rs:339.
        use crate::dispatch::layer::LayerState;
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;

        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);

        // Build a sub-layer with its own cancel + two worker tokens.
        let sub_cancel = pitboss_core::session::CancelToken::new();
        let sub_w1 = pitboss_core::session::CancelToken::new();
        let sub_w2 = pitboss_core::session::CancelToken::new();
        let sub_manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::Never,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(50.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        let sub_layer = std::sync::Arc::new(LayerState::new(
            run_id,
            sub_manifest,
            state.root.store.clone(),
            sub_cancel.clone(),
            "sublead-test".into(),
            state.root.spawner.clone(),
            std::path::PathBuf::from("/bin/true"),
            state.root.wt_mgr.clone(),
            CleanupPolicy::Never,
            dir.path().join(run_id.to_string()),
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
            None,
        ));
        sub_layer
            .worker_cancels
            .write()
            .await
            .insert("sub-w-1".into(), sub_w1.clone());
        sub_layer
            .worker_cancels
            .write()
            .await
            .insert("sub-w-2".into(), sub_w2.clone());
        state
            .subleads
            .write()
            .await
            .insert("sublead-test".into(), sub_layer.clone());

        let sock = dir.path().join("cancel-run-sublead.sock");
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
            state.root.cancel.is_draining(),
            "root cancel must be draining so racing sublead spawns self-cancel"
        );
        assert!(
            state.root.cancel.is_terminated(),
            "root cancel terminates last"
        );
        assert!(
            sub_cancel.is_terminated(),
            "sub-layer cancel must be terminated so the sub-lead's claude proc dies"
        );
        assert!(
            sub_w1.is_terminated(),
            "sub-lead-owned worker 1 token must be terminated"
        );
        assert!(
            sub_w2.is_terminated(),
            "sub-lead-owned worker 2 token must be terminated"
        );
        drop(handle);
    }

    #[tokio::test]
    async fn cancel_run_op_cascades_to_every_worker_token() {
        // Regression: `CancelRun` used to only fire state.root.cancel (lead-only
        // token). Workers have per-task tokens in state.root.worker_cancels;
        // without cascading, workers stayed alive after a kill-run and the
        // TUI showed the run as dead while `ps` showed live claude procs.
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let state = mk_state(dir.path(), run_id);

        let w1 = pitboss_core::session::CancelToken::new();
        let w2 = pitboss_core::session::CancelToken::new();
        state
            .root
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), w1.clone());
        state
            .root
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
        let run_cancel = state.root.cancel.clone();

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
            .root
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state.root.workers.write().await.insert(
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
        let workers = state.root.workers.read().await;
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
        state.root.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Paused {
                session_id: "sess-xyz".into(),
                paused_at: chrono::Utc::now(),
                prior_token_usage: Default::default(),
            },
        );
        state
            .root
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "hi".into());
        state
            .root
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
        let workers = state.root.workers.read().await;
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
            .root
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state.root.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess-xyz".into()),
            },
        );
        state
            .root
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "hi".into());
        state
            .root
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
        state.root.workers.write().await.insert(
            "w-1".into(),
            crate::dispatch::state::WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess".into()),
            },
        );
        state
            .root
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
