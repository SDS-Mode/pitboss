//! `pitboss prune` — sweep orphaned run directories.
//!
//! A run is *orphaned* when its dispatcher exited uncleanly (`SIGKILL`,
//! OOM, segfault, host crash) and never finalized `summary.json`. The
//! v0.9 [`crate::runs`] classifier already flags these as `Stale`
//! (and, for runs that never produced any output, `Aborted`). This
//! module turns that classification into action: either
//!
//! * **synthesize a Cancelled `summary.json`** (default) — preserves
//!   whatever partial state landed in `summary.jsonl` so the run is
//!   still inspectable / resumable; or
//! * **remove the run directory entirely** (with `--remove`) —
//!   reclaims disk and cleans up the leftover socket file under
//!   `$XDG_RUNTIME_DIR/pitboss/`.
//!
//! Defaults to dry-run; `--apply` commits the action. `--older-than`
//! filters by mtime so a fresh `kill -KILL` two minutes ago doesn't
//! get swept while the operator is still investigating.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use pitboss_core::store::{RunMeta, RunSummary};

use crate::runs::{collect_run_entries, runs_base_dir, RunEntry, RunStatus};

/// What action prune will take on each candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneAction {
    /// Write a synthesized `summary.json` reflecting the partial state
    /// in `summary.jsonl` (or an empty summary when no jsonl exists).
    /// `was_interrupted` is set so consumers can distinguish synthesized
    /// summaries from cleanly-finalized ones.
    SynthesizeSummary,
    /// `rm -rf` the run dir and unlink the leftover control socket.
    Remove,
}

impl PruneAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::SynthesizeSummary => "synthesize Cancelled summary.json",
            Self::Remove => "remove run directory",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Without `apply`, prune only reports what *would* happen.
    pub apply: bool,
    /// Choose [`PruneAction::Remove`] over the default
    /// [`PruneAction::SynthesizeSummary`].
    pub remove: bool,
    /// Only target runs older than this. `None` = no age floor.
    pub older_than: Option<Duration>,
    /// Also include runs in [`RunStatus::Aborted`]. By default only
    /// `Stale` is matched, per the roadmap spec.
    pub include_aborted: bool,
    /// Override the runs base dir (used by tests; in production this
    /// is `~/.local/share/pitboss/runs`).
    pub runs_dir: Option<PathBuf>,
}

impl PruneOptions {
    pub fn action(&self) -> PruneAction {
        if self.remove {
            PruneAction::Remove
        } else {
            PruneAction::SynthesizeSummary
        }
    }
}

#[derive(Debug, Clone)]
pub struct PruneCandidate {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub status: RunStatus,
    pub age: Duration,
    pub tasks_total: usize,
    pub tasks_failed: usize,
}

#[derive(Debug)]
pub struct PruneOutcome {
    pub run_id: String,
    pub result: Result<()>,
}

#[derive(Debug)]
pub struct PruneReport {
    pub action: PruneAction,
    pub apply: bool,
    pub candidates: Vec<PruneCandidate>,
    pub outcomes: Vec<PruneOutcome>,
}

impl PruneReport {
    pub fn success_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_ok()).count()
    }

    pub fn failure_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_err()).count()
    }
}

/// Run the prune sweep. Pure orchestration — all FS effects flow
/// through the sub-helpers so tests can drive every branch with a
/// `tempdir` runs base.
pub fn run(opts: &PruneOptions) -> PruneReport {
    let base = opts.runs_dir.clone().unwrap_or_else(runs_base_dir);
    let entries = collect_run_entries(&base);
    let candidates = filter_candidates(&entries, opts, SystemTime::now());
    let action = opts.action();

    let mut outcomes: Vec<PruneOutcome> = Vec::new();
    if opts.apply {
        for c in &candidates {
            let result = match action {
                PruneAction::SynthesizeSummary => synthesize_summary(&c.run_dir),
                PruneAction::Remove => remove_run(&c.run_id, &c.run_dir),
            };
            outcomes.push(PruneOutcome {
                run_id: c.run_id.clone(),
                result,
            });
        }
    }

    PruneReport {
        action,
        apply: opts.apply,
        candidates,
        outcomes,
    }
}

/// Pure candidate filter — split out so tests can drive every branch
/// of the matching logic without touching the filesystem.
fn filter_candidates(
    entries: &[RunEntry],
    opts: &PruneOptions,
    now: SystemTime,
) -> Vec<PruneCandidate> {
    entries
        .iter()
        .filter(|e| matches_status(e.status, opts.include_aborted))
        .filter_map(|e| {
            let age = now.duration_since(e.mtime).unwrap_or_default();
            if let Some(min) = opts.older_than {
                if age < min {
                    return None;
                }
            }
            Some(PruneCandidate {
                run_id: e.run_id.clone(),
                run_dir: e.run_dir.clone(),
                status: e.status,
                age,
                tasks_total: e.tasks_total,
                tasks_failed: e.tasks_failed,
            })
        })
        .collect()
}

fn matches_status(s: RunStatus, include_aborted: bool) -> bool {
    match s {
        RunStatus::Stale => true,
        RunStatus::Aborted => include_aborted,
        // Never sweep live or finalized runs.
        RunStatus::Running | RunStatus::Complete => false,
    }
}

fn synthesize_summary(run_dir: &Path) -> Result<()> {
    let summary = build_synthesized_summary(run_dir)?;
    let json = serde_json::to_string_pretty(&summary).context("serialize synthesized summary")?;
    let path = run_dir.join("summary.json");
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Build a [`RunSummary`] for a run that never finalized.
///
/// Preference order:
/// 1. If `summary.jsonl` exists, defer to [`crate::diff::load_summary`]
///    which preserves the partial task records and sets
///    `was_interrupted = true`.
/// 2. Otherwise (Aborted runs that never produced any rows), synthesize
///    a placeholder from `meta.json` alone — `tasks_total = 0`,
///    `was_interrupted = true`. `ended_at = now()`.
fn build_synthesized_summary(run_dir: &Path) -> Result<RunSummary> {
    if run_dir.join("summary.jsonl").exists() {
        return crate::diff::load_summary(run_dir)
            .with_context(|| format!("load partial summary for {}", run_dir.display()));
    }
    let meta_path = run_dir.join("meta.json");
    let bytes = std::fs::read(&meta_path)
        .with_context(|| format!("synthesize: read {}", meta_path.display()))?;
    let meta: RunMeta = serde_json::from_slice(&bytes)
        .with_context(|| format!("synthesize: parse {}", meta_path.display()))?;
    let ended = chrono::Utc::now();
    Ok(RunSummary {
        run_id: meta.run_id,
        manifest_path: meta.manifest_path,
        pitboss_version: meta.pitboss_version,
        claude_version: meta.claude_version,
        started_at: meta.started_at,
        ended_at: ended,
        total_duration_ms: (ended - meta.started_at).num_milliseconds(),
        tasks_total: 0,
        tasks_failed: 0,
        was_interrupted: true,
        tasks: Vec::new(),
    })
}

fn remove_run(run_id: &str, run_dir: &Path) -> Result<()> {
    std::fs::remove_dir_all(run_dir)
        .with_context(|| format!("remove_dir_all {}", run_dir.display()))?;
    // Best-effort cleanup of the leftover XDG socket file. Failure is
    // not fatal — the dir is already gone.
    if let Some(sock) = leftover_socket_path(run_id) {
        let _ = std::fs::remove_file(sock);
    }
    Ok(())
}

fn leftover_socket_path(run_id: &str) -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(
        PathBuf::from(xdg)
            .join("pitboss")
            .join(format!("{run_id}.control.sock")),
    )
}

/// Render the report as a human-readable string for stdout. The format
/// is stable enough to be grep-able but explicitly not machine-parsable
/// — a JSON output mode is roadmapped if/when it's needed.
pub fn render_report(report: &PruneReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mode = if report.apply { "apply" } else { "dry-run" };
    let _ = writeln!(
        out,
        "pitboss prune ({mode}) — action: {}",
        report.action.label()
    );

    if report.candidates.is_empty() {
        out.push_str("\nNo orphaned runs to prune.\n");
        return out;
    }

    let _ = writeln!(out, "\nMatched {} run(s):", report.candidates.len());
    for c in &report.candidates {
        let _ = writeln!(
            out,
            "  {}  state={:<8} age={:<8} tasks={} failed={}",
            short_id(&c.run_id),
            c.status.label(),
            format_duration(c.age),
            c.tasks_total,
            c.tasks_failed,
        );
    }

    if report.apply {
        let _ = writeln!(
            out,
            "\nResults: {} succeeded, {} failed.",
            report.success_count(),
            report.failure_count(),
        );
        for o in &report.outcomes {
            match &o.result {
                Ok(()) => {
                    let _ = writeln!(out, "  ✓ {}", short_id(&o.run_id));
                }
                Err(e) => {
                    let _ = writeln!(out, "  ✗ {}: {}", short_id(&o.run_id), e);
                }
            }
        }
    } else {
        out.push_str("\nDry-run only. Pass --apply to commit.\n");
    }
    out
}

fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86400 {
        format!("{}d", secs / 86400)
    } else if secs >= 3600 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

/// Parse `"24h"` / `"1d"` / `"30m"` / `"60s"` (or bare `"3600"` =
/// seconds) into a [`Duration`]. Used by clap's `value_parser`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty duration".into());
    }
    let split_at = trimmed.find(|c: char| !c.is_ascii_digit());
    let (num_str, unit) = match split_at {
        Some(i) => trimmed.split_at(i),
        // No suffix → treat as bare seconds.
        None => (trimmed, "s"),
    };
    let n: u64 = num_str
        .parse()
        .map_err(|e| format!("bad number {num_str:?}: {e}"))?;
    let secs = match unit {
        "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86400),
        other => return Err(format!("unknown duration unit {other:?}; expected s/m/h/d")),
    };
    Ok(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn fake_entry(id: &str, status: RunStatus, mtime: SystemTime, run_dir: &Path) -> RunEntry {
        RunEntry {
            run_id: id.to_string(),
            run_dir: run_dir.to_path_buf(),
            mtime,
            tasks_total: 0,
            tasks_failed: 0,
            status,
        }
    }

    // ── parse_duration ──────────────────────────────────────────────────

    #[test]
    fn parse_duration_accepts_named_units() {
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
    }

    #[test]
    fn parse_duration_treats_bare_number_as_seconds() {
        assert_eq!(parse_duration("3600").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn parse_duration_rejects_unknown_unit() {
        assert!(parse_duration("5y").is_err());
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    // ── filter_candidates ────────────────────────────────────────────────

    #[test]
    fn filter_matches_stale_by_default_skips_aborted() {
        let now = SystemTime::now();
        let tmp = TempDir::new().unwrap();
        let entries = vec![
            fake_entry("a", RunStatus::Stale, now, tmp.path()),
            fake_entry("b", RunStatus::Aborted, now, tmp.path()),
            fake_entry("c", RunStatus::Running, now, tmp.path()),
            fake_entry("d", RunStatus::Complete, now, tmp.path()),
        ];
        let opts = PruneOptions {
            apply: false,
            remove: false,
            older_than: None,
            include_aborted: false,
            runs_dir: None,
        };
        let cands = filter_candidates(&entries, &opts, now);
        let ids: Vec<&str> = cands.iter().map(|c| c.run_id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn filter_include_aborted_expands() {
        let now = SystemTime::now();
        let tmp = TempDir::new().unwrap();
        let entries = vec![
            fake_entry("a", RunStatus::Stale, now, tmp.path()),
            fake_entry("b", RunStatus::Aborted, now, tmp.path()),
            fake_entry("c", RunStatus::Running, now, tmp.path()),
        ];
        let opts = PruneOptions {
            apply: false,
            remove: false,
            older_than: None,
            include_aborted: true,
            runs_dir: None,
        };
        let cands = filter_candidates(&entries, &opts, now);
        let ids: Vec<&str> = cands.iter().map(|c| c.run_id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
    }

    #[test]
    fn filter_older_than_excludes_recent() {
        let now = SystemTime::now();
        let tmp = TempDir::new().unwrap();
        let entries = vec![
            // 5h old — passes a 4h threshold.
            fake_entry(
                "old",
                RunStatus::Stale,
                now - Duration::from_secs(5 * 3600),
                tmp.path(),
            ),
            // 1h old — does not pass a 4h threshold.
            fake_entry(
                "fresh",
                RunStatus::Stale,
                now - Duration::from_secs(3600),
                tmp.path(),
            ),
        ];
        let opts = PruneOptions {
            apply: false,
            remove: false,
            older_than: Some(Duration::from_secs(4 * 3600)),
            include_aborted: false,
            runs_dir: None,
        };
        let cands = filter_candidates(&entries, &opts, now);
        let ids: Vec<&str> = cands.iter().map(|c| c.run_id.as_str()).collect();
        assert_eq!(ids, vec!["old"]);
    }

    // ── synthesize_summary (FS path) ─────────────────────────────────────

    fn write_meta(run_dir: &Path) -> uuid::Uuid {
        use chrono::Utc;
        use std::collections::HashMap;
        let run_id = uuid::Uuid::now_v7();
        let meta = RunMeta {
            run_id,
            manifest_path: PathBuf::from("/tmp/manifest.toml"),
            pitboss_version: "test".to_string(),
            claude_version: None,
            started_at: Utc::now(),
            env: HashMap::new(),
        };
        let bytes = serde_json::to_vec(&meta).unwrap();
        fs::write(run_dir.join("meta.json"), bytes).unwrap();
        run_id
    }

    #[test]
    fn synthesize_with_jsonl_preserves_partial_records() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("run-x");
        fs::create_dir_all(&run_dir).unwrap();
        write_meta(&run_dir);
        // One settled task in the jsonl.
        fs::write(
            run_dir.join("summary.jsonl"),
            br#"{"task_id":"t1","status":"Failed","exit_code":1,"started_at":"2026-04-25T10:00:00Z","ended_at":"2026-04-25T10:00:30Z","duration_ms":30000,"worktree_path":null,"log_path":"/dev/null","token_usage":{"input":0,"output":0,"cache_read":0,"cache_creation":0},"claude_session_id":null,"final_message_preview":null}
"#,
        )
        .unwrap();

        synthesize_summary(&run_dir).expect("synthesize should succeed");
        let bytes = fs::read(run_dir.join("summary.json")).unwrap();
        let summary: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(summary.tasks_total, 1);
        assert_eq!(summary.tasks_failed, 1);
        assert!(summary.was_interrupted);
        assert_eq!(summary.tasks[0].task_id, "t1");
    }

    #[test]
    fn synthesize_without_jsonl_writes_empty_summary_from_meta() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("run-aborted");
        fs::create_dir_all(&run_dir).unwrap();
        let expected_id = write_meta(&run_dir);

        synthesize_summary(&run_dir).expect("synthesize should succeed");
        let bytes = fs::read(run_dir.join("summary.json")).unwrap();
        let summary: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(summary.run_id, expected_id);
        assert_eq!(summary.tasks_total, 0);
        assert!(summary.was_interrupted);
        assert!(summary.tasks.is_empty());
    }

    #[test]
    fn synthesize_without_meta_or_jsonl_returns_error() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("run-empty");
        fs::create_dir_all(&run_dir).unwrap();
        // No meta.json, no summary.jsonl. Synthesis must fail loudly so
        // the caller can report it rather than silently producing junk.
        assert!(synthesize_summary(&run_dir).is_err());
    }

    // ── remove_run (FS path) ─────────────────────────────────────────────

    #[test]
    fn remove_deletes_run_dir() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("run-x");
        fs::create_dir_all(run_dir.join("tasks/t1")).unwrap();
        fs::write(run_dir.join("summary.jsonl"), b"hello").unwrap();
        assert!(run_dir.exists());
        remove_run("run-x", &run_dir).unwrap();
        assert!(!run_dir.exists());
    }

    // ── End-to-end run() with a real runs base ───────────────────────────

    #[test]
    fn run_dry_run_does_not_mutate() {
        let tmp = TempDir::new().unwrap();
        let runs_base = tmp.path();

        // Fabricate a Stale-ish run: meta + jsonl + back-dated mtime,
        // no socket file → classifier returns Stale.
        let run_dir = runs_base.join("019c-stale");
        fs::create_dir_all(&run_dir).unwrap();
        write_meta(&run_dir);
        let jsonl = run_dir.join("summary.jsonl");
        fs::write(&jsonl, br#"{"task_id":"t","status":"Failed","exit_code":1,"started_at":"2026-04-20T00:00:00Z","ended_at":"2026-04-20T00:00:01Z","duration_ms":1000,"worktree_path":null,"log_path":"/dev/null","token_usage":{"input":0,"output":0,"cache_read":0,"cache_creation":0},"claude_session_id":null,"final_message_preview":null}
"#).unwrap();
        // Back-date jsonl mtime past the staleness threshold (4h).
        let f = fs::OpenOptions::new().write(true).open(&jsonl).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(5 * 3600))
            .unwrap();

        let opts = PruneOptions {
            apply: false,
            remove: false,
            older_than: None,
            include_aborted: false,
            runs_dir: Some(runs_base.to_path_buf()),
        };
        let report = run(&opts);
        assert_eq!(report.candidates.len(), 1);
        assert!(report.outcomes.is_empty(), "dry-run must not act");
        // No summary.json was written.
        assert!(!run_dir.join("summary.json").exists());
    }

    #[test]
    fn run_apply_synthesize_writes_summary() {
        let tmp = TempDir::new().unwrap();
        let runs_base = tmp.path();
        let run_dir = runs_base.join("019c-apply-syn");
        fs::create_dir_all(&run_dir).unwrap();
        write_meta(&run_dir);
        let jsonl = run_dir.join("summary.jsonl");
        fs::write(&jsonl, br#"{"task_id":"t","status":"Failed","exit_code":1,"started_at":"2026-04-20T00:00:00Z","ended_at":"2026-04-20T00:00:01Z","duration_ms":1000,"worktree_path":null,"log_path":"/dev/null","token_usage":{"input":0,"output":0,"cache_read":0,"cache_creation":0},"claude_session_id":null,"final_message_preview":null}
"#).unwrap();
        let f = fs::OpenOptions::new().write(true).open(&jsonl).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(5 * 3600))
            .unwrap();

        let opts = PruneOptions {
            apply: true,
            remove: false,
            older_than: None,
            include_aborted: false,
            runs_dir: Some(runs_base.to_path_buf()),
        };
        let report = run(&opts);
        assert_eq!(report.success_count(), 1);
        assert_eq!(report.failure_count(), 0);
        assert!(run_dir.join("summary.json").exists());
    }

    #[test]
    fn run_apply_remove_deletes_dir() {
        let tmp = TempDir::new().unwrap();
        let runs_base = tmp.path();
        let run_dir = runs_base.join("019c-apply-rm");
        fs::create_dir_all(&run_dir).unwrap();
        write_meta(&run_dir);
        let jsonl = run_dir.join("summary.jsonl");
        // Need at least one settled record so the classifier returns
        // Stale rather than Aborted (which would require
        // --include-aborted to match).
        fs::write(&jsonl, br#"{"task_id":"t","status":"Failed","exit_code":1,"started_at":"2026-04-20T00:00:00Z","ended_at":"2026-04-20T00:00:01Z","duration_ms":1000,"worktree_path":null,"log_path":"/dev/null","token_usage":{"input":0,"output":0,"cache_read":0,"cache_creation":0},"claude_session_id":null,"final_message_preview":null}
"#).unwrap();
        let f = fs::OpenOptions::new().write(true).open(&jsonl).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(5 * 3600))
            .unwrap();

        let opts = PruneOptions {
            apply: true,
            remove: true,
            older_than: None,
            include_aborted: false,
            runs_dir: Some(runs_base.to_path_buf()),
        };
        let report = run(&opts);
        assert_eq!(report.success_count(), 1);
        assert!(!run_dir.exists(), "run dir should have been removed");
    }

    // ── render_report ────────────────────────────────────────────────────

    #[test]
    fn render_report_empty_says_nothing_to_prune() {
        let report = PruneReport {
            action: PruneAction::SynthesizeSummary,
            apply: false,
            candidates: vec![],
            outcomes: vec![],
        };
        let s = render_report(&report);
        assert!(s.contains("No orphaned runs"));
    }

    #[test]
    fn render_report_dry_run_includes_apply_hint() {
        let tmp = TempDir::new().unwrap();
        let report = PruneReport {
            action: PruneAction::SynthesizeSummary,
            apply: false,
            candidates: vec![PruneCandidate {
                run_id: "019c5b00-0000-0000-0000-000000000000".to_string(),
                run_dir: tmp.path().to_path_buf(),
                status: RunStatus::Stale,
                age: Duration::from_secs(5 * 3600),
                tasks_total: 3,
                tasks_failed: 1,
            }],
            outcomes: vec![],
        };
        let s = render_report(&report);
        assert!(s.contains("Dry-run"));
        assert!(s.contains("--apply"));
        assert!(s.contains("019c5b00-000"), "should show short id");
        assert!(s.contains("stale"));
    }
}
