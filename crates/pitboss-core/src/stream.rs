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

use std::path::Path;

use chrono::{DateTime, Utc};
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::record::{RunSummary, TaskRecord};
use crate::store::summary::read_run_snapshot;

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
        match &items.last().unwrap().payload {
            RunStreamPayload::Lifecycle(LifecycleEvent::Finalized { summary }) => {
                assert_eq!(summary.tasks_total, 1);
            }
            other => panic!("expected Finalized lifecycle, got {other:?}"),
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
