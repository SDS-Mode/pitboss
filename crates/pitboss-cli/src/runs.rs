//! Run discovery — single source of truth for "what runs exist on disk
//! and what state are they in."
//!
//! Both the `pitboss-tui` run picker and the `pitboss prune` subcommand
//! consume this module. The `pitboss-tui::runs` module is a thin
//! re-export shim so existing TUI imports keep working unchanged.
//!
//! ## State machine
//!
//! For each subdirectory under `~/.local/share/pitboss/runs/`:
//!
//! ```text
//!                ┌───────────────────┐  yes
//!     summary.json parses?  ─────────▶  Complete
//!                └────┬──────────────┘
//!                     │ no
//!                     ▼
//!                ┌───────────────────┐  yes
//!     control socket connect()? ────▶  Running (live dispatcher)
//!                └────┬──────────────┘
//!                     │ no
//!                     ▼
//!                ┌───────────────────┐  yes
//!     summary.jsonl mtime > 4h? ────▶  Stale
//!                └────┬──────────────┘
//!                     │ no
//!                     ▼
//!                ┌───────────────────┐  yes
//!     summary.jsonl has rows? ──────▶  Running (interrupted, but recent)
//!                └────┬──────────────┘
//!                     │ no
//!                     ▼
//!                  Aborted
//! ```
//!
//! The `Stale` branch is the v0.9 addition — previously `Running` was
//! sticky for any run whose socket *file* still existed, which produced
//! permanent false positives after `kill -KILL`/OOM. The classifier now
//! does an actual `connect()` probe (dead socket files return
//! ECONNREFUSED almost instantly) and uses the `summary.jsonl` mtime as
//! a recency floor.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long a run can sit without a `summary.jsonl` write before the
/// classifier downgrades it from `Running` (interrupted) to `Stale`.
/// Pairs with the future `pitboss prune` `--older-than` default.
pub const STALENESS_THRESHOLD: Duration = Duration::from_secs(4 * 3600);

/// Status of a discovered run directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// `summary.json` exists and parsed — run finalized cleanly.
    Complete,
    /// Live dispatcher (control socket accepts a connection) OR
    /// `summary.jsonl` was written recently (within
    /// [`STALENESS_THRESHOLD`]) so the dispatcher might still come
    /// back. Used to be sticky for any socket *file*; v0.9 requires
    /// an actual connect or recent activity.
    Running,
    /// No live dispatcher AND no `summary.jsonl` activity within
    /// [`STALENESS_THRESHOLD`]. Almost certainly the result of a
    /// `kill -KILL` / OOM / crash that orphaned the run dir. The
    /// `pitboss prune` subcommand matches on this state by default.
    Stale,
    /// No `summary.json`, no `summary.jsonl` records, and no live
    /// dispatcher — the dispatcher wrote the initial manifest +
    /// `resolved.json` but never produced task output.
    Aborted,
}

impl RunStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Running => "running",
            Self::Stale => "stale",
            Self::Aborted => "aborted",
        }
    }

    pub fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// `true` for any state in which the dispatcher is *not* expected
    /// to come back. Used by the prune sweep to decide whether a run
    /// is a candidate for cleanup.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Stale | Self::Aborted)
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

    entries.sort_by_key(|e| std::cmp::Reverse(e.mtime));
    entries
}

/// Build a [`RunEntry`] for one run directory.
pub fn collect_run_entry(run_dir: &Path, run_id: String, mtime: SystemTime) -> RunEntry {
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

    let jsonl = run_dir.join("summary.jsonl");
    let (settled_total, failed) = count_jsonl_tasks(&jsonl);
    let live = control_socket_is_live(&run_id, run_dir);

    let spawned_count = count_tasks_subdirs(run_dir);
    let total = settled_total.max(spawned_count);

    let status = classify_status(live, settled_total, jsonl_recent(&jsonl));
    RunEntry {
        run_id,
        run_dir: run_dir.to_path_buf(),
        mtime,
        tasks_total: total,
        tasks_failed: failed,
        status,
    }
}

/// Pure status classifier — split out so tests can exercise every
/// branch without touching the filesystem.
///
/// Inputs:
/// * `live` — control socket accepted a connection just now.
/// * `settled_total` — number of complete `summary.jsonl` rows.
/// * `jsonl_recent` — `summary.jsonl` mtime is within
///   [`STALENESS_THRESHOLD`].
fn classify_status(live: bool, settled_total: usize, jsonl_recent: bool) -> RunStatus {
    if live {
        return RunStatus::Running;
    }
    // Dispatcher is gone. If there's no jsonl activity at all and no
    // recent records, the run never produced output → Aborted.
    if settled_total == 0 && !jsonl_recent {
        return RunStatus::Aborted;
    }
    // Otherwise the dispatcher *did* write records. If those writes
    // are stale, the run is orphaned; if they're recent, treat the
    // run as merely interrupted (could be in the middle of a TUI
    // hand-off, fast-restart, etc).
    if jsonl_recent {
        RunStatus::Running
    } else {
        RunStatus::Stale
    }
}

fn count_tasks_subdirs(run_dir: &Path) -> usize {
    let tasks_dir = run_dir.join("tasks");
    let Ok(rd) = std::fs::read_dir(&tasks_dir) else {
        return 0;
    };
    rd.flatten().filter(|e| e.path().is_dir()).count()
}

/// `true` when the control socket for `run_id` is actually accepting
/// connections (not just when the socket *file* still exists).
///
/// A `kill -KILL`/OOM leaves the abstract socket file behind without
/// a listener — the v0.8 file-existence probe stuck `Running` on
/// those forever. A real `connect()` returns ECONNREFUSED almost
/// instantly when nothing is bound, which is the signal we want.
pub fn control_socket_is_live(run_id: &str, run_dir: &Path) -> bool {
    let path = match resolve_socket_path(run_id, run_dir) {
        Some(p) => p,
        None => return false,
    };
    if !path.exists() {
        return false;
    }
    use std::os::unix::net::UnixStream;
    // No timeout knob is needed: connect() to a unix socket either
    // finds a listener immediately or returns ECONNREFUSED. We never
    // exchange bytes — the connect itself is the liveness probe.
    UnixStream::connect(&path).is_ok()
}

/// Mirror of `pitboss_cli::control::control_socket_path` for the case
/// where we have a `run_id` string (not a `Uuid`) — the runs-discovery
/// caller reads run IDs out of directory names.
fn resolve_socket_path(run_id: &str, run_dir: &Path) -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg)
            .join("pitboss")
            .join(format!("{run_id}.control.sock"));
        if p.exists() {
            return Some(p);
        }
    }
    let fallback = run_dir.join(run_id).join("control.sock");
    Some(fallback)
}

/// `true` when `summary.jsonl` has been written within
/// [`STALENESS_THRESHOLD`]. Returns `false` when the file is missing
/// (no activity) or its mtime is older than the threshold.
fn jsonl_recent(jsonl: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(jsonl) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(mtime)
        .map(|elapsed| elapsed <= STALENESS_THRESHOLD)
        .unwrap_or(true)
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
        std::thread::sleep(std::time::Duration::from_millis(10));
        make_run_dir(tmp.path(), "run-new");

        let entries = collect_run_entries(tmp.path());
        assert_eq!(entries.len(), 2);
        let ids: Vec<&str> = entries.iter().map(|e| e.run_id.as_str()).collect();
        assert!(ids.contains(&"run-old"));
        assert!(ids.contains(&"run-new"));
        let new_idx = entries.iter().position(|e| e.run_id == "run-new").unwrap();
        let old_idx = entries.iter().position(|e| e.run_id == "run-old").unwrap();
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

    // ── classify_status: pure-function coverage ─────────────────────────

    #[test]
    fn classify_live_socket_is_running_regardless_of_age() {
        assert_eq!(
            classify_status(true, 0, false),
            RunStatus::Running,
            "live dispatcher trumps everything else"
        );
        assert_eq!(classify_status(true, 5, true), RunStatus::Running);
    }

    #[test]
    fn classify_no_socket_no_records_no_recent_is_aborted() {
        assert_eq!(classify_status(false, 0, false), RunStatus::Aborted);
    }

    #[test]
    fn classify_no_socket_with_recent_records_is_running() {
        // Dispatcher gone but jsonl mtime is fresh — interrupted
        // restart, not orphaned. Don't downgrade.
        assert_eq!(classify_status(false, 3, true), RunStatus::Running);
    }

    #[test]
    fn classify_no_socket_with_old_records_is_stale() {
        assert_eq!(classify_status(false, 3, false), RunStatus::Stale);
    }

    #[test]
    fn classify_no_socket_no_records_with_recent_jsonl_is_running() {
        // Edge case: jsonl exists and is fresh (e.g. just created),
        // but no rows yet. The dispatcher might be mid-startup —
        // don't aborted-flag it.
        assert_eq!(classify_status(false, 0, true), RunStatus::Running);
    }

    // ── Stale-classification through the file-system path ───────────────

    #[test]
    fn collect_run_entry_with_old_jsonl_no_socket_is_stale() {
        let tmp = TempDir::new().unwrap();
        let run_dir = make_run_dir(tmp.path(), "run-stale");
        // Write a summary.jsonl with one row, then back-date its mtime
        // past the staleness threshold. No control socket exists.
        let jsonl = run_dir.join("summary.jsonl");
        fs::write(
            &jsonl,
            br#"{"task_id":"t","status":"failure","error":"x","cost_usd":0,"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}
"#,
        )
        .unwrap();
        // Back-date mtime past the staleness threshold.
        let five_h_ago =
            SystemTime::now() - Duration::from_secs(STALENESS_THRESHOLD.as_secs() + 3600);
        let f = fs::OpenOptions::new().write(true).open(&jsonl).unwrap();
        f.set_modified(five_h_ago).unwrap();
        drop(f);

        let entry = collect_run_entry(&run_dir, "run-stale".to_string(), SystemTime::now());
        assert_eq!(
            entry.status,
            RunStatus::Stale,
            "old jsonl + no socket should classify as Stale"
        );
    }

    #[test]
    fn run_status_terminal_helpers() {
        assert!(RunStatus::Complete.is_terminal());
        assert!(RunStatus::Stale.is_terminal());
        assert!(RunStatus::Aborted.is_terminal());
        assert!(!RunStatus::Running.is_terminal());

        assert!(RunStatus::Complete.is_complete());
        assert!(!RunStatus::Stale.is_complete());
        assert!(!RunStatus::Aborted.is_complete());
        assert!(!RunStatus::Running.is_complete());
    }

    #[test]
    fn run_status_labels_are_distinct() {
        let labels = [
            RunStatus::Complete.label(),
            RunStatus::Running.label(),
            RunStatus::Stale.label(),
            RunStatus::Aborted.label(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            labels.len(),
            "every variant must have a distinct label"
        );
    }
}
