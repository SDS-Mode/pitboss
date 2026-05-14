//! Live + historical unified run-stream consumer.
//!
//! Design RFC: `book/src/architecture/unified-envelope-api.md`
//! Issue: <https://github.com/SDS-Mode/pitboss/issues/438>
//!
//! This is the consumer-side composition that combines
//! [`pitboss_core::stream`]'s disk-replay foundation (PR-A) with the
//! per-run control socket's live envelope stream (PR-B's `seq` plumbing)
//! into one [`LiveStreamItem`] channel. It is the substrate the TUI
//! cutover (PR-D) will sit on; web SSE and CLI one-shots follow.
//!
//! ## Where this lives
//!
//! The RFC put the unified API in `pitboss-core::stream`. We can't quite
//! deliver that today because [`EventEnvelope`] depends on
//! `dispatch::actor::ActorPath` and `mcp::policy::ApprovalRule`, both
//! `pitboss-cli`-only types. Moving them down to `pitboss-core` is its
//! own refactor; for now the live consumer lives **here** in
//! `pitboss-cli::stream` and `pitboss-core::stream` exposes only the
//! disk side. The two compose via [`From<RunStreamItem>`] for
//! [`LiveStreamItem`].
//!
//! ## What about `Subscribe { since_seq }`?
//!
//! PR-B deferred the `Subscribe { since_seq }` control-op until #259
//! lands `events.jsonl` persistence (the dispatcher has no in-memory
//! broadcast buffer to fast-forward against today). The live-side
//! subscription here therefore starts fresh on every connect; the
//! per-connection `seq` from PR-B is monotonic within a connection
//! but does not bridge a reconnect.
//!
//! Consumers in `ReplayThenLive` mode see this as: disk replay completes
//! (items carry per-stream seq), then live items begin (seq=1 from the
//! fresh socket connection). The seq spaces are disjoint. A consumer
//! that cares about ordering across the seam should sort items by `ts`
//! rather than `seq` until #259 enables a unified counter.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::stream::Stream;
use pitboss_core::store::record::TaskRecord;
use pitboss_core::stream::{
    open_run_stream, LifecycleEvent, RunStreamItem, RunStreamPayload, StreamMode,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::control::protocol::{ControlOp, EventEnvelope};

/// One item in the unified live+historical stream. Header mirrors
/// [`RunStreamItem`]; the payload adds the [`Event`](LiveStreamPayload::Event)
/// arm for live socket envelopes.
#[derive(Debug, Clone)]
pub struct LiveStreamItem {
    /// For disk-sourced items this is the per-stream seq from
    /// [`RunStreamItem::seq`]; for socket-sourced items this is the
    /// per-connection seq from [`EventEnvelope::seq`]. The two spaces
    /// are disjoint until #259 unifies them — see module docs.
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub run_id: Uuid,
    pub payload: LiveStreamPayload,
}

#[derive(Debug, Clone)]
pub enum LiveStreamPayload {
    /// A deduplicated task record from `summary.json[l]`.
    Task(Box<TaskRecord>),
    /// Run-level bookend. See [`LifecycleEvent`].
    Lifecycle(LifecycleEvent),
    /// A live control-socket envelope as the dispatcher emitted it.
    Event(Box<EventEnvelope>),
}

impl From<RunStreamItem> for LiveStreamItem {
    fn from(item: RunStreamItem) -> Self {
        let payload = match item.payload {
            RunStreamPayload::Task(t) => LiveStreamPayload::Task(t),
            RunStreamPayload::Lifecycle(l) => LiveStreamPayload::Lifecycle(l),
            // PR-N of #438 added `Event` to `pitboss-core`'s
            // `RunStreamPayload`. This crate's `open_live_run_stream`
            // only feeds `StreamMode::ReplayOnly` items through this
            // conversion (the live socket path is handled by
            // `drive_socket` below, which builds typed envelopes
            // directly). So an `Event` arriving here would be a bug in
            // a future caller — log it and skip rather than panic.
            RunStreamPayload::Event(_) => {
                tracing::warn!(
                    "live stream: unexpected RunStreamPayload::Event from disk arm; skipping",
                );
                return LiveStreamItem {
                    seq: item.seq,
                    ts: item.ts,
                    run_id: item.run_id,
                    payload: LiveStreamPayload::Lifecycle(LifecycleEvent::Started {
                        run_id: item.run_id,
                        started_at: item.ts,
                    }),
                };
            }
        };
        LiveStreamItem {
            seq: item.seq,
            ts: item.ts,
            run_id: item.run_id,
            payload,
        }
    }
}

/// Transport selector for [`open_live_run_stream`].
#[derive(Debug, Clone, Copy)]
pub enum LiveStreamMode {
    /// Replay disk until end-of-stream, never connect to socket.
    /// Equivalent to [`pitboss_core::stream::StreamMode::ReplayOnly`]
    /// with a [`LiveStreamItem`]-shaped output.
    ReplayOnly,
    /// Replay disk, then attempt socket subscribe. Cold (finalized)
    /// runs behave like `ReplayOnly`. Live runs replay everything on
    /// disk first, then switch to socket — see module docs for the
    /// seam semantics.
    ReplayThenLive,
    /// Skip disk; subscribe to socket only. Errors silently (empty
    /// stream) if `socket_path` is `None` or the socket isn't there.
    LiveOnly,
}

/// Open a unified live+historical run stream.
///
/// `run_dir` is the run's artifact directory under
/// `~/.local/share/pitboss/runs/<run-id>/`. `socket_path` is the
/// dispatcher's control socket; pass `None` to force `ReplayOnly`-style
/// behavior (the socket arm is skipped).
///
/// The returned stream completes when:
/// - `ReplayOnly`: disk replay exhausts.
/// - `LiveOnly`: socket closes (run finalizes or dispatcher exits).
/// - `ReplayThenLive`: disk replay exhausts and then socket closes.
///
/// Consumers should treat the stream as best-effort: socket connection
/// failures translate to an early end-of-stream rather than an error,
/// matching the existing `pitboss-tui::control` behavior.
pub fn open_live_run_stream(
    run_dir: PathBuf,
    socket_path: Option<PathBuf>,
    mode: LiveStreamMode,
) -> impl Stream<Item = LiveStreamItem> + Send + 'static {
    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        if matches!(
            mode,
            LiveStreamMode::ReplayOnly | LiveStreamMode::ReplayThenLive
        ) {
            let disk = open_run_stream(&run_dir, StreamMode::ReplayOnly);
            let mut disk = std::pin::pin!(disk);
            while let Some(item) = disk.next().await {
                if tx.send(item.into()).await.is_err() {
                    return;
                }
            }
        }

        if matches!(
            mode,
            LiveStreamMode::LiveOnly | LiveStreamMode::ReplayThenLive
        ) {
            if let Some(sock) = socket_path {
                drive_socket(sock, tx.clone()).await;
            }
        }
    });

    ReceiverStream::new(rx)
}

/// Connect to a control socket, send Hello, and forward every received
/// envelope as a [`LiveStreamItem::Event`]. Returns when the socket
/// EOFs or the channel closes. Failures (refused connection, parse
/// errors) terminate quietly — the consumer sees end-of-stream.
async fn drive_socket(socket_path: PathBuf, tx: mpsc::Sender<LiveStreamItem>) {
    let Ok(stream) = UnixStream::connect(&socket_path).await else {
        return;
    };
    let (r, mut w) = stream.into_split();

    if send_hello(&mut w).await.is_err() {
        return;
    }

    drive_socket_reads(r, tx).await;
}

/// Write a `Hello` op to a fresh socket connection. The dispatcher's
/// server requires this as the first message; without it the connection
/// is dropped before any envelopes are emitted.
async fn send_hello(w: &mut OwnedWriteHalf) -> Result<()> {
    let hello = ControlOp::Hello {
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        mode: crate::control::protocol::ClientMode::default(),
    };
    let mut line = serde_json::to_string(&hello)?;
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// Pump envelopes from a pre-opened read half into `tx` as
/// [`LiveStreamItem::Event`] items. Used by both [`drive_socket`] (one-
/// shot stream consumer) and [`open_run_session`] (session that exposes
/// a writer alongside the stream).
async fn drive_socket_reads(r: OwnedReadHalf, tx: mpsc::Sender<LiveStreamItem>) {
    let mut reader = BufReader::new(r).lines();
    while let Ok(Some(text)) = reader.next_line().await {
        let envelope: EventEnvelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    line = text,
                    "live stream: skipping unparseable envelope",
                );
                continue;
            }
        };
        let item = LiveStreamItem {
            seq: envelope.seq,
            ts: Utc::now(),
            // Live envelopes don't carry a `run_id`. Consumers that
            // need one must pass it in via context — for the TUI this
            // is fine because the run-id is known at attach time.
            run_id: Uuid::nil(),
            payload: LiveStreamPayload::Event(Box::new(envelope)),
        };
        if tx.send(item).await.is_err() {
            return;
        }
    }
}

// ----------------------------------------------------------------------------
// Session API — for consumers that need to write ops in addition to read
// envelopes. Used by the TUI cutover (PR-D of #438). The dispatcher allows
// only one client connection per socket, so the read and write halves must
// share one connection.
// ----------------------------------------------------------------------------

/// A run-stream session that owns one socket connection and exposes its
/// read half as a [`Stream`] of [`LiveStreamItem`] and its write half via
/// [`RunStreamSession::writer`]. Use [`open_run_session`] to construct.
pub struct RunStreamSession {
    /// The unified envelope stream. Items arrive in mode order:
    /// `ReplayOnly` / `ReplayThenLive` drain disk first; live items
    /// follow (or are the only output in `LiveOnly`).
    pub stream: ReceiverStream<LiveStreamItem>,
    /// Writer over the control socket, present iff a live socket
    /// connection was established. Use [`RunStreamWriter::send_op`].
    pub writer: Option<Arc<RunStreamWriter>>,
}

/// Owning handle to a control-socket writer. Cloneable via `Arc`. Drop
/// or task abort tears down the underlying write half.
#[derive(Debug)]
pub struct RunStreamWriter {
    inner: Mutex<OwnedWriteHalf>,
}

impl RunStreamWriter {
    /// Send a single control op as one JSON line. Fails if the socket
    /// is closed or the write errors.
    pub async fn send_op(&self, op: &ControlOp) -> Result<()> {
        let mut line = serde_json::to_string(op)?;
        line.push('\n');
        let mut guard = self.inner.lock().await;
        guard.write_all(line.as_bytes()).await?;
        guard.flush().await?;
        Ok(())
    }
}

/// Open a unified run-stream session that exposes both a read stream
/// and (when a live socket is available) a writer.
///
/// Unlike [`open_live_run_stream`], this function is **async** —
/// the socket connection happens before it returns, so callers receive
/// the writer (or `None` for disconnected sessions) without waiting on
/// a background task. The stream's items still flow asynchronously.
///
/// `socket_path = None` or a missing socket file → writer is `None`
/// and the live arm is skipped (effectively `ReplayOnly` regardless of
/// `mode`). This matches the existing fail-soft behavior of the
/// TUI's `ControlClient::connect`.
pub async fn open_run_session(
    run_dir: PathBuf,
    socket_path: Option<PathBuf>,
    mode: LiveStreamMode,
) -> RunStreamSession {
    let (tx, rx) = mpsc::channel(64);

    // Try to establish the live socket up front so the writer is
    // available before this function returns. ReplayOnly skips this.
    let live_state = if matches!(
        mode,
        LiveStreamMode::LiveOnly | LiveStreamMode::ReplayThenLive
    ) {
        if let Some(sock) = socket_path {
            connect_session_socket(&sock).await
        } else {
            None
        }
    } else {
        None
    };

    let writer = live_state.as_ref().map(|(_, w)| w.clone());

    // Spawn the drainers in order: disk first (if applicable), then
    // socket reads.
    tokio::spawn(async move {
        if matches!(
            mode,
            LiveStreamMode::ReplayOnly | LiveStreamMode::ReplayThenLive
        ) {
            let disk = open_run_stream(&run_dir, StreamMode::ReplayOnly);
            let mut disk = std::pin::pin!(disk);
            while let Some(item) = disk.next().await {
                if tx.send(item.into()).await.is_err() {
                    return;
                }
            }
        }
        if let Some((read_half, _writer)) = live_state {
            drive_socket_reads(read_half, tx).await;
        }
    });

    RunStreamSession {
        stream: ReceiverStream::new(rx),
        writer,
    }
}

/// Try to open a control socket, send Hello, and split into read+write
/// halves wrapped for session use. Returns `None` on connect/handshake
/// failure (fail-soft).
async fn connect_session_socket(
    socket_path: &std::path::Path,
) -> Option<(OwnedReadHalf, Arc<RunStreamWriter>)> {
    let stream = UnixStream::connect(socket_path).await.ok()?;
    let (read_half, mut write_half) = stream.into_split();
    send_hello(&mut write_half).await.ok()?;
    let writer = Arc::new(RunStreamWriter {
        inner: Mutex::new(write_half),
    });
    Some((read_half, writer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitboss_core::parser::TokenUsage;
    use pitboss_core::store::record::TaskStatus;
    use std::path::Path;
    use tempfile::TempDir;

    fn rec(task_id: &str, status: TaskStatus, ended_secs: i64) -> TaskRecord {
        let t = Utc::now() - chrono::Duration::seconds(1000 - ended_secs);
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
        }
    }

    fn write_jsonl(path: &Path, records: &[TaskRecord]) {
        let lines: Vec<String> = records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    async fn collect_items(
        run_dir: PathBuf,
        socket_path: Option<PathBuf>,
        mode: LiveStreamMode,
    ) -> Vec<LiveStreamItem> {
        let stream = open_live_run_stream(run_dir, socket_path, mode);
        stream.collect().await
    }

    #[tokio::test]
    async fn replay_only_lifts_disk_items() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("a", TaskStatus::Success, 10),
                rec("b", TaskStatus::Failed, 20),
            ],
        );
        let items = collect_items(tmp.path().to_path_buf(), None, LiveStreamMode::ReplayOnly).await;
        assert_eq!(items.len(), 2);
        for item in &items {
            assert!(matches!(item.payload, LiveStreamPayload::Task(_)));
        }
    }

    #[tokio::test]
    async fn replay_only_ignores_socket_path() {
        let tmp = TempDir::new().unwrap();
        let bogus_sock = tmp.path().join("nonexistent.sock");
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[rec("a", TaskStatus::Success, 10)],
        );
        let items = collect_items(
            tmp.path().to_path_buf(),
            Some(bogus_sock),
            LiveStreamMode::ReplayOnly,
        )
        .await;
        assert_eq!(items.len(), 1, "socket path is ignored in ReplayOnly");
    }

    #[tokio::test]
    async fn live_only_empty_when_socket_missing() {
        let tmp = TempDir::new().unwrap();
        let bogus_sock = tmp.path().join("nope.sock");
        let items = collect_items(
            tmp.path().to_path_buf(),
            Some(bogus_sock),
            LiveStreamMode::LiveOnly,
        )
        .await;
        assert!(items.is_empty(), "no socket → empty stream");
    }

    #[tokio::test]
    async fn replay_then_live_drains_disk_before_socket() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("a", TaskStatus::Success, 10),
                rec("b", TaskStatus::Failed, 20),
            ],
        );
        // No socket — ReplayThenLive should still yield the disk items
        // and then terminate (the socket arm is a no-op).
        let items = collect_items(
            tmp.path().to_path_buf(),
            None,
            LiveStreamMode::ReplayThenLive,
        )
        .await;
        assert_eq!(items.len(), 2);
    }

    #[tokio::test]
    async fn run_stream_item_lifts_into_live() {
        let item = RunStreamItem {
            seq: 7,
            ts: Utc::now(),
            run_id: Uuid::now_v7(),
            payload: RunStreamPayload::Task(Box::new(rec("t", TaskStatus::Success, 0))),
        };
        let lifted: LiveStreamItem = item.into();
        assert_eq!(lifted.seq, 7);
        assert!(matches!(lifted.payload, LiveStreamPayload::Task(_)));
    }
}
