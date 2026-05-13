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
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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

// ----------------------------------------------------------------------------
// Stateful reader — Tier 2 of #437.
//
// `read_run_snapshot` re-parses the full files on every call. For the TUI
// watcher, which polls at 4 Hz, that's O(file size) work per tick. The
// reader below keeps the parsed [`RunSnapshot`] live across calls and
// `refresh()` does the minimum work: a stat, then either no-op, a tail
// parse of the new bytes only, or — on rotation / truncate / unexpected
// mtime regress — a full reparse.
//
// One-shot callers (`pitboss status`, `pitboss list`, the web HTTP
// handlers, the dispatcher's finalize aggregator) keep using
// `read_run_snapshot` — they never read the same file twice in the
// hot path, so paying the per-call setup cost is wasted.
// ----------------------------------------------------------------------------

/// Per-source tracking state for [`RunSnapshotReader`].
#[derive(Debug, Clone, Default)]
struct JsonlTail {
    /// Byte offset of the next unread byte. After a successful tail, this
    /// advances to the position just past the last complete `\n` in the
    /// new bytes. A mid-write trailing line is left in the file at this
    /// offset and re-read on the next refresh.
    offset: u64,
    mtime: Option<SystemTime>,
    /// File identity. On unix, the inode discriminates "appended" from
    /// "replaced wholesale" (`pitboss resume` against a wiped run dir,
    /// `mv` over the file, etc.). Inode alone is insufficient — Linux
    /// tmpfs (and ext4 under churn) readily reuses inodes when a file
    /// is removed and a new one is created at the same path. We pair
    /// inode with [`Self::created_at`] below for unambiguous rotation
    /// detection.
    #[cfg(unix)]
    inode: Option<u64>,
    /// File creation time (`statx::stx_btime` on Linux,
    /// `st_birthtimespec` on macOS). Set once at file creation and
    /// never updated, so a rotated file at the same path with a reused
    /// inode is still distinguishable. Falls back to `None` on
    /// filesystems that don't expose birthtime (legacy NFS, some
    /// FUSE), in which case we rely on the inode + size + mtime
    /// triple alone — which is what the pre-#440 code did and what
    /// the macOS local tests passed under.
    created_at: Option<SystemTime>,
}

/// Stateful reader for a run directory's summary artifacts.
///
/// Hold one per (`run_id`, consumer) and call [`Self::refresh`] each
/// tick. The returned snapshot is the same canonical merge as
/// [`read_run_snapshot`] (see that function's docs for semantics) —
/// the only difference is the cost profile: O(new bytes) on append,
/// O(0) on no change, O(full file) only on rotation / truncate / first
/// call.
///
/// # Cost model
///
/// | Disk change between refreshes | Work |
/// |---|---|
/// | None | One stat per file. No parse. |
/// | `summary.jsonl` appended (same inode, mtime advanced) | Parse only the new bytes; overlay into the cached map. |
/// | `summary.json` appeared or its mtime advanced | Re-read and reparse `summary.json` once. Tasks re-seeded into the map. |
/// | `summary.jsonl` rotated (inode change), truncated (size shrunk), or mtime regressed | Full reparse via [`read_run_snapshot`]; tracking state reset. |
///
/// The reader is `!Sync` — it holds owned mutable state. Wrap in a
/// `Mutex` if multiple threads need to share one instance; the more
/// typical pattern is one reader per polling consumer.
#[derive(Debug)]
pub struct RunSnapshotReader {
    run_dir: PathBuf,
    cached: RunSnapshot,
    json_mtime: Option<SystemTime>,
    jsonl: JsonlTail,
}

impl RunSnapshotReader {
    /// Construct an empty reader. The first `refresh()` call populates
    /// the cache via a full read.
    #[must_use]
    pub fn new(run_dir: PathBuf) -> Self {
        Self {
            run_dir,
            cached: RunSnapshot::default(),
            json_mtime: None,
            jsonl: JsonlTail::default(),
        }
    }

    /// Reconcile the cached snapshot against the current on-disk state
    /// and return a reference to it. See the struct docs for the cost
    /// model.
    pub fn refresh(&mut self) -> &RunSnapshot {
        let json_path = self.run_dir.join("summary.json");
        let jsonl_path = self.run_dir.join("summary.jsonl");

        let head_meta = std::fs::metadata(&json_path).ok();
        let tail_meta = std::fs::metadata(&jsonl_path).ok();

        // Decide whether the jsonl side is in a state where an
        // incremental tail is safe. Anything ambiguous → full reparse.
        let tail_safe = self.jsonl_tail_is_safe(tail_meta.as_ref());
        if !tail_safe {
            return self.full_reparse(tail_meta.as_ref(), head_meta.as_ref());
        }

        // summary.json: re-read whenever its mtime advances. Appearing
        // for the first time is the common case (run just finalized).
        if let Some(jm) = head_meta.as_ref() {
            let m = jm.modified().ok();
            if self.json_mtime != m {
                self.reload_json(&json_path);
                self.json_mtime = m;
            }
        } else if self.json_mtime.is_some() {
            // File went away — uncommon outside test fixtures, but be
            // defensive. Drop the cached finalized summary; per-task
            // rows live in `cached.tasks` and stay valid.
            self.cached.run_summary = None;
            self.json_mtime = None;
        }

        // summary.jsonl: tail any new bytes from `self.jsonl.offset`
        // to current EOF.
        if let Some(jm) = tail_meta.as_ref() {
            let size = jm.len();
            if size > self.jsonl.offset {
                self.tail_jsonl(&jsonl_path, size);
            }
            self.jsonl.mtime = jm.modified().ok();
            self.jsonl.created_at = jm.created().ok();
            #[cfg(unix)]
            {
                self.jsonl.inode = Some(jm.ino());
            }
        }

        &self.cached
    }

    /// Access the cached snapshot without touching the filesystem.
    #[must_use]
    pub fn snapshot(&self) -> &RunSnapshot {
        &self.cached
    }

    fn jsonl_tail_is_safe(&self, meta: Option<&std::fs::Metadata>) -> bool {
        let Some(meta) = meta else {
            // No file. Safe iff we never saw one — otherwise the file
            // went away and we should rebuild from scratch.
            return self.jsonl.offset == 0;
        };
        // First refresh: any state is safe (the read starts from 0).
        if self.jsonl.mtime.is_none() && self.jsonl.offset == 0 {
            return true;
        }
        // Truncate detected.
        if meta.len() < self.jsonl.offset {
            return false;
        }
        // Backdated write.
        if let (Some(prev), Ok(cur)) = (self.jsonl.mtime, meta.modified()) {
            if cur < prev {
                return false;
            }
        }
        // Rotation detected via birthtime. `rm path && create path`
        // on Linux tmpfs readily reuses inodes — local macOS tests
        // passed under inode-only detection, CI on Linux did not.
        // Birthtime advances unconditionally on every fresh inode
        // allocation. Missing on a filesystem that doesn't expose it
        // (legacy NFS, some FUSE) → fall through to inode check.
        if let (Some(prev), Ok(cur)) = (self.jsonl.created_at, meta.created()) {
            if cur != prev {
                return false;
            }
        }
        // Rotation detected via inode (catches platforms that don't
        // expose birthtime, and is correct on filesystems that don't
        // reuse inodes promptly).
        #[cfg(unix)]
        {
            if let Some(prev) = self.jsonl.inode {
                if prev != meta.ino() {
                    return false;
                }
            }
        }
        true
    }

    fn full_reparse(
        &mut self,
        tail_meta: Option<&std::fs::Metadata>,
        head_meta: Option<&std::fs::Metadata>,
    ) -> &RunSnapshot {
        self.cached = read_run_snapshot(&self.run_dir);
        self.json_mtime = head_meta.and_then(|m| m.modified().ok());
        self.jsonl = JsonlTail::default();
        if let Some(m) = tail_meta {
            self.jsonl.offset = m.len();
            self.jsonl.mtime = m.modified().ok();
            self.jsonl.created_at = m.created().ok();
            #[cfg(unix)]
            {
                self.jsonl.inode = Some(m.ino());
            }
        }
        &self.cached
    }

    fn reload_json(&mut self, path: &Path) {
        // Drop the previous `run_summary` first so a parse failure
        // leaves the cache in an honest "not finalized" state.
        self.cached.run_summary = None;
        self.cached.run_summary = load_summary_json(path, &mut self.cached.tasks);
    }

    fn tail_jsonl(&mut self, path: &Path, current_size: u64) {
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.jsonl.offset)).is_err() {
            return;
        }
        let to_read = current_size.saturating_sub(self.jsonl.offset);
        let mut buf = vec![0u8; usize::try_from(to_read).unwrap_or(usize::MAX)];
        let Ok(n) = file.read(&mut buf) else { return };
        buf.truncate(n);

        // Parse only up through the last `\n`. Anything after the last
        // newline is a mid-write tail — leave it in the file and
        // re-read on the next refresh.
        let last_newline = buf.iter().rposition(|&b| b == b'\n');
        let Some(last_nl) = last_newline else {
            // Read bytes but no newline yet — wait for more writes.
            return;
        };
        let complete = &buf[..=last_nl];
        let text = match std::str::from_utf8(complete) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "summary.jsonl tail: non-utf8 bytes; skipping this delta",
                );
                self.jsonl.offset += u64::try_from(last_nl + 1).unwrap_or(0);
                return;
            }
        };
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<TaskRecord>(trimmed) {
                Ok(rec) => {
                    self.cached.tasks.insert(rec.task_id.clone(), rec);
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    line = trimmed,
                    path = %path.display(),
                    "summary.jsonl tail: skipping unparseable line",
                ),
            }
        }
        self.jsonl.offset += u64::try_from(last_nl + 1).unwrap_or(0);
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

    // ------------------------------------------------------------------
    // RunSnapshotReader — Tier 2 incremental-tail tests.
    // ------------------------------------------------------------------

    /// Bump the file's mtime forward by `delta_secs` so tests that
    /// rely on mtime-comparison don't race the filesystem's
    /// granularity (HFS+ is 1s; ext4 is 1ns but the dispatch tick is
    /// fast enough to land in the same nanosecond on aarch64).
    fn bump_mtime(path: &Path, delta_secs: u64) {
        let now = SystemTime::now()
            .checked_add(std::time::Duration::from_secs(delta_secs))
            .unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(now).unwrap();
    }

    fn write_records_to_jsonl(path: &Path, records: &[TaskRecord]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for r in records {
            writeln!(f, "{}", serde_json::to_string(r).unwrap()).unwrap();
        }
        f.sync_all().unwrap();
    }

    #[test]
    fn reader_initial_refresh_handles_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        let snap = reader.refresh();
        assert_eq!(snap.task_count(), 0);
        assert!(!snap.is_finalized());
    }

    #[test]
    fn reader_tail_picks_up_appended_rows() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        write_records_to_jsonl(&jsonl, &[rec("a", TaskStatus::Success)]);

        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        assert_eq!(reader.refresh().task_count(), 1);

        // Append a second row + bump mtime past prior reading.
        write_records_to_jsonl(&jsonl, &[rec("b", TaskStatus::Failed)]);
        bump_mtime(&jsonl, 60);

        let snap = reader.refresh();
        assert_eq!(snap.task_count(), 2);
        assert_eq!(snap.failed_count(), 1);
    }

    /// Append with no trailing newline (mid-write tail) must NOT parse
    /// the partial line. The next refresh, after the newline lands,
    /// picks it up.
    #[test]
    fn reader_tail_defers_partial_line_until_newline() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        write_records_to_jsonl(&jsonl, &[rec("a", TaskStatus::Success)]);

        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        assert_eq!(reader.refresh().task_count(), 1);

        // Append the start of "b" but no closing newline yet.
        let partial = serde_json::to_string(&rec("b", TaskStatus::Success)).unwrap();
        let half = &partial[..partial.len() / 2];
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&jsonl)
                .unwrap();
            f.write_all(half.as_bytes()).unwrap();
        }
        bump_mtime(&jsonl, 60);
        assert_eq!(reader.refresh().task_count(), 1, "partial line is deferred");

        // Now complete the row with the rest + a newline.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&jsonl)
                .unwrap();
            f.write_all(&partial.as_bytes()[partial.len() / 2..])
                .unwrap();
            writeln!(f).unwrap();
        }
        bump_mtime(&jsonl, 120);
        assert_eq!(
            reader.refresh().task_count(),
            2,
            "completed line is picked up on next refresh",
        );
    }

    /// Truncating the file (size shrinks past the reader's offset)
    /// triggers a full reparse rather than a misaligned tail.
    #[test]
    fn reader_truncate_triggers_full_reparse() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        write_records_to_jsonl(
            &jsonl,
            &[rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
        );

        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        assert_eq!(reader.refresh().task_count(), 2);

        // Truncate to one record.
        std::fs::write(
            &jsonl,
            serde_json::to_string(&rec("c", TaskStatus::Success)).unwrap() + "\n",
        )
        .unwrap();
        bump_mtime(&jsonl, 60);

        let snap = reader.refresh();
        assert_eq!(snap.task_count(), 1, "post-truncate count");
        assert!(snap.get("c").is_some(), "post-truncate content");
        assert!(snap.get("a").is_none(), "pre-truncate rows dropped");
    }

    /// Replacing the file at the same path (new inode) is treated as
    /// rotation: cached tracking resets, content is rebuilt from
    /// scratch.
    #[cfg(unix)]
    #[test]
    fn reader_inode_change_triggers_full_reparse() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        write_records_to_jsonl(&jsonl, &[rec("a", TaskStatus::Success)]);

        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        assert_eq!(reader.refresh().task_count(), 1);

        // Rotate: remove and rewrite. New file = new inode.
        std::fs::remove_file(&jsonl).unwrap();
        write_records_to_jsonl(
            &jsonl,
            &[
                rec("x", TaskStatus::Success),
                rec("y", TaskStatus::Cancelled),
            ],
        );

        let snap = reader.refresh();
        assert_eq!(snap.task_count(), 2);
        assert!(snap.get("a").is_none(), "post-rotation: old rows gone");
        assert!(snap.get("x").is_some());
        assert!(snap.get("y").is_some());
    }

    /// `summary.json` appearing for the first time after the reader was
    /// already tracking `.jsonl` (the run just finalized) re-seeds the
    /// snapshot with the authoritative summary.
    #[test]
    fn reader_picks_up_summary_json_when_it_appears() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        write_records_to_jsonl(&jsonl, &[rec("a", TaskStatus::Success)]);

        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        assert!(!reader.refresh().is_finalized());

        // Finalize: write summary.json.
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
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            notify_failures: Some(2),
            tasks: vec![rec("a", TaskStatus::Success)],
            spend_breakdown: None,
        };
        std::fs::write(
            tmp.path().join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();

        let snap = reader.refresh();
        assert!(snap.is_finalized());
        assert_eq!(snap.run_summary.as_ref().unwrap().notify_failures, Some(2));
    }

    /// No-op refreshes (no disk change) leave the cache stable and do
    /// no parsing work. We can't directly assert "no parsing
    /// happened", but we can assert the snapshot doesn't drift.
    #[test]
    fn reader_idempotent_when_disk_unchanged() {
        let tmp = TempDir::new().unwrap();
        let jsonl = tmp.path().join("summary.jsonl");
        write_records_to_jsonl(
            &jsonl,
            &[rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
        );

        let mut reader = RunSnapshotReader::new(tmp.path().to_path_buf());
        let first = reader.refresh().task_count();
        let second = reader.refresh().task_count();
        let third = reader.refresh().task_count();
        assert_eq!(first, second);
        assert_eq!(second, third);
        assert_eq!(first, 2);
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
