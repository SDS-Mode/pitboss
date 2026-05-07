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

    // `notify_failures` only exists on a finalized `summary.json`; the
    // in-flight `summary.jsonl` is per-task records and never carries
    // run-scoped fields. Operators reading status during a live run see
    // no notify count — that's correct (the count is only authoritative
    // at finalize), and the journaled `notifications.jsonl` is still
    // available for live debugging.
    let (records, notify_failures) = if summary_json.exists() {
        let bytes = std::fs::read(&summary_json)
            .with_context(|| format!("read {}", summary_json.display()))?;
        let summary: serde_json::Value = serde_json::from_slice(&bytes)?;
        let notify_failures = summary
            .get("notify_failures")
            .and_then(|v| v.as_u64())
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
        let recs = summary
            .get("tasks")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(serde_json::from_value::<pitboss_core::store::TaskRecord>)
            .collect::<Result<Vec<_>, _>>()?;
        (recs, notify_failures)
    } else {
        let content = std::fs::read_to_string(&summary_jsonl)
            .with_context(|| format!("read {}", summary_jsonl.display()))?;
        let recs = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str::<pitboss_core::store::TaskRecord>)
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parse {}", summary_jsonl.display()))?;
        (recs, None)
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
    render_status(&mut stdout, &run_name, &records, notify_failures)?;

    Ok(0)
}

/// Pure rendering helper, separated from I/O setup so the table + footer
/// can be exercised by tests without driving the binary or capturing
/// stdout. The non-positive-failure path (no router OR `Some(0)`) must
/// not emit a footer line — callers depend on the absence as a signal.
pub(crate) fn render_status<W: Write>(
    out: &mut W,
    run_name: &str,
    records: &[pitboss_core::store::TaskRecord],
    notify_failures: Option<u32>,
) -> Result<()> {
    writeln!(out, "Run: {run_name}")?;

    // Size the TASK_ID column to fit the widest id (with a floor of 30 and
    // ceiling of 60) instead of a hard-coded 30-pad that silently overflows
    // into the STATUS column when sub-lead ids are long (#96).
    const TASK_ID_MIN: usize = 30;
    const TASK_ID_MAX: usize = 60;
    let observed_max = records
        .iter()
        .map(|r| r.task_id.chars().count())
        .max()
        .unwrap_or(0);
    let task_id_width = observed_max.clamp(TASK_ID_MIN, TASK_ID_MAX);
    let sep_width = task_id_width + 1 + 16 + 1 + 10 + 1 + 24 + 1 + 6;

    writeln!(
        out,
        "{:<task_id_width$} {:<16} {:>10} {:<24} {:>6}",
        "TASK_ID", "STATUS", "DURATION", "STARTED", "EXIT",
    )?;
    writeln!(out, "{}", "-".repeat(sep_width))?;

    for rec in records {
        let status = status_label(&rec.status);
        let duration = pitboss_core::fmt::format_duration_ms(rec.duration_ms);
        let started = rec.started_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let exit = rec
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".to_string());
        let task_id = pitboss_core::fmt::truncate_ellipsis(&rec.task_id, task_id_width);
        writeln!(
            out,
            "{task_id:<task_id_width$} {status:<16} {duration:>10} {started:<24} {exit:>6}",
        )?;
    }

    if records.is_empty() {
        writeln!(out, "(no tasks recorded yet)")?;
    } else {
        let total = records.len();
        let failed = records
            .iter()
            .filter(|r| !matches!(r.status, TaskStatus::Success))
            .count();
        writeln!(out, "{}", "-".repeat(sep_width))?;
        writeln!(out, "Total: {total}  Failed: {failed}")?;
    }

    // Approvals aggregate. `approvals_rejected` covers both Path A
    // (`request_approval` denials) and Path B (`permission_prompt`
    // denials) — per #367 the Path-B handler bumps the same counter.
    // Per-`DeniedReasonKind` breakdown lives on each actor's
    // `events.jsonl::tool_denied` rows; surfacing that requires reading
    // those files per-actor and is left for `--json` consumers / TUI.
    let approvals_requested: u32 = records.iter().map(|r| r.approvals_requested).sum();
    let approvals_approved: u32 = records.iter().map(|r| r.approvals_approved).sum();
    let approvals_rejected: u32 = records.iter().map(|r| r.approvals_rejected).sum();
    if approvals_requested > 0 || approvals_approved > 0 || approvals_rejected > 0 {
        writeln!(
            out,
            "Approvals: {approvals_requested} requested, {approvals_approved} approved, {approvals_rejected} rejected"
        )?;
    }

    // Surface notification emit failures only when the count is positive —
    // the common case (no router or no failures) stays uncluttered.
    // Operators who see this line should consult
    // `<run_dir>/notifications.jsonl` for the failing sink + error.
    if let Some(n) = notify_failures {
        if n > 0 {
            writeln!(out, "Notification failures: {n} (see notifications.jsonl)")?;
        }
    }

    Ok(())
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

fn default_runs_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".local/share/pitboss/runs")
}

fn resolve_run_dir(base: &Path, prefix: &str) -> Result<PathBuf> {
    crate::runs::resolve_run_dir_by_prefix(base, prefix)
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

    /// `notify_failures` > 0 must surface as a footer line so operators
    /// see it without `cat`-ing notifications.jsonl by hand. This is the
    /// observability gap that #329 (ISSUE-notify-prior-4) flagged.
    #[test]
    fn render_status_emits_notify_failures_footer_when_positive() {
        let recs = vec![make_record("w-1", TaskStatus::Success)];
        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, Some(3)).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Notification failures: 3 (see notifications.jsonl)"),
            "footer missing in:\n{out}"
        );
    }

    /// Zero failures with a wired router must not clutter the output —
    /// the footer is reserved for actionable signal. `Some(0)` is the
    /// "router was active and saw nothing" case.
    #[test]
    fn render_status_omits_notify_failures_footer_when_zero() {
        let recs = vec![make_record("w-1", TaskStatus::Success)];
        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, Some(0)).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Notification failures"),
            "spurious footer for zero count:\n{out}"
        );
    }

    /// `None` is the back-compat path (no router OR pre-`notify_failures`
    /// summary) and must produce identical output to `Some(0)` so that
    /// reading an old run doesn't print misleading text.
    #[test]
    fn render_status_omits_notify_failures_footer_when_unknown() {
        let recs = vec![make_record("w-1", TaskStatus::Success)];
        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, None).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Notification failures"),
            "spurious footer for None:\n{out}"
        );
    }

    fn make_record_with_approvals(
        task_id: &str,
        requested: u32,
        approved: u32,
        rejected: u32,
    ) -> TaskRecord {
        let mut r = make_record(task_id, TaskStatus::Success);
        r.approvals_requested = requested;
        r.approvals_approved = approved;
        r.approvals_rejected = rejected;
        r
    }

    /// When any approval counter is positive across the run, the footer
    /// must surface the aggregate so operators see Path B denial volume
    /// without piping through `--json`. The line shape is stable —
    /// downstream tooling may grep for "Approvals:".
    #[test]
    fn render_status_emits_approvals_footer_when_any_positive() {
        let recs = vec![
            make_record_with_approvals("w-1", 5, 4, 1),
            make_record_with_approvals("w-2", 3, 1, 2),
        ];
        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, None).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Approvals: 8 requested, 5 approved, 3 rejected"),
            "approvals footer missing or wrong shape in:\n{out}"
        );
    }

    /// All-zero approvals is the dominant case (no policy rules fired,
    /// no Path B routing) and must NOT print the footer — it would just
    /// be noise.
    #[test]
    fn render_status_omits_approvals_footer_when_all_zero() {
        let recs = vec![
            make_record_with_approvals("w-1", 0, 0, 0),
            make_record_with_approvals("w-2", 0, 0, 0),
        ];
        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, None).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("Approvals:"),
            "spurious approvals footer in:\n{out}"
        );
    }

    /// A run with rejections but zero approvals (e.g. every request hit
    /// `default_approval_policy = "auto_reject"`) must still show the
    /// footer — the rejected count is the actionable signal.
    #[test]
    fn render_status_emits_approvals_footer_when_only_rejected_positive() {
        let recs = vec![make_record_with_approvals("w-1", 4, 0, 4)];
        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, None).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Approvals: 4 requested, 0 approved, 4 rejected"),
            "approvals footer missing for rejection-only run in:\n{out}"
        );
    }
}
