//! Canonical reader for a run directory's summary artifacts.
//!
//! Before this module existed, every reader surface — the TUI watcher,
//! the TUI launcher, `pitboss status`, `pitboss diff`, `pitboss list`,
//! the dispatcher's hierarchical finalize aggregator, the pitboss-web
//! `/api/runs/:id/tasks/:task_id` handler, and the insights aggregator
//! — implemented its own `summary.json` / `summary.jsonl` parser. Three
//! different merge strategies coexisted; only one of them deduplicated
//! by `task_id`. The SPA-side fix in #429 covered the JS reader; this
//! module unifies the Rust readers behind one shape so the same class
//! of bug cannot recur. See #437 for the empirical drift catalogue.
//!
//! [`read_run_snapshot`] is the entry point. Surfaces that need a
//! per-task view consume [`RunSnapshot::tasks`]; those that need the
//! finalized `RunSummary` consume [`RunSnapshot::run_summary`].

use std::collections::HashMap;
use std::path::Path;

use crate::store::record::{RunSummary, TaskRecord, TaskStatus};

/// Merged, deduplicated view of a run's task state.
///
/// Built by [`read_run_snapshot`] from a run directory's
/// `summary.json` (finalized, optional) and `summary.jsonl`
/// (append-only, always present once `init_run` ran). Merge rules:
///
/// 1. `summary.json`, when present and parseable, seeds [`Self::tasks`]
///    from its `tasks` array.
/// 2. `summary.jsonl` rows are overlaid with **last-wins by `task_id`**.
///    Reprompt / cancel / respawn lifecycles append multiple rows for
///    one `task_id`; the freshest row is canonical. This is the same
///    semantic the `SvelteKit` SPA adopted in #429.
/// 3. Truncated trailing lines and malformed JSON are dropped with a
///    `tracing::warn`. The read never errors on parse failure.
/// 4. Missing files are treated as empty inputs. A brand-new run dir
///    (after `init_run` but before any worker has finished) yields a
///    default-constructed snapshot.
#[derive(Debug, Clone, Default)]
pub struct RunSnapshot {
    /// The finalized `summary.json` payload when present; `None` for
    /// in-progress, aborted, or interrupted runs.
    pub run_summary: Option<RunSummary>,
    /// All task records, keyed by `task_id`, after merge + dedup.
    pub tasks: HashMap<String, TaskRecord>,
}

impl RunSnapshot {
    /// Whether `summary.json` was found and parsed cleanly.
    #[must_use]
    pub fn is_finalized(&self) -> bool {
        self.run_summary.is_some()
    }

    /// Total deduplicated task count.
    #[must_use]
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Count of tasks not in [`TaskStatus::Success`].
    #[must_use]
    pub fn failed_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|t| !matches!(t.status, TaskStatus::Success))
            .count()
    }

    /// Look up a single record by `task_id`.
    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<&TaskRecord> {
        self.tasks.get(task_id)
    }
}

/// Read the canonical run snapshot from a run directory.
///
/// All surfaces (TUI, web, CLI) should call this rather than rolling
/// their own parser. Synchronous: async callers can wrap in
/// `tokio::task::spawn_blocking` when the run dir lives on a slow
/// filesystem; for typical `~/.local/share/pitboss/runs/<run-id>/`
/// volumes the files are small enough that direct calls from axum
/// handlers are acceptable.
#[must_use]
pub fn read_run_snapshot(run_dir: &Path) -> RunSnapshot {
    let mut tasks = HashMap::new();
    let run_summary = load_summary_json(&run_dir.join("summary.json"), &mut tasks);
    overlay_summary_jsonl(&run_dir.join("summary.jsonl"), &mut tasks);
    RunSnapshot { run_summary, tasks }
}

fn load_summary_json(path: &Path, tasks: &mut HashMap<String, TaskRecord>) -> Option<RunSummary> {
    let bytes = std::fs::read(path).ok()?;
    let summary: RunSummary = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "summary.json: failed to parse; falling back to summary.jsonl only",
            );
            return None;
        }
    };
    for rec in &summary.tasks {
        tasks.insert(rec.task_id.clone(), rec.clone());
    }
    Some(summary)
}

fn overlay_summary_jsonl(path: &Path, tasks: &mut HashMap<String, TaskRecord>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TaskRecord>(trimmed) {
            Ok(rec) => {
                tasks.insert(rec.task_id.clone(), rec);
            }
            Err(e) => tracing::warn!(
                error = %e,
                line = trimmed,
                path = %path.display(),
                "summary.jsonl: skipping unparseable line",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::TokenUsage;
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn rec(task_id: &str, status: TaskStatus) -> TaskRecord {
        let t = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
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

    #[test]
    fn empty_dir_yields_default_snapshot() {
        let tmp = TempDir::new().unwrap();
        let snap = read_run_snapshot(tmp.path());
        assert!(!snap.is_finalized());
        assert_eq!(snap.task_count(), 0);
        assert_eq!(snap.failed_count(), 0);
    }

    #[test]
    fn jsonl_only_in_progress_run() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
        );
        let snap = read_run_snapshot(tmp.path());
        assert!(!snap.is_finalized());
        assert_eq!(snap.task_count(), 2);
        assert_eq!(snap.failed_count(), 1);
        assert!(snap.get("a").is_some());
    }

    /// The motivating bug from #437: reprompt / cancel / respawn writes
    /// multiple `.jsonl` rows for one `task_id`. Last wins. Task counts
    /// must reflect the unique actor set, not the line count.
    #[test]
    fn jsonl_duplicate_task_id_last_wins() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                rec("worker-1", TaskStatus::Cancelled),
                rec("worker-1", TaskStatus::Success), // newer row wins
                rec("worker-2", TaskStatus::Failed),
            ],
        );
        let snap = read_run_snapshot(tmp.path());
        assert_eq!(snap.task_count(), 2, "dedupe by task_id");
        assert_eq!(snap.failed_count(), 1, "only worker-2 stays Failed");
        assert!(matches!(
            snap.get("worker-1").unwrap().status,
            TaskStatus::Success
        ));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let mut bytes = serde_json::to_string(&rec("a", TaskStatus::Success)).unwrap();
        bytes.push('\n');
        bytes.push_str("{not valid json}\n");
        bytes.push('\n'); // blank line
        bytes.push_str(&serde_json::to_string(&rec("b", TaskStatus::Failed)).unwrap());
        bytes.push('\n');
        std::fs::write(tmp.path().join("summary.jsonl"), bytes).unwrap();
        let snap = read_run_snapshot(tmp.path());
        assert_eq!(snap.task_count(), 2);
    }

    /// In-progress run that crashed mid-write of the last record. The
    /// canonical reader must drop the truncated tail without failing.
    #[test]
    fn truncated_trailing_line_is_dropped() {
        let tmp = TempDir::new().unwrap();
        let good = serde_json::to_string(&rec("a", TaskStatus::Success)).unwrap();
        let truncated = "{\"task_id\":\"b\",\"status\":\"Suc"; // mid-write
        let bytes = format!("{good}\n{truncated}");
        std::fs::write(tmp.path().join("summary.jsonl"), bytes).unwrap();
        let snap = read_run_snapshot(tmp.path());
        assert_eq!(snap.task_count(), 1);
        assert!(snap.get("a").is_some());
    }

    #[test]
    fn finalized_summary_json_seeds_snapshot() {
        let tmp = TempDir::new().unwrap();
        let t = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let summary = RunSummary {
            run_id: uuid::Uuid::nil(),
            manifest_path: PathBuf::from("/x.toml"),
            manifest_name: None,
            pitboss_version: "test".into(),
            claude_version: None,
            started_at: t,
            ended_at: t,
            total_duration_ms: 0,
            tasks_total: 2,
            tasks_failed: 1,
            was_interrupted: false,
            notify_failures: None,
            tasks: vec![rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
            spend_breakdown: None,
        };
        std::fs::write(
            tmp.path().join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();

        let snap = read_run_snapshot(tmp.path());
        assert!(snap.is_finalized());
        assert_eq!(snap.task_count(), 2);
        assert_eq!(snap.failed_count(), 1);
        assert_eq!(snap.run_summary.unwrap().tasks_total, 2);
    }

    /// The interesting merge case: `summary.json` is older than the
    /// most recent `summary.jsonl` row (e.g. resume after finalize).
    /// The newer `.jsonl` row wins.
    #[test]
    fn jsonl_overlays_summary_json() {
        let tmp = TempDir::new().unwrap();
        let t = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let snapshot_json = RunSummary {
            run_id: uuid::Uuid::nil(),
            manifest_path: PathBuf::from("/x.toml"),
            manifest_name: None,
            pitboss_version: "test".into(),
            claude_version: None,
            started_at: t,
            ended_at: t,
            total_duration_ms: 0,
            tasks_total: 1,
            tasks_failed: 1,
            was_interrupted: false,
            notify_failures: None,
            tasks: vec![rec("a", TaskStatus::Failed)],
            spend_breakdown: None,
        };
        std::fs::write(
            tmp.path().join("summary.json"),
            serde_json::to_vec(&snapshot_json).unwrap(),
        )
        .unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[rec("a", TaskStatus::Success)],
        );
        let snap = read_run_snapshot(tmp.path());
        assert_eq!(snap.task_count(), 1);
        assert!(
            matches!(snap.get("a").unwrap().status, TaskStatus::Success),
            ".jsonl overlays .json for the same task_id",
        );
    }

    /// Cross-surface parity contract (#437 acceptance criterion). Every
    /// consumer of the canonical reader derives its counts and lookups
    /// from the same `RunSnapshot`, so the same fixture must produce
    /// identical numbers regardless of which surface is asking.
    ///
    /// Surfaces and the shape they consume:
    ///
    /// - TUI watcher / TUI launcher: `snapshot.tasks` (`HashMap` iterated
    ///   for tile rendering).
    /// - `pitboss status`: `snapshot.tasks.into_values().collect::<Vec>()`
    ///   + `snapshot.run_summary.notify_failures`.
    /// - `pitboss list` / Stale-state classifier: `snapshot.task_count()`
    ///   + `snapshot.failed_count()` — the latent over-count bug #429
    ///     missed on the SPA side.
    /// - `pitboss diff`: `snapshot.run_summary` when present, else
    ///   `snapshot.tasks.into_values().collect()` for reconstruction.
    /// - Dispatcher finalize aggregation: `snapshot.tasks.keys()` for the
    ///   "is this actor already settled?" filter + the records for the
    ///   `summary.json` write.
    /// - `pitboss-web` `task_detail`: `snapshot.tasks.get(task_id)`.
    /// - `pitboss-web` insights aggregator: `snapshot.run_summary`.
    ///
    /// If this test breaks, a consumer has either drifted away from the
    /// canonical reader or the canonical reader's merge semantics have
    /// changed without a coordinated audit of the consumers.
    #[test]
    fn cross_surface_parity_on_duplicate_task_ids() {
        let tmp = TempDir::new().unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[
                // worker-1 cycled through cancel + respawn before
                // landing Success — the reprompt/cancel/respawn lifecycle
                // that motivated #429.
                rec("worker-1", TaskStatus::Cancelled),
                rec("worker-1", TaskStatus::Success),
                rec("worker-2", TaskStatus::Failed),
                rec("worker-3", TaskStatus::Success),
            ],
        );
        let snap = read_run_snapshot(tmp.path());

        // ---- TUI surface (HashMap iteration for tile rendering) ----
        assert_eq!(snap.tasks.len(), 3, "TUI tile count");

        // ---- pitboss status / diff / hierarchical (Vec<TaskRecord>) ----
        let vec: Vec<&TaskRecord> = snap.tasks.values().collect();
        assert_eq!(vec.len(), 3, "status/diff/hierarchical tasks_total");
        assert_eq!(
            vec.iter()
                .filter(|t| !matches!(t.status, TaskStatus::Success))
                .count(),
            1,
            "tasks_failed = worker-2 only",
        );

        // ---- pitboss list / Stale classifier (helper methods) ----
        assert_eq!(snap.task_count(), 3, "list view tasks_total");
        assert_eq!(snap.failed_count(), 1, "list view tasks_failed");

        // ---- pitboss-web task_detail (get by id) ----
        let w1 = snap.get("worker-1").expect("worker-1 present");
        assert!(
            matches!(w1.status, TaskStatus::Success),
            "task_detail returns the freshest row, not the Cancelled one",
        );
        assert!(snap.get("worker-2").is_some());
        assert!(snap.get("worker-3").is_some());
        assert!(snap.get("nonexistent").is_none());
    }

    #[test]
    fn corrupt_summary_json_falls_back_to_jsonl() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("summary.json"), "{not json").unwrap();
        write_jsonl(
            &tmp.path().join("summary.jsonl"),
            &[rec("a", TaskStatus::Success)],
        );
        let snap = read_run_snapshot(tmp.path());
        assert!(!snap.is_finalized(), "broken .json doesn't mark finalized");
        assert_eq!(snap.task_count(), 1);
    }
}
