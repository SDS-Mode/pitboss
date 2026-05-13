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
//! Status: foundation. This PR ships [`StreamMode::ReplayOnly`] only;
//! [`StreamMode::ReplayThenLive`] and [`StreamMode::LiveOnly`] return
//! the same `ReplayOnly` stream until PR-B wires the live transport
//! (`Subscribe { since_seq }` op + dispatcher-assigned `seq` on
//! `EventEnvelope`).
//!
//! The [`RunStreamPayload::Event`] arm is reserved for PR-B as well.
//! Today's [`RunStreamPayload`] carries only [`RunStreamPayload::Task`]
//! and [`RunStreamPayload::Lifecycle`] — adding a new variant later
//! is a non-breaking change for the in-tree consumers that exist in
//! this PR (only tests).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

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

/// The payload variants. Extended in subsequent PRs:
/// PR-B adds `Event(EventEnvelope)`; future PRs add `Message`,
/// `Notification`, etc., per the RFC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunStreamPayload {
    /// A finalized task row. Sourced from `summary.jsonl` / `summary.json`
    /// via [`crate::store::summary::read_run_snapshot`].
    Task(Box<TaskRecord>),
    /// Run-level bookend. See [`LifecycleEvent`].
    Lifecycle(LifecycleEvent),
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
///
/// In this PR (PR-A), all three modes behave identically — they all
/// return a `ReplayOnly` stream. PR-B implements `ReplayThenLive`
/// (replay disk to last seen seq, then subscribe to the live socket)
/// and `LiveOnly` (skip disk, subscribe to socket only).
#[derive(Debug, Clone, Copy)]
pub enum StreamMode {
    /// Replay disk until end-of-stream, never connect to socket.
    /// Use for `pitboss status` / `pitboss diff` / cold-run web pages.
    ReplayOnly,
    /// Replay disk, then attempt socket subscribe. Returns `ReplayOnly`
    /// behavior for cold (finalized) runs.
    ReplayThenLive,
    /// Skip disk; subscribe to socket only. Errors if the socket
    /// doesn't exist.
    LiveOnly,
}

/// Open a unified stream over a run directory.
///
/// Returns an async [`Stream`] of [`RunStreamItem`] values. See the
/// module-level docs and the RFC for semantics.
///
/// # Mode semantics in PR-A
///
/// All three [`StreamMode`] variants currently return the same
/// `ReplayOnly` stream. The other modes get their real transports
/// in PR-B. Callers can already pick the mode that matches their
/// intent — once PR-B lands, the behavior changes transparently.
pub fn open_run_stream(
    run_dir: &Path,
    _mode: StreamMode,
) -> impl Stream<Item = RunStreamItem> + Send + 'static {
    let items = build_replay_items(run_dir);
    tokio_stream::iter(items)
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
                RunStreamPayload::Lifecycle(_) => None,
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
                RunStreamPayload::Lifecycle(_) => None,
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

    /// In PR-A, all three modes alias to `ReplayOnly` behavior so that
    /// callers can already write to the final API. PR-B differentiates.
    #[tokio::test]
    async fn all_modes_behave_as_replay_only_in_pr_a() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("a", TaskStatus::Success, 10),
                rec("b", TaskStatus::Failed, 20),
            ],
        );
        let r = collect(tmp.path(), StreamMode::ReplayOnly).await;
        let rl = collect(tmp.path(), StreamMode::ReplayThenLive).await;
        let l = collect(tmp.path(), StreamMode::LiveOnly).await;
        assert_eq!(r.len(), rl.len());
        assert_eq!(r.len(), l.len());
        assert_eq!(r.len(), 2);
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
            other @ RunStreamPayload::Lifecycle(_) => panic!("expected Task, got {other:?}"),
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
            other @ RunStreamPayload::Lifecycle(_) => panic!("expected Task, got {other:?}"),
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
            other @ RunStreamPayload::Lifecycle(_) => panic!("expected Task, got {other:?}"),
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
            other @ RunStreamPayload::Lifecycle(_) => panic!("expected Task, got {other:?}"),
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
            RunStreamPayload::Lifecycle(_) => panic!("payload variant changed across round-trip"),
        }
    }
}
