//! `pitboss status <run-id>` — snapshot view of all task records for a run.
//!
//! Reads `summary.jsonl` (in-flight or completed run) and prints a table.
//! With `--json` emits a JSON array of task records instead.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use pitboss_core::store::TaskStatus;

/// Entry point for the `status` subcommand.
pub fn run(
    run_id_prefix: &str,
    json: bool,
    run_dir_override: Option<PathBuf>,
    resources: bool,
) -> Result<i32> {
    let base = run_dir_override.unwrap_or_else(default_runs_dir);
    let run_dir = resolve_run_dir(&base, run_id_prefix)?;

    let summary_json = run_dir.join("summary.json");
    let summary_jsonl = run_dir.join("summary.jsonl");

    if !summary_json.exists() && !summary_jsonl.exists() {
        bail!(
            "no summary found in {}; is the run still starting up?",
            run_dir.display()
        );
    }

    // Canonical reader (#437 + #438): consume the unified replay stream
    // and bin items back into `(records, notify_failures)`. `notify_failures`
    // only appears on a finalized run (`Finalized` lifecycle); in-flight
    // runs report None — that's correct (the count is only authoritative
    // at finalize), and the journaled `notifications.jsonl` is still
    // available for live debugging.
    let (records, notify_failures) = collect_replay(&run_dir)?;

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
    let denied_counts = count_denials_per_task(&run_dir, &records);
    render_status(
        &mut stdout,
        &run_name,
        &records,
        notify_failures,
        &denied_counts,
    )?;

    // #553: optional resource summary block from
    // `summary.json::resource_high_water`. Skipped when `--json` is
    // set (caller can read the field directly off the JSON output)
    // and when the run was sampled at cadence=0 (no high-water data
    // present). Renders a compact human-readable block — total peak,
    // cgroup ceiling, top-3 actors — so the operator sees the OOM
    // story without grepping summary.json.
    if resources {
        if let Some(hw) = load_resource_high_water(&summary_json) {
            render_resource_high_water(&mut stdout, &hw)?;
        } else {
            writeln!(
                &mut stdout,
                "\nresource_high_water: unavailable (resource_sample_secs=0, \
                 darwin host, or run finalized before first sample)"
            )?;
        }
    }

    Ok(0)
}

/// Read `summary.json` and pull off the `resource_high_water` block.
/// Returns `None` when the file doesn't exist (in-flight run) or when
/// the field is absent (sampling disabled / unsupported / pre-#553).
fn load_resource_high_water(
    summary_json: &Path,
) -> Option<pitboss_core::store::record::ResourceHighWater> {
    let bytes = std::fs::read(summary_json).ok()?;
    let summary: pitboss_core::store::RunSummary = serde_json::from_slice(&bytes).ok()?;
    summary.resource_high_water
}

fn render_resource_high_water<W: Write>(
    out: &mut W,
    hw: &pitboss_core::store::record::ResourceHighWater,
) -> Result<()> {
    let total_mb = (hw.total_rss_bytes_max as f64) / (1024.0 * 1024.0);
    let limit_str = match hw.cgroup_memory_max_bytes {
        Some(max) => {
            let max_mb = (max as f64) / (1024.0 * 1024.0);
            format!("{max_mb:.0} MB")
        }
        None => "host".to_string(),
    };
    let pct_str = match hw.peak_utilization_pct {
        Some(p) => format!("{:.0}%", p * 100.0),
        None => "—".to_string(),
    };
    writeln!(out)?;
    writeln!(
        out,
        "RESOURCE HIGH-WATER ({} samples · cadence {}s)",
        hw.sample_count, hw.sample_cadence_secs
    )?;
    writeln!(
        out,
        "  total RSS peak: {total_mb:.0} MB / {limit_str} ({pct_str})"
    )?;
    // Top-3 actors by RSS peak. `BTreeMap` iteration order is by key
    // — sort by value to surface the actual hotspots first.
    let mut by_actor: Vec<(&String, &u64)> = hw.rss_bytes_max_by_actor.iter().collect();
    by_actor.sort_by(|a, b| b.1.cmp(a.1));
    for (actor_id, peak) in by_actor.iter().take(3) {
        let peak_mb = (**peak as f64) / (1024.0 * 1024.0);
        writeln!(out, "    {actor_id:<32} {peak_mb:>6.0} MB")?;
    }
    Ok(())
}

/// Drain `open_run_stream(ReplayOnly)` into the snapshot shape this
/// renderer expects. The unified API replaces the legacy
/// `read_run_snapshot` call (#438 Step 4): same on-disk reader under
/// the hood, but routes the data through the consumer-facing stream so
/// `status` stays API-aligned with the TUI and SPA. Inline rather than
/// promoted to `pitboss-core` because `diff` is the only other site
/// and rule-of-three isn't met yet.
fn collect_replay(run_dir: &Path) -> Result<(Vec<pitboss_core::store::TaskRecord>, Option<u32>)> {
    use futures_util::StreamExt;
    use pitboss_core::stream::{open_run_stream, LifecycleEvent, RunStreamPayload, StreamMode};

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let items = rt.block_on(async {
        open_run_stream(run_dir, StreamMode::ReplayOnly)
            .collect::<Vec<_>>()
            .await
    });

    let mut records = Vec::with_capacity(items.len());
    let mut notify_failures = None;
    for item in items {
        match item.payload {
            RunStreamPayload::Task(t) => records.push(*t),
            RunStreamPayload::Lifecycle(LifecycleEvent::Finalized { summary }) => {
                notify_failures = summary.notify_failures;
            }
            // `ReplayOnly` never emits Started lifecycles or live Event
            // items; ignore both so a future stream-mode change is
            // additive rather than breaking. (#438)
            RunStreamPayload::Lifecycle(LifecycleEvent::Started { .. })
            | RunStreamPayload::Event(_) => {}
        }
    }
    Ok((records, notify_failures))
}

/// Count `tool_denied` rows in each task's `<run_dir>/tasks/<id>/events.jsonl`.
/// Returns a map keyed by `task_id`. Tasks with no jsonl file (quiet
/// actors, common case) are simply absent from the map. (#405)
///
/// The five-line `kind == "tool_denied"` discrimination is the stable
/// wire contract — same logic lives in `pitboss-tui/src/watcher.rs`'s
/// `read_denial_state`. Duplicated here rather than shared via
/// `pitboss-core` because the helper is too small to justify an
/// events-schema dep on core, and the wire-stable `kind` string is
/// what protects against drift.
fn count_denials_per_task(
    run_dir: &Path,
    records: &[pitboss_core::store::TaskRecord],
) -> std::collections::HashMap<String, u32> {
    use std::io::{BufRead, BufReader};
    let mut out = std::collections::HashMap::new();
    for rec in records {
        let path = run_dir
            .join("tasks")
            .join(&rec.task_id)
            .join("events.jsonl");
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut count: u32 = 0;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                continue;
            };
            if val.get("kind").and_then(|v| v.as_str()) == Some("tool_denied") {
                count = count.saturating_add(1);
            }
        }
        if count > 0 {
            out.insert(rec.task_id.clone(), count);
        }
    }
    out
}

/// Pure rendering helper, separated from I/O setup so the table + footer
/// can be exercised by tests without driving the binary or capturing
/// stdout. The non-positive-failure path (no router OR `Some(0)`) must
/// not emit a footer line — callers depend on the absence as a signal.
///
/// `denied_counts` is keyed by `task_id`; tasks absent from the map (or
/// mapped to 0) carry no denials. When the map is empty across the
/// whole run, the DENIED column is hidden entirely so the common-case
/// output stays tight (#405).
pub(crate) fn render_status<W: Write>(
    out: &mut W,
    run_name: &str,
    records: &[pitboss_core::store::TaskRecord],
    notify_failures: Option<u32>,
    denied_counts: &std::collections::HashMap<String, u32>,
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

    // Show the DENIED column only when at least one actor recorded a
    // Path-B denial. Empty map => column hidden; same conditional
    // discipline as the TUI tile counter (#405).
    let show_denied = denied_counts.values().any(|n| *n > 0);
    const DENIED_WIDTH: usize = 6;
    let sep_width = task_id_width
        + 1
        + 16
        + 1
        + 10
        + 1
        + 24
        + 1
        + 6
        + if show_denied { 1 + DENIED_WIDTH } else { 0 };

    if show_denied {
        writeln!(
            out,
            "{:<task_id_width$} {:<16} {:>10} {:<24} {:>6} {:>DENIED_WIDTH$}",
            "TASK_ID", "STATUS", "DURATION", "STARTED", "EXIT", "DENIED",
        )?;
    } else {
        writeln!(
            out,
            "{:<task_id_width$} {:<16} {:>10} {:<24} {:>6}",
            "TASK_ID", "STATUS", "DURATION", "STARTED", "EXIT",
        )?;
    }
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
        if show_denied {
            // Render "0" for absent tasks rather than "—" — the count is
            // an authoritative count, not an unknown value, and "0"
            // visually contrasts against the populated rows so an
            // operator sees which actors hit denials at a glance.
            let denied = denied_counts.get(&rec.task_id).copied().unwrap_or(0);
            writeln!(
                out,
                "{task_id:<task_id_width$} {status:<16} {duration:>10} {started:<24} {exit:>6} {denied:>DENIED_WIDTH$}",
            )?;
        } else {
            writeln!(
                out,
                "{task_id:<task_id_width$} {status:<16} {duration:>10} {started:<24} {exit:>6}",
            )?;
        }
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
    // Per-actor Path-B denial counts surface in the DENIED column
    // above (#405); per-`DeniedReasonKind` breakdown still lives on
    // `events.jsonl::tool_denied` rows and is left for `--json`
    // consumers / TUI Detail's RECENT DENIALS section.
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
            terminate_reason: None,
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

        let result = run(run_id, false, Some(tmp.path().to_path_buf()), false);
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn run_emits_json_with_flag() {
        let tmp = TempDir::new().unwrap();
        let run_id = "019da1bb-1234-5678-9abc-def012345678";
        let run_dir = tmp.path().join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();

        write_record(&run_dir, &make_record("worker-1", TaskStatus::Success));

        let result = run(run_id, true, Some(tmp.path().to_path_buf()), false);
        assert_eq!(result.unwrap(), 0);
    }

    /// `notify_failures` > 0 must surface as a footer line so operators
    /// see it without `cat`-ing notifications.jsonl by hand. This is the
    /// observability gap that #329 (ISSUE-notify-prior-4) flagged.
    #[test]
    fn render_status_emits_notify_failures_footer_when_positive() {
        let recs = vec![make_record("w-1", TaskStatus::Success)];
        let mut buf = Vec::new();
        render_status(
            &mut buf,
            "test-run",
            &recs,
            Some(3),
            &std::collections::HashMap::new(),
        )
        .unwrap();
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
        render_status(
            &mut buf,
            "test-run",
            &recs,
            Some(0),
            &std::collections::HashMap::new(),
        )
        .unwrap();
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
        render_status(
            &mut buf,
            "test-run",
            &recs,
            None,
            &std::collections::HashMap::new(),
        )
        .unwrap();
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
        render_status(
            &mut buf,
            "test-run",
            &recs,
            None,
            &std::collections::HashMap::new(),
        )
        .unwrap();
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
        render_status(
            &mut buf,
            "test-run",
            &recs,
            None,
            &std::collections::HashMap::new(),
        )
        .unwrap();
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
        render_status(
            &mut buf,
            "test-run",
            &recs,
            None,
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("Approvals: 4 requested, 0 approved, 4 rejected"),
            "approvals footer missing for rejection-only run in:\n{out}"
        );
    }

    /// When no actor in the run has Path-B denials, the DENIED column is
    /// hidden entirely so the common-case output stays unchanged. Pin
    /// the absence so a future "always-show" refactor that breaks
    /// downstream parsers gets caught. (#405)
    #[test]
    fn render_status_omits_denied_column_when_all_zero() {
        let recs = vec![
            make_record("w-1", TaskStatus::Success),
            make_record("w-2", TaskStatus::Success),
        ];
        let mut buf = Vec::new();
        render_status(
            &mut buf,
            "test-run",
            &recs,
            None,
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("DENIED"),
            "DENIED column must be hidden when no actor has denials:\n{out}"
        );
    }

    /// When at least one actor recorded a Path-B denial, the DENIED
    /// column appears; absent tasks render as "0", populated tasks
    /// render the count right-aligned. The signal is per-actor so an
    /// operator can see at a glance which workers hit denials. (#405)
    #[test]
    fn render_status_emits_denied_column_when_any_positive() {
        let recs = vec![
            make_record("writer-1", TaskStatus::Success),
            make_record("writer-2", TaskStatus::Failed),
            make_record("reader-1", TaskStatus::Success),
        ];
        let mut counts = std::collections::HashMap::new();
        counts.insert("writer-1".to_string(), 3);
        counts.insert("writer-2".to_string(), 7);
        // reader-1 has no denials — should render as "0", not be hidden.

        let mut buf = Vec::new();
        render_status(&mut buf, "test-run", &recs, None, &counts).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert!(
            out.contains("DENIED"),
            "DENIED column heading missing in:\n{out}"
        );
        // Per-row counts. Right-aligned in 6-col field, so look for the
        // task-id followed by the row content and a final "    3"
        // (4 spaces padding + count).
        let writer1_line = out
            .lines()
            .find(|l| l.starts_with("writer-1"))
            .expect("writer-1 row missing");
        assert!(
            writer1_line.trim_end().ends_with("3"),
            "writer-1 row should end with denied count 3, got: {writer1_line:?}"
        );
        let writer2_line = out
            .lines()
            .find(|l| l.starts_with("writer-2"))
            .expect("writer-2 row missing");
        assert!(
            writer2_line.trim_end().ends_with("7"),
            "writer-2 row should end with denied count 7, got: {writer2_line:?}"
        );
        let reader_line = out
            .lines()
            .find(|l| l.starts_with("reader-1"))
            .expect("reader-1 row missing");
        assert!(
            reader_line.trim_end().ends_with("0"),
            "reader-1 row (no denials) should render as 0, got: {reader_line:?}"
        );
    }

    /// `count_denials_per_task` reads each task's events.jsonl and
    /// counts only `kind == "tool_denied"` rows. Tasks with no jsonl
    /// file (quiet actors) are absent from the result map; tasks with
    /// jsonl but no denials don't appear either (the rendering helper
    /// treats absent as zero).
    #[test]
    fn count_denials_per_task_reads_jsonl_per_task() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path();
        let mk = |id: &str, body: &str| {
            let dir = run_dir.join("tasks").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("events.jsonl"), body).unwrap();
        };
        mk(
            "noisy",
            "{\"kind\":\"tool_denied\",\"at\":\"2026-05-08T00:00:00Z\",\"tool_name\":\"Bash\",\"actor_id\":\"noisy\",\"reason_kind\":\"denied_by_rule\",\"reason\":\"x\"}\n\
             {\"kind\":\"pause\",\"at\":\"2026-05-08T00:00:01Z\"}\n\
             {\"kind\":\"tool_denied\",\"at\":\"2026-05-08T00:00:02Z\",\"tool_name\":\"Write\",\"actor_id\":\"noisy\",\"reason_kind\":\"operator_rejected\",\"reason\":\"y\"}\n",
        );
        mk(
            "approved-only",
            "{\"kind\":\"approval_response\",\"at\":\"2026-05-08T00:00:00Z\",\"request_id\":\"r1\",\"approved\":true,\"edited\":false}\n",
        );
        // "quiet" has no events.jsonl at all.

        let recs = vec![
            make_record("noisy", TaskStatus::Success),
            make_record("approved-only", TaskStatus::Success),
            make_record("quiet", TaskStatus::Success),
        ];
        let counts = count_denials_per_task(run_dir, &recs);
        assert_eq!(counts.get("noisy"), Some(&2));
        assert!(
            !counts.contains_key("approved-only"),
            "task with jsonl but no tool_denied rows must be absent from map"
        );
        assert!(
            !counts.contains_key("quiet"),
            "task with no jsonl must be absent from map"
        );
    }

    /// `--resources` block reads `summary.json::resource_high_water`
    /// and prints a compact peak summary with the top actors by RSS.
    /// (#553)
    #[test]
    fn render_resource_high_water_includes_peak_and_top_actors() {
        let mut hw = pitboss_core::store::record::ResourceHighWater::default();
        hw.total_rss_bytes_max = 1_500 * 1024 * 1024; // 1500 MB
        hw.cgroup_memory_max_bytes = Some(2_000 * 1024 * 1024); // 2000 MB
        hw.peak_utilization_pct = Some(0.75);
        hw.sample_count = 100;
        hw.sample_cadence_secs = 5;
        hw.rss_bytes_max_by_actor.insert("worker-a".into(), 800 * 1024 * 1024);
        hw.rss_bytes_max_by_actor.insert("worker-b".into(), 400 * 1024 * 1024);
        hw.rss_bytes_max_by_actor.insert("worker-c".into(), 100 * 1024 * 1024);
        let mut buf = Vec::new();
        render_resource_high_water(&mut buf, &hw).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("100 samples"), "sample count: {out}");
        assert!(out.contains("cadence 5s"), "cadence: {out}");
        assert!(out.contains("1500 MB / 2000 MB"), "totals: {out}");
        assert!(out.contains("75%"), "pct: {out}");
        // Top-3 by RSS, sorted descending.
        let a = out.find("worker-a").unwrap();
        let b = out.find("worker-b").unwrap();
        let c = out.find("worker-c").unwrap();
        assert!(a < b && b < c, "top-3 must be sorted by peak desc: {out}");
    }

    #[test]
    fn render_resource_high_water_handles_missing_cgroup() {
        let mut hw = pitboss_core::store::record::ResourceHighWater::default();
        hw.total_rss_bytes_max = 1024 * 1024 * 1024;
        hw.cgroup_memory_max_bytes = None; // flat host
        hw.sample_count = 1;
        let mut buf = Vec::new();
        render_resource_high_water(&mut buf, &hw).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("1024 MB / host"), "flat denom: {out}");
        assert!(out.contains("—"), "pct fallback when None: {out}");
    }
}
