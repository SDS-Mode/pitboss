//! Pitboss TUI — v0.2-alpha entry point.
//!
//! Usage:
//!   pitboss-tui              — open most recent run
//!   pitboss-tui `<run-id>`   — open specific run by UUID string or directory name
//!   pitboss-tui list         — print table of recent runs and exit
//!   pitboss-tui --help
//!   pitboss-tui --version

#![deny(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

mod app;
mod control;
mod runs;
mod state;
mod tui;
mod watcher;

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Parser)]
#[command(
    name = "pitboss-tui",
    version,
    about = "Pitboss TUI — observe Pitboss runs",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// List recent runs in a table and exit (non-TUI).
    List,
    /// Render one frame of the TUI to stdout as plain text (no TTY needed).
    Screenshot {
        /// Run id/prefix to render. Omit to use the most recent run.
        #[arg(long)]
        run: Option<String>,
        /// Width of the rendered frame in columns.
        #[arg(long, default_value_t = 120)]
        cols: u16,
        /// Height of the rendered frame in rows.
        #[arg(long, default_value_t = 30)]
        rows: u16,
    },
    /// Print shell completion script for the given shell (bash, zsh, fish,
    /// elvish, powershell) to stdout.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Open a specific run by UUID or directory name.
    #[command(external_subcommand)]
    Run(Vec<String>),
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            // Default: open the most recent run.
            let (run_dir, run_id) = find_most_recent_run()?;
            app::run(run_dir, run_id)
        }
        Some(Commands::List) => {
            cmd_list()?;
            Ok(())
        }
        Some(Commands::Screenshot { run, cols, rows }) => {
            cmd_screenshot(run.as_deref(), cols, rows)
        }
        Some(Commands::Completions { shell }) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "pitboss-tui", &mut std::io::stdout());
            Ok(())
        }
        Some(Commands::Run(args)) => {
            if args.is_empty() {
                bail!("usage: pitboss-tui <run-id>");
            }
            let id = &args[0];
            let (run_dir, run_id) = find_run_by_id(id)?;
            app::run(run_dir, run_id)
        }
    }
}

// ---------------------------------------------------------------------------
// Run discovery
// ---------------------------------------------------------------------------

/// Returns `(run_subdir, run_id_string)` for the most recently modified run
/// directory under the base runs dir.
fn find_most_recent_run() -> Result<(PathBuf, String)> {
    let base = runs::runs_base_dir();
    if !base.exists() {
        bail!(
            "No pitboss runs directory found at {}.\n\
             Run `pitboss` first to create a run.",
            base.display()
        );
    }

    let entries = runs::collect_run_entries(&base);
    if entries.is_empty() {
        bail!(
            "No run directories found under {}.\n\
             Run `pitboss` first to create a run.",
            base.display()
        );
    }

    // collect_run_entries already returns newest-first.
    let first = entries.into_iter().next().unwrap();
    Ok((first.run_dir, first.run_id))
}

/// Locate a run by id/name. Accepts either an exact UUID string or a prefix.
fn find_run_by_id(id: &str) -> Result<(PathBuf, String)> {
    let base = runs::runs_base_dir();
    let candidate = base.join(id);
    if candidate.is_dir() {
        return Ok((candidate, id.to_string()));
    }

    // Try prefix match.
    if let Ok(rd) = std::fs::read_dir(&base) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(id) && entry.path().is_dir() {
                return Ok((entry.path(), name));
            }
        }
    }

    bail!("Run '{}' not found under {}", id, base.display());
}

// ---------------------------------------------------------------------------
// `list` subcommand
// ---------------------------------------------------------------------------

#[allow(clippy::unnecessary_wraps)]
fn cmd_list() -> Result<()> {
    let base = runs::runs_base_dir();
    if !base.exists() {
        println!("No runs directory found at {}.", base.display());
        println!("Run `pitboss` first to create a run.");
        return Ok(());
    }

    let entries = runs::collect_run_entries(&base);

    if entries.is_empty() {
        println!("No runs found under {}.", base.display());
        return Ok(());
    }

    // Print header.
    println!(
        "{:<38}  {:<22}  {:>6}  {:>6}  STATUS",
        "RUN ID", "STARTED", "TASKS", "FAILED"
    );
    println!("{}", "─".repeat(80));

    for e in &entries {
        let started = runs::format_mtime(e.mtime);
        let status = if e.is_complete {
            "complete"
        } else {
            "in-progress"
        };
        println!(
            "{:<38}  {:<22}  {:>6}  {:>6}  {}",
            e.run_id, started, e.tasks_total, e.tasks_failed, status
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// `screenshot` subcommand — render one frame to stdout as plain text
// ---------------------------------------------------------------------------

fn cmd_screenshot(run: Option<&str>, cols: u16, rows: u16) -> Result<()> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (run_dir, run_id) = match run {
        Some(id) => find_run_by_id(id)?,
        None => find_most_recent_run()?,
    };

    // Build a one-shot AppState from the run dir (no background thread).
    let mut state = crate::state::AppState::new(run_dir.clone(), run_id);
    let snapshot = build_one_shot_snapshot(&run_dir);
    state.apply_snapshot(snapshot);

    // Render into a ratatui TestBackend.
    let backend = TestBackend::new(cols, rows);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| crate::tui::render(frame, &state))?;

    // Dump buffer as plain text, one line per row.
    let buf = terminal.backend().buffer();
    for y in 0..rows {
        let mut line = String::with_capacity(cols as usize);
        for x in 0..cols {
            let cell = buf.cell((x, y)).unwrap();
            line.push_str(cell.symbol());
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// Same shape as `watcher::build_snapshot` but synchronous and self-contained.
fn build_one_shot_snapshot(run_dir: &std::path::Path) -> crate::state::AppSnapshot {
    use crate::state::{AppSnapshot, TileState, TileStatus};
    use pitboss_core::store::{TaskRecord, TaskStatus};
    use serde::Deserialize;
    use std::io::BufRead;

    #[derive(Deserialize)]
    struct ResTask {
        id: String,
        #[serde(default)]
        model: Option<String>,
    }
    #[derive(Deserialize)]
    struct Res {
        tasks: Vec<ResTask>,
    }

    let resolved_tasks: Vec<ResTask> = std::fs::read(run_dir.join("resolved.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Res>(&bytes).ok())
        .map(|r| r.tasks)
        .unwrap_or_default();

    let model_map: std::collections::HashMap<String, Option<String>> = resolved_tasks
        .iter()
        .map(|t| (t.id.clone(), t.model.clone()))
        .collect();
    let task_ids: Vec<String> = resolved_tasks.into_iter().map(|t| t.id).collect();

    let mut completed: std::collections::HashMap<String, TaskRecord> =
        std::collections::HashMap::new();
    // Prefer summary.json (finalized) over summary.jsonl (which is cleared on
    // finalize in some code paths). Fall back to jsonl when the run is in-progress.
    if let Ok(bytes) = std::fs::read(run_dir.join("summary.json")) {
        if let Ok(sum) = serde_json::from_slice::<pitboss_core::store::RunSummary>(&bytes) {
            for rec in sum.tasks {
                completed.insert(rec.task_id.clone(), rec);
            }
        }
    }
    if completed.is_empty() {
        if let Ok(f) = std::fs::File::open(run_dir.join("summary.jsonl")) {
            for line in std::io::BufReader::new(f).lines().map_while(Result::ok) {
                if let Ok(rec) = serde_json::from_str::<TaskRecord>(line.trim()) {
                    completed.insert(rec.task_id.clone(), rec);
                }
            }
        }
    }

    let tasks_dir = run_dir.join("tasks");
    let mut tiles: Vec<TileState> = Vec::new();
    let mut failed_count = 0usize;
    let mut run_started_at: Option<chrono::DateTime<chrono::Utc>> = None;
    for id in &task_ids {
        let log_path = tasks_dir.join(id).join("stdout.log");
        let model = model_map.get(id).and_then(Option::clone);
        if let Some(rec) = completed.get(id) {
            if !matches!(rec.status, TaskStatus::Success) {
                failed_count += 1;
            }
            match run_started_at {
                None => run_started_at = Some(rec.started_at),
                Some(existing) if rec.started_at < existing => {
                    run_started_at = Some(rec.started_at);
                }
                _ => {}
            }
            tiles.push(TileState {
                id: id.clone(),
                status: TileStatus::Done(rec.status.clone()),
                duration_ms: Some(rec.duration_ms),
                token_usage_input: rec.token_usage.input,
                token_usage_output: rec.token_usage.output,
                cache_read: rec.token_usage.cache_read,
                cache_creation: rec.token_usage.cache_creation,
                exit_code: rec.exit_code,
                log_path,
                model,
                parent_task_id: rec.parent_task_id.clone(),
            });
        } else {
            tiles.push(TileState {
                id: id.clone(),
                status: TileStatus::Pending,
                duration_ms: None,
                token_usage_input: 0,
                token_usage_output: 0,
                cache_read: 0,
                cache_creation: 0,
                exit_code: None,
                log_path,
                model,
                parent_task_id: None,
            });
        }
    }

    AppSnapshot {
        tasks: tiles,
        focus_log: Vec::new(),
        failed_count,
        run_started_at,
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;

    #[test]
    fn completions_bash_contains_binary_name() {
        // Smoke test: generate bash completions for pitboss-tui and confirm
        // the output references the binary name. We're not validating the
        // script content, just that the subcommand plumbing is wired up.
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut cmd,
            "pitboss-tui",
            &mut buf,
        );
        let s = String::from_utf8(buf).unwrap();
        assert!(
            s.contains("pitboss-tui"),
            "output should reference the binary name"
        );
        assert!(
            s.contains("complete"),
            "output should look like a completion script"
        );
    }
}
