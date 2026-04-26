//! `pitboss list` — flat-CLI inventory of runs, designed for orchestrators.
//!
//! The pitboss-tui binary already has a `pitboss-tui list` subcommand that
//! prints the same data, but orchestrators (RacerX, CI scripts) generally
//! don't want to depend on the TUI binary just for a directory walk. This
//! flat-CLI form gives them a stable surface to call:
//!
//! ```text
//! $ pitboss list
//! RUN ID                                  STARTED                 TASKS  FAILED  STATUS
//! ────────────────────────────────────────────────────────────────────────────────
//! 019d…                                   2026-04-26 17:00:00     5      0       complete
//! 019c…                                   2026-04-26 16:45:00     8      1       running
//!
//! $ pitboss list --active
//! ...only runs with status = "running"...
//!
//! $ pitboss list --json
//! [{"run_id":"019d…","status":"complete","tasks_total":5, ...}, ...]
//! ```
//!
//! Implementation reuses `crate::runs::collect_run_entries`, the same
//! classifier the TUI and `pitboss prune` already share (PR #130).

use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Result;
use serde::Serialize;

use crate::runs;

/// JSON shape emitted with `--json`. Stable contract for orchestrators —
/// one record per run directory under `~/.local/share/pitboss/runs/`.
#[derive(Debug, Serialize)]
pub struct RunListEntry {
    pub run_id: String,
    pub run_dir: PathBuf,
    /// Last modification time of the run dir, RFC 3339.
    pub started_at: String,
    pub tasks_total: usize,
    pub tasks_failed: usize,
    /// One of "complete", "running", "stale", "aborted".
    pub status: &'static str,
}

impl From<&runs::RunEntry> for RunListEntry {
    fn from(e: &runs::RunEntry) -> Self {
        Self {
            run_id: e.run_id.clone(),
            run_dir: e.run_dir.clone(),
            started_at: rfc3339(e.mtime),
            tasks_total: e.tasks_total,
            tasks_failed: e.tasks_failed,
            status: e.status.label(),
        }
    }
}

/// Entry point for the `list` subcommand.
///
/// `runs_dir_override` mirrors the same flag on `pitboss prune` — defaults
/// to `~/.local/share/pitboss/runs/` when `None`.
pub fn run(json: bool, active: bool, runs_dir_override: Option<PathBuf>) -> Result<i32> {
    let base = runs_dir_override.unwrap_or_else(runs::runs_base_dir);
    if !base.exists() {
        if json {
            println!("[]");
            return Ok(0);
        }
        eprintln!("No runs directory found at {}.", base.display());
        eprintln!("Run `pitboss dispatch` first to create a run.");
        return Ok(0);
    }

    let entries = runs::collect_run_entries(&base);
    let filtered: Vec<&runs::RunEntry> = if active {
        // `--active` strictly means "the dispatcher is alive and working
        // right now" → only `Running`. Stale runs LOOK active but the
        // dispatcher is gone; they belong to `pitboss prune --dry-run`.
        // Aborted runs never produced output. Both excluded.
        entries
            .iter()
            .filter(|e| matches!(e.status, runs::RunStatus::Running))
            .collect()
    } else {
        entries.iter().collect()
    };

    if json {
        let out: Vec<RunListEntry> = filtered.iter().map(|e| RunListEntry::from(*e)).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(0);
    }

    if filtered.is_empty() {
        if active {
            println!("No active runs.");
        } else {
            println!("No runs found under {}.", base.display());
        }
        return Ok(0);
    }

    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{:<38}  {:<22}  {:>6}  {:>6}  STATUS",
        "RUN ID", "STARTED", "TASKS", "FAILED"
    )?;
    writeln!(stdout, "{}", "─".repeat(80))?;
    for e in &filtered {
        let started = runs::format_mtime(e.mtime);
        writeln!(
            stdout,
            "{:<38}  {:<22}  {:>6}  {:>6}  {}",
            e.run_id,
            started,
            e.tasks_total,
            e.tasks_failed,
            e.status.label()
        )?;
    }
    Ok(0)
}

/// RFC 3339 timestamp for `--json` output. `format_mtime` in the runs
/// module returns a human "YYYY-MM-DD HH:MM:SS" form for table display;
/// JSON consumers want the parseable RFC 3339 form.
fn rfc3339(mtime: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = mtime.into();
    dt.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_run_dir(base: &std::path::Path, id: &str, with_summary: bool) -> PathBuf {
        let dir = base.join(id);
        fs::create_dir_all(&dir).unwrap();
        if with_summary {
            // Minimal valid summary.json for `Complete` classification.
            fs::write(
                dir.join("summary.json"),
                br#"{"run_id":"00000000-0000-0000-0000-000000000000","manifest_path":"/tmp/m","pitboss_version":"x","claude_version":null,"started_at":"2026-04-26T00:00:00Z","ended_at":"2026-04-26T00:00:01Z","total_duration_ms":1000,"tasks_total":3,"tasks_failed":1,"was_interrupted":false,"tasks":[]}"#,
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn list_default_lists_all_runs() {
        let tmp = TempDir::new().unwrap();
        make_run_dir(tmp.path(), "019d-aaaa", true);
        make_run_dir(tmp.path(), "019d-bbbb", true);
        let rc = run(false, false, Some(tmp.path().to_path_buf())).unwrap();
        assert_eq!(rc, 0);
    }

    #[test]
    fn list_json_emits_array_of_records() {
        let tmp = TempDir::new().unwrap();
        make_run_dir(tmp.path(), "019d-aaaa", true);
        // Snapshot stdout via a child run to avoid pulling in libtest's
        // capture infrastructure: just confirm the invocation succeeds and
        // that JSON serialization round-trips through RunListEntry.
        let entries = runs::collect_run_entries(tmp.path());
        let json: Vec<RunListEntry> = entries.iter().map(RunListEntry::from).collect();
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.starts_with('['));
        assert!(s.contains("\"run_id\":\"019d-aaaa\""));
        assert!(s.contains("\"status\":\"complete\""));
        assert!(s.contains("\"tasks_total\":3"));
    }

    #[test]
    fn list_active_excludes_complete_and_aborted() {
        let tmp = TempDir::new().unwrap();
        // Complete run.
        make_run_dir(tmp.path(), "019d-aaaa", true);
        // Aborted run (no summary.json, no jsonl, no socket).
        make_run_dir(tmp.path(), "019d-bbbb", false);

        let entries = runs::collect_run_entries(tmp.path());
        let active: Vec<_> = entries
            .iter()
            .filter(|e| matches!(e.status, runs::RunStatus::Running))
            .collect();
        // Neither directory should pass the active filter.
        assert!(
            active.is_empty(),
            "active filter should exclude complete + aborted"
        );
    }

    #[test]
    fn list_missing_runs_dir_returns_ok_with_empty_json() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let rc = run(true, false, Some(nonexistent)).unwrap();
        assert_eq!(rc, 0);
    }

    #[test]
    fn rfc3339_roundtrip_through_chrono() {
        let s = rfc3339(SystemTime::UNIX_EPOCH);
        assert!(s.starts_with("1970-01-01T00:00:00"));
    }
}
