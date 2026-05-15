//! Unified run-state consumer stream.
//!
//! Design RFC: `book/src/architecture/unified-envelope-api.md`
//! Issue: <https://github.com/SDS-Mode/pitboss/issues/438>
//!
//! This is the consumer-facing API that collapses the historical
//! `summary.json` / `summary.jsonl` reader (see [`crate::store::summary`])
//! and the live per-run control socket (see
//! `crates/pitboss-cli/src/control`) into one envelope-shaped stream.
//!
//! Three transport modes:
//! - [`StreamMode::ReplayOnly`] — disk only; closes on EOF.
//! - [`StreamMode::LiveOnly`] — control socket only; closes when the
//!   dispatcher EOFs the socket. Errors silently (empty stream) when
//!   no socket exists for the run.
//! - [`StreamMode::ReplayThenLive`] — drain disk, then subscribe to the
//!   live socket. Falls back to `ReplayOnly` behavior when the run is
//!   cold (no live socket).
//!
//! The live-socket arms speak the same wire protocol as `pitboss-cli`'s
//! control bridge: client `Hello` → server `Hello` → client
//! `Subscribe { since_seq }` → server pushes `EventEnvelope`s.
//! Envelopes are surfaced as [`RunStreamPayload::Event`] items.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::control_protocol::{resolve_control_socket, EventEnvelope};
use crate::store::record::{RunSummary, TaskRecord};
use crate::store::summary::{read_run_snapshot, RunSnapshotReader};

/// One item in the unified run stream.
///
/// Every item carries a uniform header — `seq` (monotonic per run,
/// dispatcher-assigned post-PR-B) and `ts` (when the dispatcher emitted)
/// — plus a typed [`RunStreamPayload`].
///
/// For `ReplayOnly` mode in this PR, `seq` is populated as a
/// per-stream-instance counter starting at 1. It is **not** the
/// run-wide dispatcher seq yet; PR-A consumers should treat it as an
/// opaque ordering token for de-duplicating across multiple replays.
/// PR-B threads the real dispatcher seq through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStreamItem {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub run_id: Uuid,
    pub payload: RunStreamPayload,
}

/// The payload variants. Future PRs may add `Message`, `Notification`,
/// etc., per the RFC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunStreamPayload {
    /// A finalized task row. Sourced from `summary.jsonl` / `summary.json`
    /// via [`crate::store::summary::read_run_snapshot`].
    Task(Box<TaskRecord>),
    /// Run-level bookend. See [`LifecycleEvent`].
    Lifecycle(LifecycleEvent),
    /// Raw control-socket envelope, as pushed by the dispatcher. Only
    /// emitted by [`StreamMode::LiveOnly`] and the live tail of
    /// [`StreamMode::ReplayThenLive`]. The inner event is held
    /// type-erased ([`serde_json::Value`]) so consumers can typed-parse
    /// downstream without forcing a `pitboss-cli` dependency on this
    /// crate. See [`crate::control_protocol`].
    Event(Box<EventEnvelope>),
}

/// Run-level lifecycle markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LifecycleEvent {
    /// Run started. Reserved — not emitted by `ReplayOnly` in this PR
    /// because `summary.json` doesn't carry a started-only record
    /// independently of the finalized summary. PR-B can emit this
    /// from the live `Hello` envelope.
    Started {
        run_id: Uuid,
        started_at: DateTime<Utc>,
    },
    /// Run finalized. Carries the full [`RunSummary`] so a consumer
    /// that wants only the run summary can short-circuit by reading
    /// the last item of a `ReplayOnly` stream.
    Finalized { summary: Box<RunSummary> },
}

/// Transport selector for [`open_run_stream`].
#[derive(Debug, Clone, Copy)]
pub enum StreamMode {
    /// Replay disk until end-of-stream, never connect to socket.
    /// Use for `pitboss status` / `pitboss diff` / cold-run web pages.
    ReplayOnly,
    /// Drain disk, then subscribe to the live control socket. Falls
    /// back to `ReplayOnly` behavior when no live socket exists (cold
    /// run, finalized).
    ReplayThenLive,
    /// Skip disk; subscribe to socket only. Closes silently (empty
    /// stream) when no socket exists.
    LiveOnly,
}

/// Buffer depth for the cross-task feeder. Bounded so a slow consumer
/// applies backpressure to the disk replay + socket reader rather than
/// leaking memory unboundedly. Tuned to the same scale as
/// [`tail_run_stream`].
const FEEDER_CHANNEL_CAP: usize = 64;

/// Open a unified stream over a run directory.
///
/// Returns an async [`Stream`] of [`RunStreamItem`] values. See the
/// module-level docs and the RFC for semantics.
pub fn open_run_stream(
    run_dir: &Path,
    mode: StreamMode,
) -> impl Stream<Item = RunStreamItem> + Send + 'static {
    let (tx, rx) = mpsc::channel::<RunStreamItem>(FEEDER_CHANNEL_CAP);
    let run_dir = run_dir.to_path_buf();
    tokio::spawn(async move {
        match mode {
            StreamMode::ReplayOnly => {
                drive_replay(&run_dir, &tx).await;
            }
            StreamMode::ReplayThenLive => {
                let next_seq = drive_replay(&run_dir, &tx).await;
                // Best-effort: continue into live transport. Cold-run
                // (no socket) is the silent fallback — the stream ends.
                drive_live(&run_dir, &tx, next_seq).await;
            }
            StreamMode::LiveOnly => {
                drive_live(&run_dir, &tx, 1).await;
            }
        }
    });
    ReceiverStream::new(rx)
}

/// Drain `summary.json[l]` into `tx`. Returns the next seq the live
/// arm should use so the per-stream counter stays monotonic across the
/// disk → live transition.
async fn drive_replay(run_dir: &Path, tx: &mpsc::Sender<RunStreamItem>) -> u64 {
    let items = build_replay_items(run_dir);
    let mut next_seq = items.last().map_or(1, |i| i.seq + 1);
    for item in items {
        next_seq = item.seq + 1;
        if tx.send(item).await.is_err() {
            return next_seq;
        }
    }
    next_seq
}

/// Connect to the run's control socket, perform the Hello + Subscribe
/// handshake, then forward dispatcher envelopes as `Event` items.
///
/// `start_seq` is the per-stream seq counter the live items use, picked
/// up where disk replay left off. Each emitted item's `seq` field comes
/// from this per-stream counter; the dispatcher-assigned seq lives on
/// the inner `EventEnvelope.seq` and is preserved verbatim for
/// consumers that care.
///
/// Transport selection (#474):
/// - If `meta.json` carries `control_tcp_addr`, dial that TCP address
///   first. This is the path that container-dispatch runs on macOS use
///   because the in-container `AF_UNIX` socket isn't reachable from the
///   host through virtiofs.
/// - Otherwise, fall back to the historical `AF_UNIX` path resolved via
///   `resolve_control_socket`.
///
/// Returns silently when no socket exists or the socket EOFs — the
/// caller's stream then ends.
async fn drive_live(run_dir: &Path, tx: &mpsc::Sender<RunStreamItem>, start_seq: u64) {
    use tokio::net::{TcpStream, UnixStream};

    // Run-id is the run-dir's basename (uuid). Used both for socket
    // resolution and for the `RunStreamItem.run_id` field on emitted
    // events.
    let Some(run_id_str) = run_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
    else {
        tracing::debug!(
            "live transport: run_dir has no basename, can't resolve socket: {}",
            run_dir.display()
        );
        return;
    };
    let run_id = Uuid::parse_str(&run_id_str).unwrap_or_else(|_| Uuid::nil());

    if let Some(tcp_addr) = read_control_tcp_addr(run_dir) {
        match TcpStream::connect(&tcp_addr).await {
            Ok(stream) => {
                handshake_and_forward(stream, &run_id_str, run_id, tx, start_seq).await;
            }
            Err(e) => {
                tracing::debug!(run_id = %run_id_str, addr = %tcp_addr, error = %e,
                                "live transport: tcp connect failed");
            }
        }
        return;
    }

    let Some(sock_path) = resolve_control_socket(&run_id_str, run_dir) else {
        tracing::debug!(run_id = %run_id_str, "live transport: no control socket");
        return;
    };

    match UnixStream::connect(&sock_path).await {
        Ok(stream) => {
            handshake_and_forward(stream, &run_id_str, run_id, tx, start_seq).await;
        }
        Err(e) => {
            tracing::debug!(run_id = %run_id_str, error = %e, "live transport: unix connect failed");
        }
    }
}

/// Perform the subscriber-mode Hello + Subscribe handshake, then loop
/// forwarding envelopes from the stream until EOF or a send error.
/// Generic over `S` so the same body serves `AF_UNIX` and TCP streams. (#474)
async fn handshake_and_forward<S>(
    mut stream: S,
    run_id_str: &str,
    run_id: Uuid,
    tx: &mpsc::Sender<RunStreamItem>,
    start_seq: u64,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // Client hello. PR-P of #438: send `"mode":"subscriber"` so the
    // dispatcher skips the writer-slot install — any number of unified-API
    // consumers can attach to a run without displacing each other or a
    // concurrent writer (e.g. `control_bridge.rs::send_op` from pitboss-web).
    let hello = format!(
        r#"{{"op":"hello","client_version":"pitboss-core/{}","mode":"subscriber"}}{}"#,
        crate::VERSION,
        "\n"
    );
    if stream.write_all(hello.as_bytes()).await.is_err() {
        return;
    }
    if stream.flush().await.is_err() {
        return;
    }

    // Client subscribe. Sent immediately after Hello — the wire is
    // full-duplex and the dispatcher processes ops in order, so this
    // doesn't race the server-side Hello reply. `since_seq: 0`
    // requests everything; consumer-side reconciliation in
    // `ReplayThenLive` dedupes against its disk replay.
    let subscribe = "{\"op\":\"subscribe\",\"since_seq\":0}\n";
    if stream.write_all(subscribe.as_bytes()).await.is_err() {
        return;
    }
    if stream.flush().await.is_err() {
        return;
    }

    let (read_half, _write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();

    // Forward envelopes.
    let mut seq = start_seq;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                let envelope: EventEnvelope = match serde_json::from_str(&line) {
                    Ok(e) => e,
                    Err(e) => {
                        // Server may emit non-envelope replies (OpAcked
                        // for the subscribe op, op-replies for future
                        // ops). Best-effort: keep parsing.
                        tracing::debug!(
                            run_id = %run_id_str,
                            error = %e,
                            line = %line,
                            "live transport: envelope parse failed; skipping",
                        );
                        continue;
                    }
                };
                let item = RunStreamItem {
                    seq,
                    ts: Utc::now(),
                    run_id,
                    payload: RunStreamPayload::Event(Box::new(envelope)),
                };
                seq = seq.saturating_add(1);
                if tx.send(item).await.is_err() {
                    return;
                }
            }
            Ok(None) => {
                tracing::debug!(run_id = %run_id_str, "live transport: socket EOF");
                return;
            }
            Err(e) => {
                tracing::debug!(
                    run_id = %run_id_str,
                    error = %e,
                    "live transport: socket read error",
                );
                return;
            }
        }
    }
}

/// Read `meta.json` for the run at `run_dir` and return the optional
/// `control_tcp_addr` field. Returns `None` when meta.json is missing,
/// unparseable, or doesn't carry the field. Used by `drive_live` to
/// decide whether to dial TCP or fall back to `AF_UNIX`. (#474)
fn read_control_tcp_addr(run_dir: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct MetaTcpOnly {
        #[serde(default)]
        control_tcp_addr: Option<String>,
    }
    let bytes = std::fs::read(run_dir.join("meta.json")).ok()?;
    serde_json::from_slice::<MetaTcpOnly>(&bytes)
        .ok()
        .and_then(|m| m.control_tcp_addr)
}

/// Polling interval for [`tail_run_stream`]. The current implementation
/// is poll-only; PR-F can add a notify-driven fast-path that wakes the
/// poll loop on FS events.
const TAIL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Open a *continuous* run stream that replays existing
/// `summary.json[l]` then keeps tailing for new rows.
///
/// PR-E of #438. This is the disk-side analogue of PR-D's
/// `pitboss-cli::stream::open_run_session` socket consumer: a
/// long-lived stream that surfaces new [`RunStreamPayload::Task`] items
/// as the dispatcher appends them to `summary.jsonl`, plus a single
/// [`RunStreamPayload::Lifecycle`] when `summary.json` finalizes.
///
/// The stream terminates when:
/// - the run finalizes (`summary.json` arrives and is parsed), **or**
/// - the receiving side drops the stream.
///
/// Internals: a 250 ms poll loop drives [`RunSnapshotReader::refresh`].
/// New rows are diff'd against a last-emitted `ended_at` map keyed by
/// `task_id` and only fresh or newer rows are emitted. PR-F will layer
/// `notify` on top so events arrive sooner than the next tick.
///
/// `seq` is a per-stream monotonic counter (same caveat as
/// [`open_run_stream`]) until #259 lifts it to per-run lifetime.
pub fn tail_run_stream(run_dir: PathBuf) -> impl Stream<Item = RunStreamItem> + Send + 'static {
    let (tx, rx) = mpsc::channel::<RunStreamItem>(64);
    tokio::spawn(drive_tail(run_dir, tx));
    ReceiverStream::new(rx)
}

/// Async tail loop: poll `RunSnapshotReader`, emit deltas, sleep, repeat.
/// Exits on finalize or when the receiver drops.
async fn drive_tail(run_dir: PathBuf, tx: mpsc::Sender<RunStreamItem>) {
    let mut reader = RunSnapshotReader::new(run_dir.clone());
    let mut emitted: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut seq: u64 = 0;
    let mut finalized = false;

    loop {
        if !emit_diff(
            &run_dir,
            &mut reader,
            &mut emitted,
            &mut seq,
            &mut finalized,
            &tx,
        )
        .await
        {
            return;
        }
        if finalized {
            return;
        }
        // Sleep responsively: wake early if the receiver disappears
        // so the task doesn't sit idle for a tick before noticing.
        tokio::select! {
            () = tokio::time::sleep(TAIL_POLL_INTERVAL) => {}
            () = tx.closed() => return,
        }
    }
}

/// Refresh the reader, diff against `emitted`, and push new items
/// through `tx`. Returns `false` if the receiver dropped (caller
/// should exit).
async fn emit_diff(
    run_dir: &Path,
    reader: &mut RunSnapshotReader,
    emitted: &mut HashMap<String, DateTime<Utc>>,
    seq: &mut u64,
    finalized: &mut bool,
    tx: &mpsc::Sender<RunStreamItem>,
) -> bool {
    let snap = reader.refresh();

    // Resolve a run_id from the finalized summary when available; else
    // fall back to the dir name (uuid-parsed) or `Uuid::nil`.
    let run_id = snap.run_summary.as_ref().map_or_else(
        || {
            run_dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| Uuid::parse_str(n).ok())
                .unwrap_or_else(Uuid::nil)
        },
        |s| s.run_id,
    );

    // Collect newly-seen or newer-than-last-seen tasks.
    let mut fresh: Vec<TaskRecord> = Vec::new();
    for (id, rec) in &snap.tasks {
        let last = emitted.get(id);
        if last.is_none_or(|prev| rec.ended_at > *prev) {
            fresh.push(rec.clone());
        }
    }
    // Emit in `ended_at` order, ties on `task_id` (matches
    // `open_run_stream` semantics).
    fresh.sort_by(|a, b| {
        a.ended_at
            .cmp(&b.ended_at)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });

    for rec in fresh {
        *seq += 1;
        emitted.insert(rec.task_id.clone(), rec.ended_at);
        let item = RunStreamItem {
            seq: *seq,
            ts: rec.ended_at,
            run_id,
            payload: RunStreamPayload::Task(Box::new(rec)),
        };
        if tx.send(item).await.is_err() {
            return false;
        }
    }

    if let Some(summary) = &snap.run_summary {
        if !*finalized {
            *finalized = true;
            *seq += 1;
            let item = RunStreamItem {
                seq: *seq,
                ts: summary.ended_at,
                run_id,
                payload: RunStreamPayload::Lifecycle(LifecycleEvent::Finalized {
                    summary: Box::new(summary.clone()),
                }),
            };
            if tx.send(item).await.is_err() {
                return false;
            }
        }
    }

    true
}

fn build_replay_items(run_dir: &Path) -> Vec<RunStreamItem> {
    let snap = read_run_snapshot(run_dir);

    // Resolve a `run_id` from the finalized summary when available.
    // Cold-run replays without a finalized summary fall back to
    // `Uuid::nil()`; PR-B reads this from `run_meta.json` so live
    // subscribers can address the right socket.
    let run_id = snap
        .run_summary
        .as_ref()
        .map_or_else(Uuid::nil, |s| s.run_id);

    // Emit Task items in a stable order. We sort by `ended_at` so the
    // stream reflects the order tasks reached terminal state — the
    // closest approximation to dispatcher seq for a cold replay. Ties
    // break on `task_id` for determinism.
    let mut tasks: Vec<TaskRecord> = snap.tasks.values().cloned().collect();
    tasks.sort_by(|a, b| {
        a.ended_at
            .cmp(&b.ended_at)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });

    let mut items = Vec::with_capacity(tasks.len() + 1);
    let mut seq: u64 = 1;
    for task in tasks {
        items.push(RunStreamItem {
            seq,
            ts: task.ended_at,
            run_id,
            payload: RunStreamPayload::Task(Box::new(task)),
        });
        seq += 1;
    }

    if let Some(summary) = snap.run_summary {
        items.push(RunStreamItem {
            seq,
            ts: summary.ended_at,
            run_id,
            payload: RunStreamPayload::Lifecycle(LifecycleEvent::Finalized {
                summary: Box::new(summary),
            }),
        });
    }

    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::TokenUsage;
    use crate::store::record::{RunSummary, TaskStatus};
    use chrono::TimeZone;
    use futures_util::StreamExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn rec(task_id: &str, status: TaskStatus, ended_secs: i64) -> TaskRecord {
        let t = Utc.timestamp_opt(1_700_000_000 + ended_secs, 0).unwrap();
        TaskRecord {
            task_id: task_id.into(),
            status,
            exit_code: Some(0),
            started_at: t,
            ended_at: t,
            duration_ms: 0,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            final_message: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
            cost_usd: None,
            actor_type: None,
            terminate_reason: None,
        }
    }

    fn write_jsonl(path: &Path, records: &[TaskRecord]) {
        let lines: Vec<String> = records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    async fn collect(run_dir: &Path, mode: StreamMode) -> Vec<RunStreamItem> {
        open_run_stream(run_dir, mode).collect().await
    }

    #[tokio::test]
    async fn replay_only_empty_run_yields_no_items() {
        let tmp = TempDir::new().unwrap();
        let items = collect(tmp.path(), StreamMode::ReplayOnly).await;
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn replay_only_emits_tasks_in_ended_at_order() {
        let tmp = TempDir::new().unwrap();
        // Write in reverse chronological order to prove the stream
        // re-sorts by ended_at, not by file order.
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("c", TaskStatus::Success, 30),
                rec("a", TaskStatus::Success, 10),
                rec("b", TaskStatus::Failed, 20),
            ],
        );
        let items = collect(tmp.path(), StreamMode::ReplayOnly).await;
        assert_eq!(items.len(), 3);
        let ids: Vec<&str> = items
            .iter()
            .filter_map(|i| match &i.payload {
                RunStreamPayload::Task(t) => Some(t.task_id.as_str()),
                RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_) => None,
            })
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        // Seq is monotonic per stream.
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[1].seq, 2);
        assert_eq!(items[2].seq, 3);
    }

    #[tokio::test]
    async fn replay_only_dedupes_duplicate_task_id_rows() {
        // The #437 motivating bug: reprompt/cancel/respawn writes
        // multiple .jsonl rows for one task_id. The unified stream
        // must emit each task_id at most once.
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("worker-1", TaskStatus::Cancelled, 10),
                rec("worker-1", TaskStatus::Success, 20), // newer row wins
                rec("worker-2", TaskStatus::Failed, 15),
            ],
        );
        let items = collect(tmp.path(), StreamMode::ReplayOnly).await;
        let tasks: Vec<&TaskRecord> = items
            .iter()
            .filter_map(|i| match &i.payload {
                RunStreamPayload::Task(t) => Some(t.as_ref()),
                RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_) => None,
            })
            .collect();
        assert_eq!(tasks.len(), 2, "deduped to two unique task_ids");
        let w1 = tasks.iter().find(|t| t.task_id == "worker-1").unwrap();
        assert!(matches!(w1.status, TaskStatus::Success));
    }

    #[tokio::test]
    async fn replay_only_emits_lifecycle_finalized_when_finalized() {
        let tmp = TempDir::new().unwrap();
        let t = Utc.timestamp_opt(1_700_000_100, 0).unwrap();
        let summary = RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::from("/x.toml"),
            manifest_name: None,
            pitboss_version: "test".into(),
            claude_version: None,
            started_at: t,
            ended_at: t,
            total_duration_ms: 100_000,
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            notify_failures: None,
            tasks: vec![rec("a", TaskStatus::Success, 50)],
            spend_breakdown: None,
        };
        std::fs::write(
            tmp.path().join("summary.json"),
            serde_json::to_vec(&summary).unwrap(),
        )
        .unwrap();

        let items = collect(tmp.path(), StreamMode::ReplayOnly).await;
        assert_eq!(items.len(), 2, "one task + one finalized lifecycle");
        let last_payload = &items.last().unwrap().payload;
        if let RunStreamPayload::Lifecycle(LifecycleEvent::Finalized { summary }) = last_payload {
            assert_eq!(summary.tasks_total, 1);
        } else {
            panic!("expected Finalized lifecycle, got {last_payload:?}");
        }
    }

    #[tokio::test]
    async fn replay_only_no_lifecycle_when_in_progress() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[rec("a", TaskStatus::Success, 10)],
        );
        let items = collect(tmp.path(), StreamMode::ReplayOnly).await;
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].payload, RunStreamPayload::Task(_)));
    }

    /// `LiveOnly` on a cold run (no socket file) closes silently with
    /// an empty stream. This is the documented fallback path — consumers
    /// that observe an empty `LiveOnly` stream know there's no live
    /// dispatcher (either the run finalized or never started). Distinct
    /// from `ReplayOnly`, which still emits the disk-replay items.
    #[tokio::test]
    async fn live_only_on_cold_run_yields_no_items() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[rec("a", TaskStatus::Success, 10)],
        );
        // Point XDG at an empty dir so no stray socket on the host
        // matches by accident.
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));
        let items = collect(tmp.path(), StreamMode::LiveOnly).await;
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert!(items.is_empty(), "LiveOnly with no socket must be empty");
    }

    /// `ReplayThenLive` on a cold run delivers the disk-replay items
    /// and then closes when the live transport can't find a socket.
    /// This is the headline back-compat: callers that switch from
    /// `ReplayOnly` to `ReplayThenLive` see identical behavior on
    /// finalized runs — the live arm is a pure addition that activates
    /// only when a dispatcher is up.
    #[tokio::test]
    async fn replay_then_live_on_cold_run_matches_replay_only() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("a", TaskStatus::Success, 10),
                rec("b", TaskStatus::Failed, 20),
            ],
        );
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));
        let r = collect(tmp.path(), StreamMode::ReplayOnly).await;
        let rl = collect(tmp.path(), StreamMode::ReplayThenLive).await;
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(r.len(), 2);
        assert_eq!(rl.len(), 2);
        // Same task ids in the same order — proves the live arm
        // didn't double-emit or reorder anything.
        let r_ids: Vec<_> = r.iter().filter_map(item_task_id).collect();
        let rl_with_tasks: Vec<_> = rl.iter().filter_map(item_task_id).collect();
        assert_eq!(r_ids, rl_with_tasks);
    }

    fn item_task_id(item: &RunStreamItem) -> Option<&str> {
        match &item.payload {
            RunStreamPayload::Task(t) => Some(t.task_id.as_str()),
            _ => None,
        }
    }

    // ------------------------------------------------------------
    // Live-socket transport — PR-N of #438.
    // ------------------------------------------------------------

    /// Spawn a one-shot mock dispatcher on a unix socket. Speaks the
    /// real handshake (waits for Hello + Subscribe), writes `lines` as
    /// the response envelopes, then closes. Used in lieu of the real
    /// dispatcher so the transport contract can be pinned without
    /// pulling pitboss-cli into pitboss-core's tests.
    fn spawn_mock_dispatcher(socket: &Path, response_lines: Vec<String>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;

        let listener = UnixListener::bind(socket).expect("bind mock socket");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            // Expect client hello.
            let _client_hello = lines.next_line().await;
            // Send server hello.
            let server_hello = "{\"event\":\"hello\",\"server_version\":\"mock\",\"run_id\":\"mock\",\"run_kind\":\"test\",\"workers\":[]}\n";
            let _ = write_half.write_all(server_hello.as_bytes()).await;
            let _ = write_half.flush().await;
            // Expect client subscribe.
            let _subscribe = lines.next_line().await;
            // Push payload envelopes.
            for line in response_lines {
                let with_nl = format!("{line}\n");
                if write_half.write_all(with_nl.as_bytes()).await.is_err() {
                    break;
                }
            }
            let _ = write_half.flush().await;
            // Drop write_half — client sees EOF and ends the stream.
        });
    }

    /// `LiveOnly` against a connected dispatcher forwards every
    /// envelope the server writes — including the server Hello — as a
    /// [`RunStreamPayload::Event`] item. The inner dispatcher-assigned
    /// seq lives on `EventEnvelope.seq`; the outer `RunStreamItem.seq`
    /// advances as a per-stream counter.
    #[tokio::test]
    async fn live_only_forwards_socket_envelopes_as_events() {
        let tmp = TempDir::new().unwrap();
        // Real socket path the unified API will discover via the
        // run-dir fallback (XDG branch turned off below).
        let run_dir = tmp.path().join("019d-fake-run-id");
        std::fs::create_dir_all(&run_dir).unwrap();
        let sock = run_dir.join("control.sock");
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));

        spawn_mock_dispatcher(
            &sock,
            vec![
                r#"{"seq":7,"event":"op_acked","op":"cancel_worker","task_id":"w"}"#.into(),
                r#"{"actor_path":["lead","w"],"seq":8,"event":"worker_failed","task_id":"w","reason":{"kind":"auth_failure"}}"#.into(),
            ],
        );
        // Give the listener a tick to be ready before the consumer
        // dials in — tokio's spawn is eager so this is usually instant,
        // but a single yield closes the race deterministically.
        tokio::task::yield_now().await;

        let items = collect(&run_dir, StreamMode::LiveOnly).await;
        std::env::remove_var("XDG_RUNTIME_DIR");

        // Three items: the mock's server hello + the two response
        // envelopes. The unified API forwards everything verbatim — the
        // hello carries server_version/workers/policy info that typed
        // consumers may want.
        assert_eq!(items.len(), 3, "expected three Event items: {items:#?}");
        // Item 1 — server hello.
        match &items[0].payload {
            RunStreamPayload::Event(env) => {
                assert_eq!(
                    env.event.get("event").and_then(|v| v.as_str()),
                    Some("hello")
                );
            }
            other => panic!("expected hello Event, got {other:?}"),
        }
        // Item 2 — op_acked, dispatcher seq 7.
        match &items[1].payload {
            RunStreamPayload::Event(env) => {
                assert_eq!(env.seq, 7);
                assert_eq!(
                    env.event.get("event").and_then(|v| v.as_str()),
                    Some("op_acked")
                );
            }
            other => panic!("expected Event, got {other:?}"),
        }
        // Item 3 — worker_failed, dispatcher seq 8, with actor_path.
        match &items[2].payload {
            RunStreamPayload::Event(env) => {
                assert_eq!(env.seq, 8);
                assert_eq!(env.actor_path.0, vec!["lead", "w"]);
                assert_eq!(
                    env.event.get("event").and_then(|v| v.as_str()),
                    Some("worker_failed")
                );
            }
            other => panic!("expected Event, got {other:?}"),
        }
        // Per-stream seq is strictly monotonic across the three items.
        assert!(items[0].seq < items[1].seq);
        assert!(items[1].seq < items[2].seq);
    }

    /// PR-P of #438: the unified-API client identifies itself as
    /// `"mode":"subscriber"` so the dispatcher skips the writer-slot
    /// install — any number of `open_run_stream` consumers can attach
    /// concurrently without displacing a writer like
    /// `pitboss-web::control_bridge`. Pins the client-side handshake
    /// shape so a regression in `drive_live`'s Hello formatting is
    /// caught here rather than at the (one-way coupled) integration
    /// boundary.
    #[tokio::test]
    async fn live_only_handshake_sends_subscriber_mode() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;
        use tokio::sync::oneshot;

        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("019d-fake-run-id");
        std::fs::create_dir_all(&run_dir).unwrap();
        let sock = run_dir.join("control.sock");
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));

        let (hello_tx, hello_rx) = oneshot::channel::<String>();
        let listener = UnixListener::bind(&sock).expect("bind capture socket");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read_half, mut write_half) = stream.into_split();
            let mut lines = BufReader::new(read_half).lines();
            let client_hello = lines.next_line().await.ok().flatten().unwrap_or_default();
            let _ = hello_tx.send(client_hello);
            let server_hello = "{\"event\":\"hello\",\"server_version\":\"mock\",\"run_id\":\"mock\",\"run_kind\":\"test\",\"workers\":[]}\n";
            let _ = write_half.write_all(server_hello.as_bytes()).await;
            let _ = write_half.flush().await;
            // Eat the Subscribe and exit; closing the write half ends
            // the consumer's stream so `collect` returns deterministically.
            let _ = lines.next_line().await;
        });
        tokio::task::yield_now().await;

        let _items = collect(&run_dir, StreamMode::LiveOnly).await;
        std::env::remove_var("XDG_RUNTIME_DIR");

        let captured = hello_rx.await.expect("hello captured");
        assert!(
            captured.contains("\"op\":\"hello\""),
            "missing op tag in hello: {captured}"
        );
        assert!(
            captured.contains("\"mode\":\"subscriber\""),
            "drive_live must send subscriber mode: {captured}"
        );
    }

    /// `ReplayThenLive` against a connected dispatcher emits the disk
    /// items first, then the live envelopes — proving the disk →
    /// live transition is a continuation rather than a parallel stream.
    #[tokio::test]
    async fn replay_then_live_emits_disk_then_socket() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("019d-fake-run-id");
        std::fs::create_dir_all(&run_dir).unwrap();
        write_jsonl(
            &run_dir.join("summary.jsonl"),
            &[rec("seed", TaskStatus::Success, 10)],
        );
        let sock = run_dir.join("control.sock");
        std::env::set_var("XDG_RUNTIME_DIR", tmp.path().join("xdg-empty"));

        spawn_mock_dispatcher(&sock, vec![r#"{"seq":3,"event":"superseded"}"#.into()]);
        tokio::task::yield_now().await;

        let items = collect(&run_dir, StreamMode::ReplayThenLive).await;
        std::env::remove_var("XDG_RUNTIME_DIR");

        // Disk task + server hello + the response envelope.
        assert_eq!(
            items.len(),
            3,
            "expected disk task + 2 live events: {items:#?}"
        );
        assert!(matches!(items[0].payload, RunStreamPayload::Task(_)));
        // First live item is the mock's server hello.
        assert!(matches!(&items[1].payload, RunStreamPayload::Event(env)
            if env.event.get("event").and_then(|v| v.as_str()) == Some("hello")));
        // Second live item is the payload envelope from the mock.
        match &items[2].payload {
            RunStreamPayload::Event(env) => {
                assert_eq!(env.seq, 3);
                assert_eq!(
                    env.event.get("event").and_then(|v| v.as_str()),
                    Some("superseded")
                );
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    // ------------------------------------------------------------
    // tail_run_stream — PR-E of #438.
    // ------------------------------------------------------------

    fn append_record(path: &Path, r: &TaskRecord) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{}", serde_json::to_string(r).unwrap()).unwrap();
        f.sync_all().unwrap();
    }

    /// Pre-existing rows are replayed immediately; new rows appended
    /// after the stream is open arrive as additional Task items.
    #[tokio::test]
    async fn tail_emits_initial_replay_and_then_new_rows() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        append_record(&jsonl, &rec("seed", TaskStatus::Success, 10));

        let stream = tail_run_stream(tmp.path().to_path_buf());
        let mut stream = std::pin::pin!(stream);

        // First item — the existing row.
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .expect("seed item");
        match first.payload {
            RunStreamPayload::Task(t) => assert_eq!(t.task_id, "seed"),
            other @ (RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_)) => {
                panic!("expected Task, got {other:?}");
            }
        }

        // Append a fresh row; the watcher (or fallback timer) should
        // deliver it within the safety-net cadence.
        append_record(&jsonl, &rec("fresh", TaskStatus::Success, 20));

        // The fallback poll is 250ms when notify is dead; the notify
        // case fires almost immediately. Allow up to 3s for either.
        let next = tokio::time::timeout(std::time::Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .expect("fresh item");
        match next.payload {
            RunStreamPayload::Task(t) => assert_eq!(t.task_id, "fresh"),
            other @ (RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_)) => {
                panic!("expected Task, got {other:?}");
            }
        }

        // Seqs must be monotonic.
        assert!(next.seq > first.seq);
    }

    /// When `summary.json` finalizes, the tail emits a single
    /// `Lifecycle::Finalized` and the stream ends.
    #[tokio::test]
    async fn tail_terminates_on_finalize() {
        let tmp = TempDir::new().unwrap();
        let t = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let summary = RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::from("/x.toml"),
            manifest_name: None,
            pitboss_version: "test".into(),
            claude_version: None,
            started_at: t,
            ended_at: t,
            total_duration_ms: 0,
            tasks_total: 0,
            tasks_failed: 0,
            was_interrupted: false,
            notify_failures: None,
            tasks: vec![],
            spend_breakdown: None,
        };
        std::fs::write(
            tmp.path().join("summary.json"),
            serde_json::to_vec(&summary).unwrap(),
        )
        .unwrap();

        let stream = tail_run_stream(tmp.path().to_path_buf());
        let items: Vec<_> = stream.collect().await;
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0].payload,
            RunStreamPayload::Lifecycle(LifecycleEvent::Finalized { .. })
        ));
    }

    /// A task that appears with a newer `ended_at` (reprompt/respawn
    /// lifecycle) is re-emitted; same-or-older rows are deduped.
    #[tokio::test]
    async fn tail_re_emits_task_when_ended_at_advances() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        append_record(&jsonl, &rec("w", TaskStatus::Cancelled, 10));

        let stream = tail_run_stream(tmp.path().to_path_buf());
        let mut stream = std::pin::pin!(stream);

        // First — Cancelled.
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .expect("first item");
        match first.payload {
            RunStreamPayload::Task(t) => {
                assert_eq!(t.task_id, "w");
                assert!(matches!(t.status, TaskStatus::Cancelled));
            }
            other @ (RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_)) => {
                panic!("expected Task, got {other:?}");
            }
        }

        // Append same task_id with a later ended_at and Success status
        // (reprompt-success lifecycle).
        append_record(&jsonl, &rec("w", TaskStatus::Success, 25));

        let next = tokio::time::timeout(std::time::Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .expect("second item");
        match next.payload {
            RunStreamPayload::Task(t) => {
                assert_eq!(t.task_id, "w");
                assert!(matches!(t.status, TaskStatus::Success));
            }
            other @ (RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_)) => {
                panic!("expected Task, got {other:?}");
            }
        }
        assert!(next.seq > first.seq);
    }

    /// Item shape round-trips through JSON — necessary for the future
    /// wire format that PR-B layers on top.
    #[tokio::test]
    async fn run_stream_item_round_trips_json() {
        let item = RunStreamItem {
            seq: 42,
            ts: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            run_id: Uuid::now_v7(),
            payload: RunStreamPayload::Task(Box::new(rec("t", TaskStatus::Success, 0))),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: RunStreamItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seq, 42);
        match back.payload {
            RunStreamPayload::Task(t) => assert_eq!(t.task_id, "t"),
            RunStreamPayload::Lifecycle(_) | RunStreamPayload::Event(_) => {
                panic!("payload variant changed across round-trip");
            }
        }
    }
}
