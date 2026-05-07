//! `pitboss analyze` — triage view over per-run artifacts.
//!
//! Two modes, mutually exclusive at the CLI:
//!   * single-run — resolve a run id (full UUID or unique prefix) and
//!     produce a `RunAnalysis` (header / cost rollup / failure groups /
//!     task tree / hotspots).
//!   * cross-run (`--recent N`) — walk the most recent N runs under the
//!     standard runs base directory and produce a `RecentAnalysis`
//!     (per-run analyses + aggregates: failure histogram, model usage,
//!     outliers).
//!
//! Pure data-shaping over the existing `summary.json` / `summary.jsonl`
//! artifacts. No new disk schema, no log re-parsing — costs come from the
//! `cost_usd` field on each `TaskRecord`, with opportunistic recomputation
//! via `pitboss_core::prices::cost_usd` when a pre-v0.11 record carries a
//! known model but no cost estimate.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use pitboss_core::prices;
use pitboss_core::store::{FailureReason, RunSummary, TaskRecord, TaskStatus};
use serde::Serialize;

use crate::runs::{collect_run_entries, runs_base_dir, RunStatus};

/// Hard cap on `--recent` / `analyze_recent` to keep MCP responses and
/// CLI output bounded. A user passing 1000 wants "as many as you have";
/// we clamp to this and warn on the CLI path.
pub const MAX_RECENT_LIMIT: u32 = 50;

// ── Report types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct RunAnalysis {
    pub header: RunHeader,
    pub cost: CostRollup,
    pub failures: Vec<FailureGroup>,
    pub tasks: Vec<TaskRow>,
    pub hotspots: Hotspots,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunHeader {
    pub run_id: String,
    pub manifest_path: String,
    pub manifest_name: Option<String>,
    /// `"flat"` when the manifest declared `[[task]]`s, `"hierarchical"`
    /// when it declared `[lead]`. Detected via `resolved.json`'s `lead`
    /// field with a fallback to scanning task records for any
    /// `parent_task_id`.
    pub mode: String,
    /// Mirror of [`crate::runs::RunStatus`] — `"complete"` / `"running"`
    /// / `"stale"` / `"aborted"`. Reused so the operator vocabulary is
    /// consistent across `pitboss list` and `pitboss analyze`.
    pub run_status: String,
    pub was_interrupted: bool,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub total_duration_ms: i64,
    pub tasks_total: usize,
    pub tasks_failed: usize,
    pub pitboss_version: String,
    pub claude_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CostRollup {
    pub total_usd: f64,
    pub lead_usd: f64,
    pub worker_usd: f64,
    pub by_model: Vec<ModelRollup>,
    pub tasks_with_cost: usize,
    pub tasks_missing_cost: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRollup {
    /// Resolved model name, or `"unknown"` when the task record carries
    /// no model and `resolved.json` has no entry for the task id.
    pub model: String,
    pub task_count: usize,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureGroup {
    /// `FailureReason` discriminant (`rate_limit`, `network_error`,
    /// `auth_failure`, `context_exceeded`, `invalid_argument`,
    /// `unknown`) or `unclassified` for failures with no
    /// `failure_reason` field on the record.
    pub kind: String,
    pub count: usize,
    pub task_ids: Vec<String>,
    pub representative_excerpt: Option<String>,
    pub statuses: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRow {
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub model: Option<String>,
    pub cost_usd: Option<f64>,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    pub final_message_preview: Option<String>,
    pub failure_kind: Option<String>,
    pub pause_count: u32,
    pub reprompt_count: u32,
    pub approvals_requested: u32,
    pub approvals_approved: u32,
    pub approvals_rejected: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Hotspots {
    pub longest: Option<HotspotEntry>,
    pub costliest: Option<HotspotEntry>,
    pub most_tokens: Option<HotspotEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotspotEntry {
    pub task_id: String,
    pub value: f64,
    pub unit: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentAnalysis {
    pub runs: Vec<RunAnalysis>,
    pub aggregates: CrossRunAggregates,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CrossRunAggregates {
    pub run_count: usize,
    pub failed_run_count: usize,
    pub interrupted_run_count: usize,
    pub mode_flat: usize,
    pub mode_hierarchical: usize,
    pub earliest_started_at: Option<DateTime<Utc>>,
    pub latest_started_at: Option<DateTime<Utc>>,
    pub aggregate_duration_ms: i64,
    pub total_cost_usd: f64,
    pub failure_histogram: Vec<FailureHistogramEntry>,
    pub model_usage: Vec<ModelRollup>,
    pub slowest_runs: Vec<RunOutlier>,
    pub costliest_runs: Vec<RunOutlier>,
    pub most_failed_runs: Vec<RunOutlier>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailureHistogramEntry {
    pub kind: String,
    pub count: usize,
    pub example_run_id: String,
    pub example_task_id: String,
    pub example_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunOutlier {
    pub run_id: String,
    pub value: f64,
    pub unit: &'static str,
}

// ── Public API ───────────────────────────────────────────────────────────

/// Analyze a single run directory.
///
/// Reuses [`crate::diff::load_summary`] (prefers `summary.json`, falls
/// back to `summary.jsonl` for in-flight or interrupted runs) and
/// [`crate::diff::load_model_map`] for `task_id → model` resolution.
pub fn analyze_run_dir(run_dir: &Path) -> Result<RunAnalysis> {
    let summary = crate::diff::load_summary(run_dir)?;
    let model_map = crate::diff::load_model_map(run_dir);
    let mode = detect_mode(run_dir, &summary);
    let run_status = detect_run_status(run_dir);
    Ok(build_analysis(&summary, &model_map, mode, run_status))
}

/// Analyze the most recent `limit` runs under `base`. Caller is
/// responsible for passing a value already clamped to
/// [`MAX_RECENT_LIMIT`] when surfacing this through user-facing APIs;
/// we clamp here too as a defensive backstop.
pub fn analyze_recent_dirs(base: &Path, limit: u32, failed_only: bool) -> Result<RecentAnalysis> {
    let limit = limit.clamp(1, MAX_RECENT_LIMIT);
    let entries = collect_run_entries(base);
    let mut runs: Vec<RunAnalysis> = Vec::new();
    for entry in entries.into_iter() {
        if runs.len() >= limit as usize {
            break;
        }
        if failed_only && entry.tasks_failed == 0 {
            continue;
        }
        match analyze_run_dir(&entry.run_dir) {
            Ok(a) => runs.push(a),
            Err(e) => {
                tracing::warn!(
                    run_dir = %entry.run_dir.display(),
                    error = %e,
                    "analyze: skipping unreadable run"
                );
            }
        }
    }
    let aggregates = build_aggregates(&runs);
    Ok(RecentAnalysis { runs, aggregates })
}

// ── CLI entry ────────────────────────────────────────────────────────────

pub struct AnalyzeOpts {
    pub run_id: Option<String>,
    pub recent: Option<u32>,
    pub failed_only: bool,
    pub json: bool,
    pub run_dir: Option<PathBuf>,
}

pub fn run(opts: AnalyzeOpts) -> Result<i32> {
    let base = opts.run_dir.unwrap_or_else(runs_base_dir);
    match (opts.run_id.as_deref(), opts.recent) {
        (Some(prefix), None) => {
            let run_dir = crate::runs::resolve_run_dir_by_prefix(&base, prefix)?;
            let analysis = analyze_run_dir(&run_dir)?;
            emit(opts.json, &analysis, render_run_human)?;
            Ok(0)
        }
        (None, Some(limit_raw)) => {
            let limit = if limit_raw > MAX_RECENT_LIMIT {
                eprintln!("pitboss analyze: --recent {limit_raw} clamped to {MAX_RECENT_LIMIT}");
                MAX_RECENT_LIMIT
            } else {
                limit_raw.max(1)
            };
            let analysis = analyze_recent_dirs(&base, limit, opts.failed_only)?;
            emit(opts.json, &analysis, render_recent_human)?;
            Ok(0)
        }
        (Some(_), Some(_)) => bail!("pass either <run-id> or --recent, not both"),
        (None, None) => bail!("pass <run-id> or --recent"),
    }
}

fn emit<T: Serialize>(json: bool, value: &T, render: fn(&T) -> String) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        print!("{}", render(value));
    }
    Ok(())
}

// ── Single-run builder ───────────────────────────────────────────────────

fn detect_mode(run_dir: &Path, summary: &RunSummary) -> &'static str {
    if let Ok(bytes) = std::fs::read(run_dir.join("resolved.json")) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if v.get("lead").is_some_and(|l| !l.is_null()) {
                return "hierarchical";
            }
        }
    }
    if summary.tasks.iter().any(|t| t.parent_task_id.is_some()) {
        "hierarchical"
    } else {
        "flat"
    }
}

fn detect_run_status(run_dir: &Path) -> RunStatus {
    use std::time::SystemTime;
    let mtime = std::fs::metadata(run_dir)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::runs::collect_run_entry(run_dir, run_id, mtime).status
}

fn build_analysis(
    summary: &RunSummary,
    model_map: &HashMap<String, String>,
    mode: &'static str,
    run_status: RunStatus,
) -> RunAnalysis {
    let header = RunHeader {
        run_id: summary.run_id.to_string(),
        manifest_path: summary.manifest_path.display().to_string(),
        manifest_name: summary.manifest_name.clone(),
        mode: mode.to_string(),
        run_status: run_status.label().to_string(),
        was_interrupted: summary.was_interrupted,
        started_at: summary.started_at,
        ended_at: summary.ended_at,
        total_duration_ms: summary.total_duration_ms,
        tasks_total: summary.tasks_total,
        tasks_failed: summary.tasks_failed,
        pitboss_version: summary.pitboss_version.clone(),
        claude_version: summary.claude_version.clone(),
    };

    let lead_id = summary
        .tasks
        .iter()
        .find(|t| t.parent_task_id.is_none())
        .map(|t| t.task_id.clone());

    let mut tasks: Vec<TaskRow> = summary
        .tasks
        .iter()
        .map(|rec| task_row(rec, model_map))
        .collect();
    // Stable sort: lead first (parent_task_id is None), then workers
    // grouped under their parent, alphabetical within a group. Keeps
    // human output predictable regardless of summary.jsonl write order.
    tasks.sort_by(|a, b| {
        a.parent_task_id
            .is_some()
            .cmp(&b.parent_task_id.is_some())
            .then(a.parent_task_id.cmp(&b.parent_task_id))
            .then(a.task_id.cmp(&b.task_id))
    });

    let cost = build_cost_rollup(&summary.tasks, model_map, lead_id.as_deref());
    let failures = build_failure_groups(&summary.tasks);
    let hotspots = build_hotspots(&tasks);

    RunAnalysis {
        header,
        cost,
        failures,
        tasks,
        hotspots,
    }
}

fn task_row(rec: &TaskRecord, model_map: &HashMap<String, String>) -> TaskRow {
    let model = rec
        .model
        .clone()
        .or_else(|| model_map.get(&rec.task_id).cloned());
    let cost_usd = effective_cost(rec, model.as_deref());
    TaskRow {
        task_id: rec.task_id.clone(),
        parent_task_id: rec.parent_task_id.clone(),
        status: status_label(&rec.status).to_string(),
        exit_code: rec.exit_code,
        duration_ms: rec.duration_ms,
        model,
        cost_usd,
        tokens_input: rec.token_usage.input,
        tokens_output: rec.token_usage.output,
        tokens_cache_read: rec.token_usage.cache_read,
        tokens_cache_write: rec.token_usage.cache_creation,
        final_message_preview: rec.final_message_preview.clone(),
        failure_kind: rec.failure_reason.as_ref().map(failure_kind_label),
        pause_count: rec.pause_count,
        reprompt_count: rec.reprompt_count,
        approvals_requested: rec.approvals_requested,
        approvals_approved: rec.approvals_approved,
        approvals_rejected: rec.approvals_rejected,
    }
}

/// Cost stored on the record, falling back to recomputation from
/// `(model, token_usage)` for pre-v0.11 records that never had the
/// field populated. Returns `None` only when both the stored value and
/// the model are unavailable.
///
/// Shared with `diff.rs` so `pitboss diff` and `pitboss analyze` agree
/// on a run's cost. Without this shared helper, diff would always
/// recompute from current price tables, producing a different number
/// from analyze whenever prices have changed since the run was recorded.
pub(crate) fn effective_cost(rec: &TaskRecord, model: Option<&str>) -> Option<f64> {
    if let Some(c) = rec.cost_usd {
        return Some(c);
    }
    let model = model?;
    prices::cost_usd(model, &rec.token_usage)
}

fn build_cost_rollup(
    tasks: &[TaskRecord],
    model_map: &HashMap<String, String>,
    lead_id: Option<&str>,
) -> CostRollup {
    let mut by_model: BTreeMap<String, ModelRollup> = BTreeMap::new();
    let mut total = 0.0;
    let mut lead_usd = 0.0;
    let mut worker_usd = 0.0;
    let mut with_cost = 0;
    let mut missing_cost = 0;

    for rec in tasks {
        let model = rec
            .model
            .clone()
            .or_else(|| model_map.get(&rec.task_id).cloned());
        let cost = effective_cost(rec, model.as_deref());
        if cost.is_some() {
            with_cost += 1;
        } else {
            missing_cost += 1;
        }
        let c = cost.unwrap_or(0.0);
        total += c;
        if Some(rec.task_id.as_str()) == lead_id {
            lead_usd += c;
        } else {
            worker_usd += c;
        }
        let key = model.unwrap_or_else(|| "unknown".to_string());
        let entry = by_model.entry(key.clone()).or_insert(ModelRollup {
            model: key,
            task_count: 0,
            tokens_input: 0,
            tokens_output: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            cost_usd: 0.0,
        });
        entry.task_count += 1;
        entry.tokens_input += rec.token_usage.input;
        entry.tokens_output += rec.token_usage.output;
        entry.tokens_cache_read += rec.token_usage.cache_read;
        entry.tokens_cache_write += rec.token_usage.cache_creation;
        entry.cost_usd += c;
    }

    let mut by_model: Vec<ModelRollup> = by_model.into_values().collect();
    by_model.sort_by(|a, b| f64_desc(a.cost_usd, b.cost_usd));

    CostRollup {
        total_usd: total,
        lead_usd,
        worker_usd,
        by_model,
        tasks_with_cost: with_cost,
        tasks_missing_cost: missing_cost,
    }
}

fn build_failure_groups(tasks: &[TaskRecord]) -> Vec<FailureGroup> {
    let mut buckets: BTreeMap<String, FailureGroup> = BTreeMap::new();
    for rec in tasks {
        if matches!(rec.status, TaskStatus::Success) {
            continue;
        }
        let kind = rec
            .failure_reason
            .as_ref()
            .map(failure_kind_label)
            .unwrap_or_else(|| "unclassified".to_string());
        let entry = buckets.entry(kind.clone()).or_insert_with(|| FailureGroup {
            kind,
            count: 0,
            task_ids: Vec::new(),
            representative_excerpt: None,
            statuses: Vec::new(),
        });
        entry.count += 1;
        entry.task_ids.push(rec.task_id.clone());
        let label = status_label(&rec.status).to_string();
        if !entry.statuses.contains(&label) {
            entry.statuses.push(label);
        }
        if entry.representative_excerpt.is_none() {
            entry.representative_excerpt = failure_excerpt(rec);
        }
    }
    let mut groups: Vec<FailureGroup> = buckets.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.count));
    groups
}

fn failure_excerpt(rec: &TaskRecord) -> Option<String> {
    if let Some(reason) = &rec.failure_reason {
        return Some(match reason {
            FailureReason::RateLimit { resets_at } => match resets_at {
                Some(t) => format!("rate limit; resets at {}", t.to_rfc3339()),
                None => "rate limit hit".to_string(),
            },
            FailureReason::NetworkError { message } => format!("network: {message}"),
            FailureReason::AuthFailure => "401 invalid_api_key".to_string(),
            FailureReason::ContextExceeded => "context length exceeded".to_string(),
            FailureReason::InvalidArgument { message } => format!("invalid request: {message}"),
            FailureReason::Unknown { message } => message.clone(),
        });
    }
    rec.final_message_preview.clone()
}

fn build_hotspots(tasks: &[TaskRow]) -> Hotspots {
    let longest = tasks.iter().max_by_key(|t| t.duration_ms).and_then(|t| {
        if t.duration_ms == 0 {
            None
        } else {
            Some(HotspotEntry {
                task_id: t.task_id.clone(),
                value: t.duration_ms as f64,
                unit: "ms",
            })
        }
    });
    let costliest = tasks
        .iter()
        .filter_map(|t| t.cost_usd.map(|c| (t, c)))
        .max_by(|a, b| f64_asc(a.1, b.1))
        .filter(|(_, c)| *c > 0.0)
        .map(|(t, c)| HotspotEntry {
            task_id: t.task_id.clone(),
            value: c,
            unit: "usd",
        });
    let most_tokens = tasks
        .iter()
        .max_by_key(|t| {
            t.tokens_input + t.tokens_output + t.tokens_cache_read + t.tokens_cache_write
        })
        .and_then(|t| {
            let total =
                t.tokens_input + t.tokens_output + t.tokens_cache_read + t.tokens_cache_write;
            if total == 0 {
                None
            } else {
                Some(HotspotEntry {
                    task_id: t.task_id.clone(),
                    value: total as f64,
                    unit: "tokens",
                })
            }
        });
    Hotspots {
        longest,
        costliest,
        most_tokens,
    }
}

// ── Cross-run aggregates ────────────────────────────────────────────────

fn build_aggregates(runs: &[RunAnalysis]) -> CrossRunAggregates {
    let mut agg = CrossRunAggregates {
        run_count: runs.len(),
        ..Default::default()
    };
    let mut failure_buckets: BTreeMap<String, FailureHistogramEntry> = BTreeMap::new();
    let mut model_buckets: BTreeMap<String, ModelRollup> = BTreeMap::new();

    for run in runs {
        if run.header.tasks_failed > 0 {
            agg.failed_run_count += 1;
        }
        if run.header.was_interrupted {
            agg.interrupted_run_count += 1;
        }
        match run.header.mode.as_str() {
            "flat" => agg.mode_flat += 1,
            "hierarchical" => agg.mode_hierarchical += 1,
            _ => {}
        }
        agg.earliest_started_at = Some(match agg.earliest_started_at {
            Some(t) => t.min(run.header.started_at),
            None => run.header.started_at,
        });
        agg.latest_started_at = Some(match agg.latest_started_at {
            Some(t) => t.max(run.header.started_at),
            None => run.header.started_at,
        });
        agg.aggregate_duration_ms += run.header.total_duration_ms;
        agg.total_cost_usd += run.cost.total_usd;

        for fg in &run.failures {
            // `runs` arrives newest-first (from `collect_run_entries`),
            // so the FIRST insertion holds the most recent example —
            // don't overwrite on later runs.
            failure_buckets
                .entry(fg.kind.clone())
                .and_modify(|e| e.count += fg.count)
                .or_insert_with(|| FailureHistogramEntry {
                    kind: fg.kind.clone(),
                    count: fg.count,
                    example_run_id: run.header.run_id.clone(),
                    example_task_id: fg.task_ids.first().cloned().unwrap_or_default(),
                    example_excerpt: fg.representative_excerpt.clone(),
                });
        }

        for mr in &run.cost.by_model {
            let entry = model_buckets
                .entry(mr.model.clone())
                .or_insert_with(|| ModelRollup {
                    model: mr.model.clone(),
                    task_count: 0,
                    tokens_input: 0,
                    tokens_output: 0,
                    tokens_cache_read: 0,
                    tokens_cache_write: 0,
                    cost_usd: 0.0,
                });
            entry.task_count += mr.task_count;
            entry.tokens_input += mr.tokens_input;
            entry.tokens_output += mr.tokens_output;
            entry.tokens_cache_read += mr.tokens_cache_read;
            entry.tokens_cache_write += mr.tokens_cache_write;
            entry.cost_usd += mr.cost_usd;
        }
    }

    let mut histogram: Vec<FailureHistogramEntry> = failure_buckets.into_values().collect();
    histogram.sort_by_key(|h| std::cmp::Reverse(h.count));
    agg.failure_histogram = histogram;

    let mut model_usage: Vec<ModelRollup> = model_buckets.into_values().collect();
    model_usage.sort_by(|a, b| f64_desc(a.cost_usd, b.cost_usd));
    agg.model_usage = model_usage;

    agg.slowest_runs = top_runs(runs, 3, |r| r.header.total_duration_ms as f64, "ms");
    agg.costliest_runs = top_runs(runs, 3, |r| r.cost.total_usd, "usd");
    agg.most_failed_runs = top_runs(runs, 3, |r| r.header.tasks_failed as f64, "failed_tasks");

    agg
}

fn top_runs<F: Fn(&RunAnalysis) -> f64>(
    runs: &[RunAnalysis],
    n: usize,
    score: F,
    unit: &'static str,
) -> Vec<RunOutlier> {
    let mut scored: Vec<(String, f64)> = runs
        .iter()
        .map(|r| (r.header.run_id.clone(), score(r)))
        .collect();
    scored.sort_by(|a, b| f64_desc(a.1, b.1));
    scored
        .into_iter()
        .filter(|(_, v)| *v > 0.0)
        .take(n)
        .map(|(run_id, value)| RunOutlier {
            run_id,
            value,
            unit,
        })
        .collect()
}

// ── Labels ──────────────────────────────────────────────────────────────

fn status_label(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Success => "Success",
        TaskStatus::Failed => "Failed",
        TaskStatus::TimedOut => "TimedOut",
        TaskStatus::Cancelled => "Cancelled",
        TaskStatus::SpawnFailed => "SpawnFailed",
        TaskStatus::ApprovalRejected => "ApprovalRejected",
        TaskStatus::ApprovalTimedOut => "ApprovalTimedOut",
    }
}

fn failure_kind_label(reason: &FailureReason) -> String {
    match reason {
        FailureReason::RateLimit { .. } => "rate_limit",
        FailureReason::NetworkError { .. } => "network_error",
        FailureReason::AuthFailure => "auth_failure",
        FailureReason::ContextExceeded => "context_exceeded",
        FailureReason::InvalidArgument { .. } => "invalid_argument",
        FailureReason::Unknown { .. } => "unknown",
    }
    .to_string()
}

fn f64_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

fn f64_asc(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

// ── Renderers ───────────────────────────────────────────────────────────

pub fn render_run_human(a: &RunAnalysis) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let _ = writeln!(out, "Run: {}", a.header.run_id);
    if let Some(name) = &a.header.manifest_name {
        let _ = writeln!(out, "  Name: {name}");
    }
    let _ = writeln!(out, "  Manifest: {}", a.header.manifest_path);
    let _ = writeln!(out, "  Mode: {}", a.header.mode);
    let interrupted = if a.header.was_interrupted {
        " (interrupted)"
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "  Status: {}{} — {} task{} total, {} failed",
        a.header.run_status,
        interrupted,
        a.header.tasks_total,
        plural(a.header.tasks_total),
        a.header.tasks_failed,
    );
    let _ = writeln!(
        out,
        "  Duration: {} ({} → {})",
        pitboss_core::fmt::format_duration_ms(a.header.total_duration_ms),
        a.header.started_at.format("%Y-%m-%d %H:%M:%S"),
        a.header.ended_at.format("%Y-%m-%d %H:%M:%S"),
    );
    let _ = writeln!(
        out,
        "  Versions: pitboss {}, claude {}",
        a.header.pitboss_version,
        a.header.claude_version.as_deref().unwrap_or("\u{2014}"),
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "Cost (estimated USD)");
    let _ = writeln!(
        out,
        "  Total: {}  (lead {} + workers {})",
        prices::fmt_cost(Some(a.cost.total_usd)),
        prices::fmt_cost(Some(a.cost.lead_usd)),
        prices::fmt_cost(Some(a.cost.worker_usd)),
    );
    if a.cost.tasks_missing_cost > 0 {
        let _ = writeln!(
            out,
            "  Note: {} task{} have no cost estimate (model unknown)",
            a.cost.tasks_missing_cost,
            plural(a.cost.tasks_missing_cost),
        );
    }
    for mr in &a.cost.by_model {
        let _ = writeln!(
            out,
            "    {:<22} {} task{} — {}  (in {} / out {} / cache_r {} / cache_w {})",
            mr.model,
            mr.task_count,
            plural(mr.task_count),
            prices::fmt_cost(Some(mr.cost_usd)),
            mr.tokens_input,
            mr.tokens_output,
            mr.tokens_cache_read,
            mr.tokens_cache_write,
        );
    }
    let _ = writeln!(out);

    if a.failures.is_empty() {
        let _ = writeln!(out, "Failures: none");
    } else {
        let _ = writeln!(out, "Failures");
        for fg in &a.failures {
            let _ = writeln!(
                out,
                "  {:<18} {} task{} ({}) — {}",
                fg.kind,
                fg.count,
                plural(fg.count),
                fg.statuses.join(", "),
                truncated_list(&fg.task_ids, 5),
            );
            if let Some(ex) = &fg.representative_excerpt {
                let _ = writeln!(out, "    \u{2192} {}", oneline_truncate(ex, 200));
            }
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Tasks");
    let task_id_width = a
        .tasks
        .iter()
        .map(|t| t.task_id.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(20, 50);
    let model_width: usize = 24;
    let _ = writeln!(
        out,
        "  {:<task_id_width$} {:<14} {:>10} {:<model_width$} {:>9}",
        "TASK_ID", "STATUS", "DURATION", "MODEL", "COST",
    );
    let sep = "-".repeat(task_id_width + 1 + 14 + 1 + 10 + 1 + model_width + 1 + 9);
    let _ = writeln!(out, "  {sep}");
    for row in &a.tasks {
        let task_id = pitboss_core::fmt::truncate_ellipsis(&row.task_id, task_id_width);
        let model_label = row.model.as_deref().unwrap_or("\u{2014}");
        let _ = writeln!(
            out,
            "  {:<task_id_width$} {:<14} {:>10} {:<model_width$} {:>9}",
            task_id,
            row.status,
            pitboss_core::fmt::format_duration_ms(row.duration_ms),
            pitboss_core::fmt::truncate_ellipsis(model_label, model_width),
            prices::fmt_cost(row.cost_usd),
        );
    }
    let _ = writeln!(out);

    if a.hotspots.longest.is_some()
        || a.hotspots.costliest.is_some()
        || a.hotspots.most_tokens.is_some()
    {
        let _ = writeln!(out, "Hotspots");
        if let Some(h) = &a.hotspots.longest {
            let _ = writeln!(
                out,
                "  Longest:     {} ({})",
                h.task_id,
                pitboss_core::fmt::format_duration_ms(h.value as i64),
            );
        }
        if let Some(h) = &a.hotspots.costliest {
            let _ = writeln!(
                out,
                "  Costliest:   {} ({})",
                h.task_id,
                prices::fmt_cost(Some(h.value)),
            );
        }
        if let Some(h) = &a.hotspots.most_tokens {
            let _ = writeln!(
                out,
                "  Most tokens: {} ({} tokens)",
                h.task_id, h.value as u64,
            );
        }
    }

    out
}

pub fn render_recent_human(a: &RecentAnalysis) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let span = match (
        a.aggregates.earliest_started_at,
        a.aggregates.latest_started_at,
    ) {
        (Some(e), Some(l)) => format!(
            "{} \u{2192} {}",
            e.format("%Y-%m-%d %H:%M"),
            l.format("%Y-%m-%d %H:%M"),
        ),
        _ => "\u{2014}".to_string(),
    };

    let _ = writeln!(out, "Recent runs: {}", a.aggregates.run_count);
    let _ = writeln!(out, "  Span: {span}");
    let _ = writeln!(
        out,
        "  Modes: {} flat, {} hierarchical",
        a.aggregates.mode_flat, a.aggregates.mode_hierarchical,
    );
    let success = a
        .aggregates
        .run_count
        .saturating_sub(a.aggregates.failed_run_count);
    let success_pct = if a.aggregates.run_count == 0 {
        0.0
    } else {
        (success as f64 / a.aggregates.run_count as f64) * 100.0
    };
    let _ = writeln!(
        out,
        "  Success: {}/{} ({:.1}%) — {} interrupted",
        success, a.aggregates.run_count, success_pct, a.aggregates.interrupted_run_count,
    );
    let _ = writeln!(
        out,
        "  Aggregate duration: {}  Total cost: {}",
        pitboss_core::fmt::format_duration_ms(a.aggregates.aggregate_duration_ms),
        prices::fmt_cost(Some(a.aggregates.total_cost_usd)),
    );
    let _ = writeln!(out);

    if !a.aggregates.failure_histogram.is_empty() {
        let _ = writeln!(out, "Failure modes");
        for f in &a.aggregates.failure_histogram {
            let _ = writeln!(
                out,
                "  {:<18} {} occurrence{} — most recent: run {} task {}",
                f.kind,
                f.count,
                plural(f.count),
                short_id(&f.example_run_id),
                f.example_task_id,
            );
            if let Some(ex) = &f.example_excerpt {
                let _ = writeln!(out, "    \u{2192} {}", oneline_truncate(ex, 200));
            }
        }
        let _ = writeln!(out);
    }

    if !a.aggregates.model_usage.is_empty() {
        let _ = writeln!(out, "Model usage");
        for m in &a.aggregates.model_usage {
            let _ = writeln!(
                out,
                "  {:<22} {} task{} — {}  (in {} / out {})",
                m.model,
                m.task_count,
                plural(m.task_count),
                prices::fmt_cost(Some(m.cost_usd)),
                m.tokens_input,
                m.tokens_output,
            );
        }
        let _ = writeln!(out);
    }

    let any_outliers = !a.aggregates.slowest_runs.is_empty()
        || !a.aggregates.costliest_runs.is_empty()
        || !a.aggregates.most_failed_runs.is_empty();
    if any_outliers {
        let _ = writeln!(out, "Outliers");
        for o in &a.aggregates.slowest_runs {
            let _ = writeln!(
                out,
                "  Slowest:     {} ({})",
                short_id(&o.run_id),
                pitboss_core::fmt::format_duration_ms(o.value as i64),
            );
        }
        for o in &a.aggregates.costliest_runs {
            let _ = writeln!(
                out,
                "  Costliest:   {} ({})",
                short_id(&o.run_id),
                prices::fmt_cost(Some(o.value)),
            );
        }
        for o in &a.aggregates.most_failed_runs {
            let _ = writeln!(
                out,
                "  Most failed: {} ({} task{})",
                short_id(&o.run_id),
                o.value as u64,
                if (o.value - 1.0).abs() < f64::EPSILON {
                    ""
                } else {
                    "s"
                },
            );
        }
    }
    out
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect::<String>()
}

fn oneline_truncate(s: &str, max: usize) -> String {
    let collapsed: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    pitboss_core::fmt::truncate_ellipsis(&collapsed, max)
}

fn truncated_list(ids: &[String], n: usize) -> String {
    let shown: Vec<&str> = ids.iter().take(n).map(String::as_str).collect();
    if ids.len() > n {
        format!("{}, +{} more", shown.join(", "), ids.len() - n)
    } else {
        shown.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as CDuration, TimeZone};
    use pitboss_core::parser::TokenUsage;
    use std::io::Write;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn base_record(task_id: &str, parent: Option<&str>) -> TaskRecord {
        let started = Utc.with_ymd_and_hms(2026, 4, 17, 12, 0, 0).unwrap();
        TaskRecord {
            task_id: task_id.to_string(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: started,
            ended_at: started + CDuration::seconds(10),
            duration_ms: 10_000,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage {
                input: 100,
                output: 200,
                cache_read: 0,
                cache_creation: 0,
            },
            claude_session_id: None,
            final_message_preview: None,
            final_message: None,
            parent_task_id: parent.map(|s| s.to_string()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: Some("claude-haiku-4-5".to_string()),
            failure_reason: None,
            cost_usd: None,
            actor_type: None,
        }
    }

    fn write_summary(dir: &Path, summary: &RunSummary) {
        std::fs::create_dir_all(dir).unwrap();
        let s = serde_json::to_string_pretty(summary).unwrap();
        let mut f = std::fs::File::create(dir.join("summary.json")).unwrap();
        f.write_all(s.as_bytes()).unwrap();
        let mut meta = std::fs::File::create(dir.join("meta.json")).unwrap();
        write!(
            meta,
            "{}",
            serde_json::json!({
                "run_id": summary.run_id,
                "manifest_path": summary.manifest_path,
                "pitboss_version": summary.pitboss_version,
                "claude_version": summary.claude_version,
                "started_at": summary.started_at,
                "env": {},
            })
        )
        .unwrap();
    }

    fn write_resolved_lead(dir: &Path) {
        let v = serde_json::json!({
            "lead": { "id": "triage" },
            "tasks": [],
        });
        std::fs::write(dir.join("resolved.json"), v.to_string()).unwrap();
    }

    fn make_summary(tasks: Vec<TaskRecord>) -> RunSummary {
        let started = Utc.with_ymd_and_hms(2026, 4, 17, 12, 0, 0).unwrap();
        let tasks_failed = tasks
            .iter()
            .filter(|t| !matches!(t.status, TaskStatus::Success))
            .count();
        RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::from("pitboss.toml"),
            manifest_name: Some("nightly".to_string()),
            pitboss_version: "0.9.2".to_string(),
            claude_version: Some("2.1.0".to_string()),
            started_at: started,
            ended_at: started + CDuration::seconds(60),
            total_duration_ms: 60_000,
            tasks_total: tasks.len(),
            tasks_failed,
            was_interrupted: false,
            notify_failures: None,
            tasks,
        }
    }

    #[test]
    fn opportunistic_cost_recompute_for_pre_v0_11_records() {
        let mut rec = base_record("worker-1", Some("triage"));
        rec.cost_usd = None;
        rec.token_usage = TokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 0,
            cache_creation: 0,
        };
        let row = task_row(&rec, &HashMap::new());
        // haiku: 0.80 + 4.00 = $4.80 per 1M each
        let cost = row.cost_usd.expect("cost should recompute");
        assert!((cost - 4.80).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn stored_cost_wins_over_recompute() {
        let mut rec = base_record("worker-1", Some("triage"));
        rec.cost_usd = Some(0.42);
        let row = task_row(&rec, &HashMap::new());
        assert_eq!(row.cost_usd, Some(0.42));
    }

    #[test]
    fn cost_rollup_splits_lead_and_workers() {
        let mut lead = base_record("triage", None);
        lead.cost_usd = Some(1.00);
        let mut w1 = base_record("worker-1", Some("triage"));
        w1.cost_usd = Some(0.25);
        let mut w2 = base_record("worker-2", Some("triage"));
        w2.cost_usd = Some(0.75);
        let summary = make_summary(vec![lead, w1, w2]);
        let model_map = HashMap::new();
        let analysis = build_analysis(&summary, &model_map, "hierarchical", RunStatus::Complete);
        assert!((analysis.cost.total_usd - 2.0).abs() < 1e-9);
        assert!((analysis.cost.lead_usd - 1.0).abs() < 1e-9);
        assert!((analysis.cost.worker_usd - 1.0).abs() < 1e-9);
        assert_eq!(analysis.cost.by_model.len(), 1);
        assert_eq!(analysis.cost.by_model[0].model, "claude-haiku-4-5");
        assert_eq!(analysis.cost.by_model[0].task_count, 3);
    }

    #[test]
    fn failure_groups_dedupe_by_kind() {
        let mut a = base_record("w-a", Some("triage"));
        a.status = TaskStatus::Failed;
        a.failure_reason = Some(FailureReason::RateLimit { resets_at: None });
        let mut b = base_record("w-b", Some("triage"));
        b.status = TaskStatus::Failed;
        b.failure_reason = Some(FailureReason::RateLimit { resets_at: None });
        let mut c = base_record("w-c", Some("triage"));
        c.status = TaskStatus::Failed;
        c.failure_reason = Some(FailureReason::AuthFailure);
        let summary = make_summary(vec![base_record("triage", None), a, b, c]);
        let analysis = build_analysis(
            &summary,
            &HashMap::new(),
            "hierarchical",
            RunStatus::Complete,
        );
        assert_eq!(analysis.failures.len(), 2);
        let rate_limit = analysis
            .failures
            .iter()
            .find(|g| g.kind == "rate_limit")
            .unwrap();
        assert_eq!(rate_limit.count, 2);
        assert_eq!(rate_limit.task_ids, vec!["w-a", "w-b"]);
    }

    #[test]
    fn unclassified_failure_lands_in_unclassified_bucket() {
        let mut a = base_record("w", Some("triage"));
        a.status = TaskStatus::Cancelled;
        a.failure_reason = None;
        let summary = make_summary(vec![base_record("triage", None), a]);
        let analysis = build_analysis(
            &summary,
            &HashMap::new(),
            "hierarchical",
            RunStatus::Complete,
        );
        assert_eq!(analysis.failures.len(), 1);
        assert_eq!(analysis.failures[0].kind, "unclassified");
    }

    #[test]
    fn hotspots_pick_max_per_axis() {
        let mut quick = base_record("quick", Some("triage"));
        quick.duration_ms = 100;
        quick.cost_usd = Some(0.01);
        let mut slow = base_record("slow", Some("triage"));
        slow.duration_ms = 999_999;
        slow.cost_usd = Some(0.02);
        let mut pricey = base_record("pricey", Some("triage"));
        pricey.duration_ms = 200;
        pricey.cost_usd = Some(5.00);
        let summary = make_summary(vec![base_record("triage", None), quick, slow, pricey]);
        let analysis = build_analysis(
            &summary,
            &HashMap::new(),
            "hierarchical",
            RunStatus::Complete,
        );
        assert_eq!(analysis.hotspots.longest.as_ref().unwrap().task_id, "slow");
        assert_eq!(
            analysis.hotspots.costliest.as_ref().unwrap().task_id,
            "pricey"
        );
    }

    #[test]
    fn analyze_run_dir_reads_summary_json() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("019dabcd-1111-7222-8333-444444444444");
        std::fs::create_dir_all(&run_dir).unwrap();
        let summary = make_summary(vec![
            base_record("triage", None),
            base_record("worker-1", Some("triage")),
        ]);
        write_summary(&run_dir, &summary);
        write_resolved_lead(&run_dir);

        let analysis = analyze_run_dir(&run_dir).unwrap();
        assert_eq!(analysis.header.tasks_total, 2);
        assert_eq!(analysis.header.mode, "hierarchical");
        assert_eq!(analysis.tasks.len(), 2);
        // lead first (no parent), worker second
        assert!(analysis.tasks[0].parent_task_id.is_none());
        assert_eq!(analysis.tasks[1].parent_task_id.as_deref(), Some("triage"));
    }

    #[test]
    fn flat_mode_detected_when_no_lead_in_resolved() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join("019dabcd-2222-7222-8333-444444444444");
        std::fs::create_dir_all(&run_dir).unwrap();
        let summary = make_summary(vec![base_record("alpha", None), base_record("beta", None)]);
        write_summary(&run_dir, &summary);
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::json!({"tasks": []}).to_string(),
        )
        .unwrap();

        let analysis = analyze_run_dir(&run_dir).unwrap();
        assert_eq!(analysis.header.mode, "flat");
    }

    #[test]
    fn analyze_recent_clamps_and_filters() {
        let tmp = TempDir::new().unwrap();
        for i in 0..3 {
            let id = format!("019dabcd-{:04x}-7222-8333-444444444444", i);
            let run_dir = tmp.path().join(&id);
            std::fs::create_dir_all(&run_dir).unwrap();
            let mut tasks = vec![base_record("alpha", None)];
            if i == 1 {
                let mut failed = base_record("beta", None);
                failed.status = TaskStatus::Failed;
                failed.failure_reason = Some(FailureReason::AuthFailure);
                tasks.push(failed);
            }
            let summary = make_summary(tasks);
            write_summary(&run_dir, &summary);
        }

        let all = analyze_recent_dirs(tmp.path(), 10, false).unwrap();
        assert_eq!(all.runs.len(), 3);
        assert_eq!(all.aggregates.run_count, 3);
        assert_eq!(all.aggregates.failed_run_count, 1);

        let only_failed = analyze_recent_dirs(tmp.path(), 10, true).unwrap();
        assert_eq!(only_failed.runs.len(), 1);
        assert_eq!(only_failed.aggregates.failure_histogram.len(), 1);
        assert_eq!(
            only_failed.aggregates.failure_histogram[0].kind,
            "auth_failure"
        );

        let clamped = analyze_recent_dirs(tmp.path(), 9999, false).unwrap();
        assert_eq!(clamped.runs.len(), 3); // only 3 exist
    }

    #[test]
    fn json_output_round_trips_via_serde() {
        let summary = make_summary(vec![base_record("triage", None)]);
        let analysis = build_analysis(
            &summary,
            &HashMap::new(),
            "hierarchical",
            RunStatus::Complete,
        );
        let s = serde_json::to_string(&analysis).unwrap();
        assert!(s.contains("\"run_id\""));
        assert!(s.contains("\"hierarchical\""));
    }

    #[test]
    fn render_run_human_includes_key_sections() {
        let summary = make_summary(vec![
            base_record("triage", None),
            base_record("worker-1", Some("triage")),
        ]);
        let analysis = build_analysis(
            &summary,
            &HashMap::new(),
            "hierarchical",
            RunStatus::Complete,
        );
        let out = render_run_human(&analysis);
        assert!(out.contains("Run: "));
        assert!(out.contains("Cost (estimated USD)"));
        assert!(out.contains("Failures: none"));
        assert!(out.contains("Tasks"));
        assert!(out.contains("triage"));
        assert!(out.contains("worker-1"));
    }

    #[test]
    fn render_recent_human_handles_empty() {
        let out = render_recent_human(&RecentAnalysis {
            runs: vec![],
            aggregates: CrossRunAggregates::default(),
        });
        assert!(out.contains("Recent runs: 0"));
    }
}
