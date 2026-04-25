//! `pitboss diff <run-a> <run-b>` — compare two prior runs side-by-side.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pitboss_core::parser::TokenUsage;
use pitboss_core::prices;
use pitboss_core::store::RunSummary;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Public resolver helpers
// ---------------------------------------------------------------------------

/// Locate a run directory by UUID prefix under `~/.local/share/pitboss/runs/`.
///
/// Returns an error when zero or more than one directory matches the prefix.
pub fn resolve_run(id_or_prefix: &str) -> Result<PathBuf> {
    let base = runs_base_dir();
    resolve_run_under(&base, id_or_prefix)
}

fn runs_base_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/pitboss/runs")
}

fn resolve_run_under(base: &Path, id_or_prefix: &str) -> Result<PathBuf> {
    // Exact match first.
    let exact = base.join(id_or_prefix);
    if exact.is_dir() {
        return Ok(exact);
    }

    // Prefix scan.
    let entries = std::fs::read_dir(base)
        .with_context(|| format!("cannot read runs directory {}", base.display()))?;

    let mut matches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && name.starts_with(id_or_prefix) {
            matches.push(entry.path());
        }
    }

    match matches.len() {
        0 => bail!(
            "no run found matching prefix '{}' under {}",
            id_or_prefix,
            base.display()
        ),
        1 => Ok(matches.remove(0)),
        n => bail!(
            "{n} runs match prefix '{}' — be more specific",
            id_or_prefix
        ),
    }
}

/// Load the `summary.json` for a run directory.
///
/// If `summary.json` doesn't exist (in-progress run), falls back to reading
/// `summary.jsonl` and constructing a partial `RunSummary` with
/// `was_interrupted = true`.
pub fn load_summary(run_dir: &Path) -> Result<RunSummary> {
    // Prefer the finalized summary.json.
    let json_path = run_dir.join("summary.json");
    if json_path.exists() {
        let bytes =
            std::fs::read(&json_path).with_context(|| format!("read {}", json_path.display()))?;
        return serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", json_path.display()));
    }

    // Fall back to summary.jsonl (in-progress or interrupted run).
    let jsonl_path = run_dir.join("summary.jsonl");
    let f = std::fs::File::open(&jsonl_path)
        .with_context(|| format!("open {}", jsonl_path.display()))?;

    let mut tasks = Vec::new();
    for line in std::io::BufReader::new(f)
        .lines()
        .map_while(std::io::Result::ok)
    {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<pitboss_core::store::TaskRecord>(trimmed) {
            Ok(rec) => tasks.push(rec),
            Err(e) => {
                // Warn rather than silently skip — corrupt/truncated lines
                // cause silent undercounting of tasks_total, tasks_failed,
                // token sums, and cost (#107). In-progress runs can have a
                // genuinely partial trailing line (mid-write truncation), but
                // that is indistinguishable here from format skew or disk
                // corruption, so we always surface it.
                tracing::warn!(error = %e, line = trimmed, "summary.jsonl: skipping unparseable line — diff output may be incomplete");
            }
        }
    }

    let tasks_failed = tasks
        .iter()
        .filter(|t| !matches!(t.status, pitboss_core::store::TaskStatus::Success))
        .count();

    // Try to read meta.json for run_id and started_at.
    let meta_path = run_dir.join("meta.json");
    let meta_bytes =
        std::fs::read(&meta_path).with_context(|| format!("read {}", meta_path.display()))?;
    let meta: pitboss_core::store::RunMeta = serde_json::from_slice(&meta_bytes)
        .with_context(|| format!("parse {}", meta_path.display()))?;

    let started = meta.started_at;
    let ended = tasks.last().map_or(started, |t| t.ended_at);

    Ok(RunSummary {
        run_id: meta.run_id,
        manifest_path: meta.manifest_path,
        pitboss_version: meta.pitboss_version,
        claude_version: meta.claude_version,
        started_at: started,
        ended_at: ended,
        total_duration_ms: (ended - started).num_milliseconds(),
        tasks_total: tasks.len(),
        tasks_failed,
        was_interrupted: true,
        tasks,
    })
}

// ---------------------------------------------------------------------------
// Model map from resolved.json
// ---------------------------------------------------------------------------

/// Read `resolved.json` from a run directory and build a `task_id → model` map.
/// Returns an empty map if the file is missing or malformed.
pub fn load_model_map(run_dir: &Path) -> HashMap<String, String> {
    let path = run_dir.join("resolved.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return HashMap::new(),
    };

    #[derive(serde::Deserialize)]
    struct ResTask {
        id: String,
        #[serde(default)]
        model: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Resolved {
        tasks: Vec<ResTask>,
    }

    serde_json::from_slice::<Resolved>(&bytes)
        .map(|r| {
            r.tasks
                .into_iter()
                .filter_map(|t| t.model.map(|m| (t.id, m)))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Diff report types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RunTotals {
    pub tasks_total: usize,
    pub tasks_failed: usize,
    /// Wall-clock duration in milliseconds (ended_at − started_at).
    pub wall_ms: i64,
    pub token_in: u64,
    pub token_out: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct TaskMetrics {
    pub status: String,
    pub duration_ms: i64,
    pub token_in: u64,
    pub token_out: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct TaskPair {
    pub task_id: String,
    pub a: TaskMetrics,
    pub b: TaskMetrics,
}

#[derive(Debug, Serialize)]
pub struct DiffReport {
    pub run_a_id: String,
    pub run_b_id: String,
    pub run_a_total: RunTotals,
    pub run_b_total: RunTotals,
    pub per_task: Vec<TaskPair>,
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

fn task_metrics(rec: &pitboss_core::store::TaskRecord, model: Option<&str>) -> TaskMetrics {
    let cost = model.and_then(|m| {
        prices::cost_usd(
            m,
            &TokenUsage {
                input: rec.token_usage.input,
                output: rec.token_usage.output,
                cache_read: rec.token_usage.cache_read,
                cache_creation: rec.token_usage.cache_creation,
            },
        )
    });
    TaskMetrics {
        status: format!("{:?}", rec.status),
        duration_ms: rec.duration_ms,
        token_in: rec.token_usage.input,
        token_out: rec.token_usage.output,
        cache_read: rec.token_usage.cache_read,
        cache_write: rec.token_usage.cache_creation,
        cost_usd: cost,
    }
}

fn run_totals(summary: &RunSummary, models: &HashMap<String, String>) -> RunTotals {
    let mut token_in = 0u64;
    let mut token_out = 0u64;
    let mut cache_read = 0u64;
    let mut cache_write = 0u64;
    let mut cost_usd = 0.0f64;

    for rec in &summary.tasks {
        token_in += rec.token_usage.input;
        token_out += rec.token_usage.output;
        cache_read += rec.token_usage.cache_read;
        cache_write += rec.token_usage.cache_creation;
        if let Some(model) = models.get(&rec.task_id) {
            if let Some(c) = prices::cost_usd(
                model,
                &TokenUsage {
                    input: rec.token_usage.input,
                    output: rec.token_usage.output,
                    cache_read: rec.token_usage.cache_read,
                    cache_creation: rec.token_usage.cache_creation,
                },
            ) {
                cost_usd += c;
            }
        }
    }

    let wall_ms = (summary.ended_at - summary.started_at).num_milliseconds();

    RunTotals {
        tasks_total: summary.tasks_total,
        tasks_failed: summary.tasks_failed,
        wall_ms,
        token_in,
        token_out,
        cache_read,
        cache_write,
        cost_usd,
    }
}

/// Build a [`DiffReport`] comparing two [`RunSummary`] values.
///
/// `a_models` / `b_models` map task IDs to model names, sourced from
/// `resolved.json` in each run directory. Pass an empty map if the file is
/// absent; costs will fall back to `None` for those tasks.
pub fn build_report(
    a: &RunSummary,
    a_models: &HashMap<String, String>,
    b: &RunSummary,
    b_models: &HashMap<String, String>,
) -> DiffReport {
    let a_map: HashMap<&str, &pitboss_core::store::TaskRecord> =
        a.tasks.iter().map(|t| (t.task_id.as_str(), t)).collect();
    let b_map: HashMap<&str, &pitboss_core::store::TaskRecord> =
        b.tasks.iter().map(|t| (t.task_id.as_str(), t)).collect();

    let mut per_task: Vec<TaskPair> = Vec::new();
    let mut only_a: Vec<String> = Vec::new();
    let mut only_b: Vec<String> = Vec::new();

    // Tasks in A — may also be in B.
    for rec_a in &a.tasks {
        let id = rec_a.task_id.as_str();
        if let Some(rec_b) = b_map.get(id) {
            per_task.push(TaskPair {
                task_id: id.to_string(),
                a: task_metrics(rec_a, a_models.get(id).map(String::as_str)),
                b: task_metrics(rec_b, b_models.get(id).map(String::as_str)),
            });
        } else {
            only_a.push(id.to_string());
        }
    }

    // Tasks in B that weren't in A.
    for rec_b in &b.tasks {
        let id = rec_b.task_id.as_str();
        if !a_map.contains_key(id) {
            only_b.push(id.to_string());
        }
    }

    DiffReport {
        run_a_id: a.run_id.to_string(),
        run_b_id: b.run_id.to_string(),
        run_a_total: run_totals(a, a_models),
        run_b_total: run_totals(b, b_models),
        per_task,
        only_a,
        only_b,
    }
}

// ---------------------------------------------------------------------------
// Human-readable renderer
// ---------------------------------------------------------------------------

fn fmt_ms(ms: i64) -> String {
    // Diff display treats a zero-or-negative duration as "0s", not "—":
    // a run pair where both sides completed in <1ms is still meaningful
    // data. Short-circuit here rather than delegating to format_duration_ms,
    // which would return "—" for non-positive values — the diff view has no
    // "not started" concept (#112).
    if ms <= 0 {
        return "0s".to_string();
    }
    pitboss_core::fmt::format_duration_ms(ms)
}

fn fmt_delta_ms(delta: i64) -> String {
    if delta == 0 {
        "(0)".to_string()
    } else if delta > 0 {
        format!("(+{})", fmt_ms(delta))
    } else {
        format!("(-{})", fmt_ms(-delta))
    }
}

fn fmt_cost_opt(c: Option<f64>) -> String {
    match c {
        Some(v) => format!("${v:.2}"),
        None => "—".to_string(),
    }
}

fn fmt_delta_cost(a: Option<f64>, b: Option<f64>) -> String {
    match (a, b) {
        (Some(av), Some(bv)) => {
            let d = bv - av;
            if d.abs() < 1e-9 {
                "(0)".to_string()
            } else if d > 0.0 {
                format!("(+${d:.2})")
            } else {
                format!("(-${:.2})", -d)
            }
        }
        _ => "".to_string(),
    }
}

/// Render a [`DiffReport`] as a human-readable plain-text table.
#[must_use]
pub fn render_human(report: &DiffReport) -> String {
    let mut out = String::new();

    let a_short = short_id(&report.run_a_id);
    let b_short = short_id(&report.run_b_id);

    out.push_str(&format!(
        "Run comparison: {a_short} (A)  vs  {b_short} (B)\n\n"
    ));

    // --- TOTALS ---
    let ta = &report.run_a_total;
    let tb = &report.run_b_total;

    out.push_str("TOTALS\n");
    out.push_str(&format!(
        "  tasks:       {}/{} ok          vs    {}/{} ok\n",
        ta.tasks_total - ta.tasks_failed,
        ta.tasks_total,
        tb.tasks_total - tb.tasks_failed,
        tb.tasks_total,
    ));
    out.push_str(&format!(
        "  wall-clock:  {:<15}  vs    {:<15}  {}\n",
        fmt_ms(ta.wall_ms),
        fmt_ms(tb.wall_ms),
        fmt_delta_ms(tb.wall_ms - ta.wall_ms),
    ));
    out.push_str(&format!(
        "  tokens in:   {:<15}  vs    {:<15}  ({})\n",
        ta.token_in,
        tb.token_in,
        i128::from(tb.token_in) - i128::from(ta.token_in),
    ));
    out.push_str(&format!(
        "  tokens out:  {:<15}  vs    {:<15}  ({})\n",
        ta.token_out,
        tb.token_out,
        i128::from(tb.token_out) - i128::from(ta.token_out),
    ));
    out.push_str(&format!(
        "  cost:        ${:<14.2}  vs    ${:<14.2}  {}\n",
        ta.cost_usd,
        tb.cost_usd,
        fmt_delta_cost(Some(ta.cost_usd), Some(tb.cost_usd)),
    ));

    // --- PER TASK ---
    if !report.per_task.is_empty() {
        out.push('\n');
        out.push_str("PER TASK\n");
        out.push_str(&format!(
            "  {:<20}  {:<35}  {:<35}  {}\n",
            "task", "A: status / dur / out / cost", "B: status / dur / out / cost", "Dcost"
        ));

        for pair in &report.per_task {
            let a_icon = if pair.a.status == "Success" { "+" } else { "x" };
            let b_icon = if pair.b.status == "Success" { "+" } else { "x" };

            let a_cell = format!(
                "{} {} / {} / {} / {}",
                a_icon,
                pair.a.status,
                fmt_ms(pair.a.duration_ms),
                pair.a.token_out,
                fmt_cost_opt(pair.a.cost_usd),
            );
            let b_cell = format!(
                "{} {} / {} / {} / {}",
                b_icon,
                pair.b.status,
                fmt_ms(pair.b.duration_ms),
                pair.b.token_out,
                fmt_cost_opt(pair.b.cost_usd),
            );
            let dcost = fmt_delta_cost(pair.a.cost_usd, pair.b.cost_usd);

            out.push_str(&format!(
                "  {:<20}  {:<35}  {:<35}  {}\n",
                pair.task_id, a_cell, b_cell, dcost
            ));
        }
    }

    // --- ONLY IN A ---
    if !report.only_a.is_empty() {
        out.push('\n');
        out.push_str("ONLY IN A:\n");
        for id in &report.only_a {
            out.push_str(&format!("  {id}\n"));
        }
    }

    // --- ONLY IN B ---
    if !report.only_b.is_empty() {
        out.push('\n');
        out.push_str("ONLY IN B:\n");
        for id in &report.only_b {
            out.push_str(&format!("  {id}\n"));
        }
    }

    if report.only_a.is_empty() && report.only_b.is_empty() && report.per_task.is_empty() {
        out.push_str("\n(no tasks to compare)\n");
    }

    out
}

fn short_id(id: &str) -> &str {
    if id.len() > 8 {
        &id[..8]
    } else {
        id
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pitboss_core::parser::TokenUsage;
    use pitboss_core::store::{RunSummary, TaskRecord, TaskStatus};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    fn make_task(id: &str, status: TaskStatus, dur_ms: i64, tin: u64, tout: u64) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            task_id: id.to_string(),
            status,
            exit_code: Some(0),
            started_at: now,
            ended_at: now + chrono::Duration::milliseconds(dur_ms),
            duration_ms: dur_ms,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage {
                input: tin,
                output: tout,
                cache_read: 0,
                cache_creation: 0,
            },
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

    fn make_summary(tasks: Vec<TaskRecord>) -> RunSummary {
        let now = Utc::now();
        let total = tasks.len();
        let failed = tasks
            .iter()
            .filter(|t| !matches!(t.status, TaskStatus::Success))
            .count();
        RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::from("/tmp/test.toml"),
            pitboss_version: "0.1.0".to_string(),
            claude_version: None,
            started_at: now,
            ended_at: now + chrono::Duration::seconds(60),
            total_duration_ms: 60_000,
            tasks_total: total,
            tasks_failed: failed,
            was_interrupted: false,
            tasks,
        }
    }

    // Test 1: build_report with same task sets → no only_a / only_b
    #[test]
    fn build_report_same_tasks_no_only() {
        let a = make_summary(vec![
            make_task("foo", TaskStatus::Success, 1000, 10, 100),
            make_task("bar", TaskStatus::Success, 2000, 20, 200),
        ]);
        let b = make_summary(vec![
            make_task("foo", TaskStatus::Success, 900, 5, 80),
            make_task("bar", TaskStatus::Failed, 2100, 15, 180),
        ]);
        let report = build_report(&a, &HashMap::new(), &b, &HashMap::new());
        assert!(report.only_a.is_empty(), "no only_a expected");
        assert!(report.only_b.is_empty(), "no only_b expected");
        assert_eq!(report.per_task.len(), 2);
    }

    // Test 2: build_report with disjoint tasks → everything in only_a/only_b
    #[test]
    fn build_report_disjoint_tasks_all_in_only() {
        let a = make_summary(vec![make_task("alpha", TaskStatus::Success, 1000, 10, 100)]);
        let b = make_summary(vec![make_task("beta", TaskStatus::Success, 900, 5, 80)]);
        let report = build_report(&a, &HashMap::new(), &b, &HashMap::new());
        assert_eq!(report.only_a, vec!["alpha"]);
        assert_eq!(report.only_b, vec!["beta"]);
        assert!(report.per_task.is_empty());
    }

    // Test 3: RunTotals arithmetic — sum tokens, compute cost via prices
    #[test]
    fn run_totals_sums_tokens_and_cost() {
        let tasks = vec![
            make_task("t1", TaskStatus::Success, 1000, 1_000_000, 1_000_000),
            make_task("t2", TaskStatus::Success, 2000, 0, 500),
        ];
        let summary = make_summary(tasks);
        let mut models = HashMap::new();
        models.insert("t1".to_string(), "claude-haiku-4-5".to_string());
        let totals = run_totals(&summary, &models);
        assert_eq!(totals.token_in, 1_000_000);
        assert_eq!(totals.token_out, 1_000_500);
        // 1M input + 1M output on haiku = $4.80
        assert!(
            (totals.cost_usd - 4.80).abs() < 1e-4,
            "cost {}",
            totals.cost_usd
        );
    }

    // Test 4: resolve_run with UUID prefix → returns the matching dir
    #[test]
    fn resolve_run_under_finds_prefix_match() {
        let dir = TempDir::new().unwrap();
        let run_id = "019d9a57-5e40-7851-9f98-0a1102d85eaf";
        std::fs::create_dir(dir.path().join(run_id)).unwrap();

        let found = resolve_run_under(dir.path(), "019d9a57").unwrap();
        assert_eq!(found, dir.path().join(run_id));
    }

    // Test 5: load_summary falls back to summary.jsonl when summary.json missing
    #[test]
    fn load_summary_falls_back_to_jsonl() {
        use chrono::TimeZone;
        let dir = TempDir::new().unwrap();

        // Write meta.json
        let run_id = Uuid::now_v7();
        let started_at = Utc.with_ymd_and_hms(2026, 4, 16, 10, 0, 0).unwrap();
        let meta = pitboss_core::store::RunMeta {
            run_id,
            manifest_path: PathBuf::from("/tmp/test.toml"),
            pitboss_version: "0.1.0".to_string(),
            claude_version: None,
            started_at,
            env: HashMap::new(),
        };
        std::fs::write(
            dir.path().join("meta.json"),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .unwrap();

        // Write summary.jsonl with two records.
        let rec = make_task("task-x", TaskStatus::Success, 5000, 10, 200);
        let line = serde_json::to_string(&rec).unwrap();
        std::fs::write(dir.path().join("summary.jsonl"), format!("{line}\n")).unwrap();

        // No summary.json → should fall back.
        let loaded = load_summary(dir.path()).unwrap();
        assert!(loaded.was_interrupted, "fallback path sets was_interrupted");
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].task_id, "task-x");
    }
}
