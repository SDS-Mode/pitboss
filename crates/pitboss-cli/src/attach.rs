//! `pitboss attach <run-id> <task-id>` — follow-mode viewer for a single
//! worker's log stream. Shape inspired by `docker logs -f` / `tail -f`
//! but understands pitboss's stream-json event format.
//!
//! What it does:
//!   1. Resolve `<run-id>` (full UUID or unique prefix) → run directory
//!      under `~/.local/share/pitboss/runs/`.
//!   2. Locate `<run-dir>/tasks/<task-id>/stdout.log`. Fail with a
//!      helpful error if the task doesn't exist (lists siblings).
//!   3. Optionally replay the last N historical events (`--lines`).
//!   4. Enter a follow loop: poll the file every 200 ms, emit new lines
//!      as they arrive. Formatted by default; raw stream-json via
//!      `--raw` so output is pipe-friendly.
//!   5. Exit on Ctrl-C OR when a stream-json `Event::Result` is seen
//!      (claude finished and wrote its terminal event).
//!
//! Intentionally NOT a TTY relay. Claude workers are non-interactive
//! (prompt comes from `-p` at spawn time), so there is nothing to
//! relay stdin TO; portable-pty / PTY wrapping is out of scope. See
//! `ROADMAP.md` for the retired analysis.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use pitboss_core::parser::{parse_line, Event};

/// Poll cadence for new bytes. 200 ms feels responsive without burning
/// CPU when a worker is idle between turns.
const POLL_INTERVAL_MS: u64 = 200;

/// Per-event display caps when rendering non-raw mode. Same ceilings
/// the TUI watcher uses — long pathologically-large events get a
/// `… +N chars` marker so operators see there's more without the
/// terminal drowning.
const CAP_ASSISTANT_TEXT: usize = 2000;
const CAP_TOOL_INPUT: usize = 1000;
const CAP_TOOL_RESULT: usize = 3000;

/// Entry point for the `attach` subcommand. Builds a single-threaded
/// tokio runtime for the Ctrl-C handler then drives the sync polling
/// loop inline. Returns a process exit code: 0 on clean completion /
/// Ctrl-C, non-zero on resolution / IO errors.
pub fn run(run_id_prefix: &str, task_id: &str, raw: bool, lines: usize) -> Result<i32> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(run_async(run_id_prefix, task_id, raw, lines))
}

async fn run_async(run_id_prefix: &str, task_id: &str, raw: bool, lines: usize) -> Result<i32> {
    validate_task_id(task_id)?;
    let base = default_runs_dir();
    let run_dir = resolve_run_dir(&base, run_id_prefix)?;
    let tasks_root = run_dir.join("tasks");
    let task_dir = tasks_root.join(task_id);
    if !task_dir.is_dir() {
        bail_with_siblings(&run_dir, task_id);
    }
    // Belt-and-suspenders: after the directory check, canonicalize both
    // sides and assert the task dir is still inside <run>/tasks/. Guards
    // against a pre-planted symlink under tasks/<task_id>/ pointing out of
    // the run dir.
    let tasks_root_canon = std::fs::canonicalize(&tasks_root)
        .with_context(|| format!("canonicalize {}", tasks_root.display()))?;
    let task_dir_canon = std::fs::canonicalize(&task_dir)
        .with_context(|| format!("canonicalize {}", task_dir.display()))?;
    if !task_dir_canon.starts_with(&tasks_root_canon) {
        bail!("task id '{task_id}' resolves outside the run directory; refusing to follow",);
    }
    let log_path = task_dir_canon.join("stdout.log");

    let mut stderr = std::io::stderr();
    writeln!(
        stderr,
        "pitboss attach: following {} (Ctrl-C to exit)",
        log_path.display()
    )?;
    stderr.flush().ok();

    // Ctrl-C signal handler: flip a flag; the polling loop checks it
    // between reads so we exit promptly without truncating a partial
    // line.
    let sigint = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sigint_bg = sigint.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        sigint_bg.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    follow_log(&log_path, raw, lines, &sigint).await
}

/// Reject task ids that are empty, `.`/`..`, or contain a path separator
/// or NUL byte. Task ids are expected to be simple directory names chosen
/// by the manifest author; anything else is a traversal attempt.
fn validate_task_id(task_id: &str) -> Result<()> {
    if task_id.is_empty() || task_id == "." || task_id == ".." {
        bail!("invalid task id '{task_id}'");
    }
    if task_id.contains('/') || task_id.contains('\\') || task_id.contains('\0') {
        bail!("task id must not contain path separators or NUL: '{task_id}'");
    }
    Ok(())
}

/// Returns `~/.local/share/pitboss/runs/` — matches the default used by
/// `pitboss resume` / `pitboss diff` and the TUI's run discovery.
fn default_runs_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".local/share/pitboss/runs")
}

/// Resolve a run id (full UUID or unique prefix) to an absolute run
/// directory. Mirrors the resolver in `main.rs` (used by `resume`)
/// except the error messages are tailored for `attach`.
fn resolve_run_dir(base: &Path, prefix: &str) -> Result<PathBuf> {
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

/// When the requested task id doesn't exist under `<run>/tasks/`, print
/// the list of siblings so the operator can fix the typo in one try.
fn bail_with_siblings(run_dir: &Path, task_id: &str) -> ! {
    let tasks_dir = run_dir.join("tasks");
    let mut siblings: Vec<String> = std::fs::read_dir(&tasks_dir)
        .ok()
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    siblings.sort();
    eprintln!(
        "pitboss attach: task '{}' not found in run {}",
        task_id,
        run_dir.display()
    );
    if siblings.is_empty() {
        eprintln!("(no tasks have logged output yet)");
    } else {
        eprintln!("available task ids:");
        for s in siblings {
            eprintln!("  {s}");
        }
    }
    std::process::exit(2);
}

/// Follow the log: emit last `history` events, then tail new bytes.
/// Exits cleanly on Ctrl-C (via `sigint` flag) or when a stream-json
/// `Event::Result` is seen (claude wrote its terminal event).
async fn follow_log(
    path: &Path,
    raw: bool,
    history: usize,
    sigint: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<i32> {
    let mut stdout = std::io::stdout();

    // Wait up to ~5s for the log to appear if the run is just starting.
    let mut waited_ms = 0u64;
    while !path.exists() && waited_ms < 5_000 {
        if sigint.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(0);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        waited_ms += 200;
    }
    if !path.exists() {
        bail!(
            "log file {} never appeared (is the task actually running?)",
            path.display()
        );
    }

    // Seed: emit the last `history` parsed events. Cheap-enough read
    // of the whole file; worker logs are typically <10 MB even for long
    // sessions and this path only runs once at startup.
    let mut seeded_bytes: u64 = {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let mut rendered: Vec<String> = Vec::new();
        for raw_line in bytes.split(|b| *b == b'\n') {
            if raw_line.is_empty() {
                continue;
            }
            if raw {
                // In raw mode we preserve the exact JSON line (newline
                // appended below). No parse needed for the tail slice.
                rendered.push(String::from_utf8_lossy(raw_line).into_owned());
            } else if let Some(s) = format_event_capped(raw_line) {
                rendered.push(s);
            }
        }
        let start = rendered.len().saturating_sub(history);
        for line in &rendered[start..] {
            writeln!(stdout, "{line}")?;
        }
        stdout.flush().ok();
        u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    };

    // Follow loop: poll file size, read any new bytes, process lines.
    let mut tail_buf: Vec<u8> = Vec::new();
    loop {
        if sigint.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(0);
        }
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("pitboss attach: stat {}: {e}", path.display());
                return Ok(1);
            }
        };
        let size = meta.len();
        if size < seeded_bytes {
            // File was truncated or rotated. Reset to follow from new
            // start — rare for pitboss's append-only stdout.log, but
            // handle gracefully rather than reading negative offsets.
            seeded_bytes = 0;
            tail_buf.clear();
        }
        if size > seeded_bytes {
            // Read only the new bytes from `seeded_bytes` onward.
            let mut f =
                std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
            f.seek(SeekFrom::Start(seeded_bytes))?;
            let mut chunk = Vec::with_capacity((size - seeded_bytes) as usize);
            f.read_to_end(&mut chunk)?;
            seeded_bytes = size;

            tail_buf.extend_from_slice(&chunk);
            // Split on '\n'; keep the last partial segment (no trailing
            // newline) in `tail_buf` for the next iteration.
            let mut start = 0usize;
            let mut done = false;
            for (i, b) in tail_buf.iter().enumerate() {
                if *b == b'\n' {
                    let line = &tail_buf[start..i];
                    start = i + 1;
                    if line.is_empty() {
                        continue;
                    }
                    if raw {
                        stdout.write_all(line)?;
                        stdout.write_all(b"\n")?;
                    } else if let Some(s) = format_event_capped(line) {
                        writeln!(stdout, "{s}")?;
                    }
                    // Terminal event — claude wrote its final `result`.
                    // Emit, then exit so pipelines move on.
                    if !raw && is_result_event(line) {
                        done = true;
                    }
                }
            }
            if start > 0 {
                tail_buf.drain(..start);
            }
            stdout.flush().ok();
            if done {
                return Ok(0);
            }
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

fn is_result_event(line: &[u8]) -> bool {
    matches!(parse_line(line), Ok(Event::Result { .. }))
}

/// Same shape + caps as the TUI watcher's `format_event` — parse one
/// stream-json line and return a display string, or `None` to skip.
/// Duplicated here rather than cross-crate imported so `attach` works
/// without pulling pitboss-tui into pitboss-cli.
fn format_event_capped(bytes: &[u8]) -> Option<String> {
    match parse_line(bytes).ok()? {
        Event::AssistantText { text } => {
            let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or(&text);
            Some(format!("> {}", cap_with_marker(first, CAP_ASSISTANT_TEXT)))
        }
        Event::AssistantToolUse {
            tool_name,
            input_summary,
        } => Some(format!(
            "* {tool_name} {}",
            cap_with_marker(&input_summary, CAP_TOOL_INPUT)
        )),
        Event::ToolResult { content_summary } => Some(format!(
            "< {}",
            cap_with_marker(&content_summary, CAP_TOOL_RESULT)
        )),
        Event::Result { usage, .. } => Some(format!(
            "v result (in={} out={} cache_r={} cache_c={})",
            usage.input, usage.output, usage.cache_read, usage.cache_creation
        )),
        Event::RateLimit {
            status,
            rate_limit_type,
            resets_at,
        } => {
            let rtype = rate_limit_type.as_deref().unwrap_or("unknown");
            let resets = resets_at.map_or_else(|| "?".to_string(), |ts| ts.to_string());
            Some(format!("! rate-limit {status} ({rtype}) resets={resets}"))
        }
        Event::System { .. } | Event::Unknown { .. } => None,
    }
}

fn cap_with_marker(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    let extra = char_count - max_chars;
    format!("{head} … +{extra} chars")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn resolve_run_dir_finds_exact_match() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("019da1bb-7820-7d73-92ea-146e21f77dd8");
        std::fs::create_dir_all(&target).unwrap();
        let got = resolve_run_dir(tmp.path(), "019da1bb").unwrap();
        assert_eq!(got, target);
    }

    #[test]
    fn resolve_run_dir_errors_on_no_match() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_run_dir(tmp.path(), "000").unwrap_err();
        assert!(err.to_string().contains("no run found"));
    }

    #[test]
    fn resolve_run_dir_errors_on_ambiguous_prefix() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("019da1bb-aaa")).unwrap();
        std::fs::create_dir_all(tmp.path().join("019da1bb-bbb")).unwrap();
        let err = resolve_run_dir(tmp.path(), "019da1bb").unwrap_err();
        assert!(err.to_string().contains("2 runs match"));
    }

    #[test]
    fn cap_with_marker_unchanged_below_limit() {
        assert_eq!(cap_with_marker("hi", 10), "hi");
    }

    #[test]
    fn cap_with_marker_reports_delta() {
        assert_eq!(cap_with_marker("hello world", 5), "hello … +6 chars");
    }

    #[test]
    fn format_event_assistant_text_gets_arrow_prefix() {
        let line =
            br#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi there"}]}}"#;
        let out = format_event_capped(line).unwrap();
        assert!(out.starts_with("> "));
        assert!(out.contains("hi there"));
    }

    #[test]
    fn format_event_result_shows_usage() {
        let line = br#"{"type":"result","session_id":"s","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":3,"cache_creation_input_tokens":2}}"#;
        let out = format_event_capped(line).unwrap();
        assert!(out.contains("in=10"));
        assert!(out.contains("out=5"));
    }

    #[test]
    fn validate_task_id_rejects_traversal() {
        assert!(validate_task_id("../etc").is_err());
        assert!(validate_task_id("..").is_err());
        assert!(validate_task_id(".").is_err());
        assert!(validate_task_id("").is_err());
        assert!(validate_task_id("a/b").is_err());
        assert!(validate_task_id("a\\b").is_err());
        assert!(validate_task_id("a\0b").is_err());
    }

    #[test]
    fn validate_task_id_accepts_normal_names() {
        assert!(validate_task_id("worker-1").is_ok());
        assert!(validate_task_id("019da1bb-7820-7d73-92ea-146e21f77dd8").is_ok());
    }

    #[test]
    fn is_result_event_true_for_terminal_line() {
        let line =
            br#"{"type":"result","session_id":"s","usage":{"input_tokens":0,"output_tokens":0}}"#;
        assert!(is_result_event(line));
        let not_result =
            br#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"}]}}"#;
        assert!(!is_result_event(not_result));
    }
}
