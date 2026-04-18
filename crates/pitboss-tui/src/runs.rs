//! Run discovery helpers shared between the `list` subcommand and the run picker.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Status of a discovered run directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// `summary.json` exists and parsed — run finalized cleanly.
    Complete,
    /// `summary.jsonl` has at least one task record but no final
    /// `summary.json` yet — dispatcher is (or was) running.
    Running,
    /// Neither `summary.json` nor any records in `summary.jsonl` — the
    /// dispatcher wrote the initial manifest + resolved.json but never
    /// produced task output (orphaned/aborted invocation).
    Aborted,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Running => "running",
            Self::Aborted => "aborted",
        }
    }

    pub fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// A summary entry for a single run directory.
#[derive(Debug)]
pub struct RunEntry {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub mtime: SystemTime,
    pub tasks_total: usize,
    pub tasks_failed: usize,
    pub status: RunStatus,
}

/// Returns the base directory that holds all run sub-directories.
pub fn runs_base_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".local/share/pitboss/runs")
    } else {
        PathBuf::from("./pitboss-runs")
    }
}

/// Collect all run entries under `base`, sorted newest-first by mtime.
///
/// Silently ignores entries that cannot be read.
pub fn collect_run_entries(base: &Path) -> Vec<RunEntry> {
    let Ok(rd) = std::fs::read_dir(base) else {
        return Vec::new();
    };

    let mut entries: Vec<RunEntry> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let run_id = e.file_name().to_string_lossy().to_string();
            let run_dir = e.path();
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            collect_run_entry(&run_dir, run_id, mtime)
        })
        .collect();

    // Newest first.
    entries.sort_by_key(|e| std::cmp::Reverse(e.mtime));
    entries
}

/// Build a [`RunEntry`] for one run directory.
pub fn collect_run_entry(run_dir: &Path, run_id: String, mtime: SystemTime) -> RunEntry {
    // Try summary.json first (finalized run).
    let summary_json = run_dir.join("summary.json");
    if let Ok(bytes) = std::fs::read(&summary_json) {
        if let Ok(s) = serde_json::from_slice::<pitboss_core::store::RunSummary>(&bytes) {
            return RunEntry {
                run_id,
                run_dir: run_dir.to_path_buf(),
                mtime,
                tasks_total: s.tasks_total,
                tasks_failed: s.tasks_failed,
                status: RunStatus::Complete,
            };
        }
    }

    // No summary.json. Determine whether the dispatcher is still alive by
    // looking for the per-run control socket — pitboss-cli creates it at
    // run start (see `pitboss_cli::control::control_socket_path`) and
    // removes it at finalize. If it's there, the run is actively running
    // regardless of whether summary.jsonl has records yet. Keeps early-
    // stage runs (workers not yet settled) from being misclassified as
    // aborted.
    let jsonl = run_dir.join("summary.jsonl");
    let (settled_total, failed) = count_jsonl_tasks(&jsonl);
    let live = control_socket_is_live(&run_id, run_dir);

    // For the count column: during a live run, summary.jsonl only grows as
    // tasks SETTLE. The spawned-so-far count is the subdirectory count of
    // <run-dir>/tasks/ (pitboss creates one per task at spawn time). Show
    // max(settled, spawned) so the picker reflects in-flight work even
    // before any of it has finished.
    let spawned_count = count_tasks_subdirs(run_dir);
    let total = settled_total.max(spawned_count);

    let status = if live {
        RunStatus::Running
    } else if settled_total > 0 {
        // Dispatcher is gone but did produce records — an interrupted run.
        // We bucket this as "Running" in the 3-state taxonomy; users can
        // distinguish by the absence of a live socket if needed.
        RunStatus::Running
    } else {
        RunStatus::Aborted
    };
    RunEntry {
        run_id,
        run_dir: run_dir.to_path_buf(),
        mtime,
        tasks_total: total,
        tasks_failed: failed,
        status,
    }
}

/// Count subdirectories under `<run-dir>/tasks/`. Each task (lead + every
/// spawned worker) gets its own subdir at spawn time, so this gives the
/// in-flight count during a live run — reliable even when summary.jsonl
/// is still empty.
fn count_tasks_subdirs(run_dir: &Path) -> usize {
    let tasks_dir = run_dir.join("tasks");
    let Ok(rd) = std::fs::read_dir(&tasks_dir) else {
        return 0;
    };
    rd.flatten().filter(|e| e.path().is_dir()).count()
}

/// Return `true` if the pitboss control socket for `run_id` currently
/// exists on disk. Mirrors `pitboss_cli::control::control_socket_path`:
/// prefers `$XDG_RUNTIME_DIR/pitboss/<run-id>.control.sock`, falls back
/// to `<run-dir>/<run-id>/control.sock` when `XDG_RUNTIME_DIR` is unset.
/// The file is a unix-domain socket when the dispatcher is alive and
/// gets unlinked on clean shutdown.
fn control_socket_is_live(run_id: &str, run_dir: &Path) -> bool {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg)
            .join("pitboss")
            .join(format!("{run_id}.control.sock"));
        if p.exists() {
            return true;
        }
    }
    run_dir.join(run_id).join("control.sock").exists()
}

/// Count total and failed task records from a `summary.jsonl` file.
pub fn count_jsonl_tasks(path: &Path) -> (usize, usize) {
    let Ok(file) = std::fs::File::open(path) else {
        return (0, 0);
    };
    let reader = std::io::BufReader::new(file);
    let mut total = 0;
    let mut failed = 0;
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        total += 1;
        if let Ok(r) = serde_json::from_str::<pitboss_core::store::TaskRecord>(&trimmed) {
            if !matches!(r.status, pitboss_core::store::TaskStatus::Success) {
                failed += 1;
            }
        }
    }
    (total, failed)
}

/// Format a [`SystemTime`] as `"YYYY-MM-DD HH:MM:SS UTC"`.
pub fn format_mtime(mtime: SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let secs = mtime
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    #[allow(clippy::cast_possible_wrap)]
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0).unwrap_or_default();
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_run_dir(base: &Path, name: &str) -> PathBuf {
        let d = base.join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn collect_run_entries_empty_base() {
        let tmp = TempDir::new().unwrap();
        let entries = collect_run_entries(tmp.path());
        assert!(entries.is_empty());
    }

    #[test]
    fn collect_run_entries_finds_dirs() {
        let tmp = TempDir::new().unwrap();
        make_run_dir(tmp.path(), "run-aaa");
        make_run_dir(tmp.path(), "run-bbb");
        let entries = collect_run_entries(tmp.path());
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn collect_run_entries_ignores_files() {
        let tmp = TempDir::new().unwrap();
        make_run_dir(tmp.path(), "run-aaa");
        fs::write(tmp.path().join("not-a-dir.txt"), b"hi").unwrap();
        let entries = collect_run_entries(tmp.path());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].run_id, "run-aaa");
    }

    #[test]
    fn collect_run_entries_sorted_newest_first() {
        let tmp = TempDir::new().unwrap();
        make_run_dir(tmp.path(), "run-old");
        // Brief pause so the second directory has a later mtime.
        std::thread::sleep(std::time::Duration::from_millis(10));
        make_run_dir(tmp.path(), "run-new");

        let entries = collect_run_entries(tmp.path());
        assert_eq!(entries.len(), 2);
        // Both run IDs must appear.
        let ids: Vec<&str> = entries.iter().map(|e| e.run_id.as_str()).collect();
        assert!(ids.contains(&"run-old"));
        assert!(ids.contains(&"run-new"));
        // Newest (run-new) should come first; if mtimes are equal (fast FS),
        // we only assert that both are present.
        let new_idx = entries.iter().position(|e| e.run_id == "run-new").unwrap();
        let old_idx = entries.iter().position(|e| e.run_id == "run-old").unwrap();
        // new_idx <= old_idx (newer first), or they can be equal if same mtime
        assert!(new_idx <= old_idx);
    }

    #[test]
    fn collect_run_entry_no_summary_files() {
        let tmp = TempDir::new().unwrap();
        let run_dir = make_run_dir(tmp.path(), "run-x");
        let entry = collect_run_entry(&run_dir, "run-x".to_string(), SystemTime::UNIX_EPOCH);
        assert_eq!(entry.run_id, "run-x");
        assert_eq!(entry.tasks_total, 0);
        assert_eq!(entry.tasks_failed, 0);
        assert_eq!(entry.status, RunStatus::Aborted);
    }

    #[test]
    fn count_jsonl_tasks_missing_file_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let (total, failed) = count_jsonl_tasks(&tmp.path().join("nonexistent.jsonl"));
        assert_eq!((total, failed), (0, 0));
    }

    #[test]
    fn format_mtime_epoch() {
        let s = format_mtime(SystemTime::UNIX_EPOCH);
        assert!(s.starts_with("1970-01-01"));
    }
}
