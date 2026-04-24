//! `pitboss status <run-id>` — snapshot view of all task records for a run.
//!
//! Reads `summary.jsonl` (in-flight or completed run) and prints a table.
//! With `--json` emits a JSON array of task records instead.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pitboss_core::store::TaskStatus;

/// Entry point for the `status` subcommand.
pub fn run(run_id_prefix: &str, json: bool, run_dir_override: Option<PathBuf>) -> Result<i32> {
    let base = run_dir_override.unwrap_or_else(default_runs_dir);
    let run_dir = resolve_run_dir(&base, run_id_prefix)?;

    // Prefer summary.json (finalized run) over summary.jsonl (in-flight).
    let summary_json = run_dir.join("summary.json");
    let summary_jsonl = run_dir.join("summary.jsonl");

    if !summary_json.exists() && !summary_jsonl.exists() {
        bail!(
            "no summary found in {}; is the run still starting up?",
            run_dir.display()
        );
    }

    let records = if summary_json.exists() {
        let bytes = std::fs::read(&summary_json)
            .with_context(|| format!("read {}", summary_json.display()))?;
        let summary: serde_json::Value = serde_json::from_slice(&bytes)?;
        summary
            .get("tasks")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(serde_json::from_value::<pitboss_core::store::TaskRecord>)
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let content = std::fs::read_to_string(&summary_jsonl)
            .with_context(|| format!("read {}", summary_jsonl.display()))?;
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str::<pitboss_core::store::TaskRecord>)
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parse {}", summary_jsonl.display()))?
    };

    if json {
        let out = serde_json::to_string_pretty(&records)?;
        println!("{out}");
        return Ok(0);
    }

    let mut stdout = std::io::stdout();
    let run_name = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| run_dir.display().to_string());
    writeln!(stdout, "Run: {run_name}")?;
    writeln!(
        stdout,
        "{:<30} {:<16} {:>10} {:<24} {:>6}",
        "TASK_ID", "STATUS", "DURATION", "STARTED", "EXIT"
    )?;
    writeln!(stdout, "{}", "-".repeat(90))?;

    for rec in &records {
        let status = status_label(&rec.status);
        let duration = format_duration(rec.duration_ms);
        let started = rec.started_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let exit = rec
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".to_string());
        writeln!(
            stdout,
            "{:<30} {:<16} {:>10} {:<24} {:>6}",
            &rec.task_id, status, duration, started, exit
        )?;
    }

    if records.is_empty() {
        writeln!(stdout, "(no tasks recorded yet)")?;
    } else {
        let total = records.len();
        let failed = records
            .iter()
            .filter(|r| !matches!(r.status, TaskStatus::Success))
            .count();
        writeln!(stdout, "{}", "-".repeat(90))?;
        writeln!(stdout, "Total: {total}  Failed: {failed}")?;
    }

    Ok(0)
}

fn status_label(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Success => "✓ Success",
        TaskStatus::Failed => "✗ Failed",
        TaskStatus::TimedOut => "⏱ TimedOut",
        TaskStatus::Cancelled => "⊘ Cancelled",
        TaskStatus::SpawnFailed => "! SpawnFailed",
        TaskStatus::ApprovalRejected => "⊘ ApprovalRej",
        TaskStatus::ApprovalTimedOut => "⏱ ApprovalTO",
    }
}

fn format_duration(ms: i64) -> String {
    if ms <= 0 {
        return "—".to_string();
    }
    let secs = ms / 1000;
    format!("{}m{:02}s", secs / 60, secs % 60)
}

fn default_runs_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".local/share/pitboss/runs")
}

fn resolve_run_dir(base: &Path, prefix: &str) -> Result<PathBuf> {
    if prefix.is_empty() {
        bail!("run id prefix must not be empty");
    }
    let entries = std::fs::read_dir(base)
        .with_context(|| format!("cannot read runs directory {}", base.display()))?;

    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.path().is_dir() && name.starts_with(prefix) {
            matches.push(entry.path());
        }
    }

    match matches.len() {
        0 => bail!(
            "no run found matching prefix '{}' in {}",
            prefix,
            base.display()
        ),
        1 => Ok(matches.remove(0)),
        n => bail!("{n} runs match prefix '{}' — be more specific", prefix),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitboss_core::store::{TaskRecord, TaskStatus};
    use std::io::Write;
    use tempfile::TempDir;

    fn write_record(dir: &Path, rec: &TaskRecord) {
        let jsonl_path = dir.join("summary.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&jsonl_path)
            .unwrap();
        writeln!(f, "{}", serde_json::to_string(rec).unwrap()).unwrap();
    }

    fn make_record(task_id: &str, status: TaskStatus) -> TaskRecord {
        use chrono::Utc;
        use pitboss_core::parser::TokenUsage;
        TaskRecord {
            task_id: task_id.to_string(),
            status,
            exit_code: Some(0),
            started_at: Utc::now(),
            ended_at: Utc::now(),
            duration_ms: 5000,
            worktree_path: None,
            log_path: PathBuf::from("/tmp/stdout.log"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
        }
    }

    #[test]
    fn resolve_run_dir_finds_prefix() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("019da1bb-aaaa-bbbb-cccc-dddddddddddd");
        std::fs::create_dir_all(&run_dir).unwrap();
        let found = resolve_run_dir(tmp.path(), "019da1bb").unwrap();
        assert_eq!(found, run_dir);
    }

    #[test]
    fn resolve_run_dir_errors_on_no_match() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_run_dir(tmp.path(), "deadbeef").unwrap_err();
        assert!(err.to_string().contains("no run found"), "{err}");
    }

    #[test]
    fn run_prints_table_for_jsonl() {
        let tmp = TempDir::new().unwrap();
        let run_id = "019da1bb-1234-5678-9abc-def012345678";
        let run_dir = tmp.path().join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();

        write_record(&run_dir, &make_record("worker-1", TaskStatus::Success));
        write_record(&run_dir, &make_record("worker-2", TaskStatus::Failed));

        let result = run(run_id, false, Some(tmp.path().to_path_buf()));
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn run_emits_json_with_flag() {
        let tmp = TempDir::new().unwrap();
        let run_id = "019da1bb-1234-5678-9abc-def012345678";
        let run_dir = tmp.path().join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();

        write_record(&run_dir, &make_record("worker-1", TaskStatus::Success));

        let result = run(run_id, true, Some(tmp.path().to_path_buf()));
        assert_eq!(result.unwrap(), 0);
    }
}
