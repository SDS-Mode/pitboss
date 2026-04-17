//! Background watcher thread — polls the run directory every 500ms and
//! emits `AppSnapshot` updates via an mpsc channel.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use mosaic_core::parser::{parse_line, Event};
use mosaic_core::store::{TaskRecord, TaskStatus};
use serde::Deserialize;

use crate::state::{AppSnapshot, TileState, TileStatus};

const POLL_INTERVAL_MS: u64 = 500;
/// Number of parsed focus-pane lines to keep.
const TAIL_LINES: usize = 40;
/// A task is considered "running" if its log was modified within this many seconds.
const RUNNING_FRESHNESS_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// resolved.json schema (only the fields we care about)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ResolvedTask {
    pub id: String,
}

#[derive(Debug, Deserialize)]
struct ResolvedManifest {
    pub tasks: Vec<ResolvedTask>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Spawn the watcher thread.  The caller gives us a `focus_rx` so we can
/// tail the correct log; the watcher notifies back via `snapshot_tx`.
///
/// Simplest design: the watcher tracks `focused_task_id` internally and
/// the app tells us the focused id via `focus_tx`.
pub fn watch(
    run_dir: PathBuf,
    snapshot_tx: mpsc::SyncSender<AppSnapshot>,
    focus_rx: mpsc::Receiver<String>,
) {
    std::thread::spawn(move || {
        let mut focused_id: Option<String> = None;

        loop {
            // Drain focus updates (non-blocking).
            while let Ok(id) = focus_rx.try_recv() {
                focused_id = Some(id);
            }

            let snapshot = build_snapshot(&run_dir, focused_id.as_deref());
            // If the receiver is gone (app quit), exit thread.
            if snapshot_tx.try_send(snapshot).is_err() {
                break;
            }

            std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        }
    });
}

// ---------------------------------------------------------------------------
// Snapshot construction
// ---------------------------------------------------------------------------

fn build_snapshot(run_dir: &Path, focused_id: Option<&str>) -> AppSnapshot {
    // 1. Read resolved.json → get all task ids in order.
    let resolved_path = run_dir.join("resolved.json");
    let task_ids: Vec<String> = match std::fs::read(&resolved_path) {
        Ok(bytes) => match serde_json::from_slice::<ResolvedManifest>(&bytes) {
            Ok(m) => m.tasks.into_iter().map(|t| t.id).collect(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };

    // 2. Gather completed task records. Prefer summary.json (written on clean
    //    finalize) since summary.jsonl may be empty or truncated after
    //    finalization. Merge the jsonl records on top so in-progress runs
    //    still show completed tiles live.
    let mut completed: std::collections::HashMap<String, TaskRecord> =
        read_summary_json(&run_dir.join("summary.json"));
    let jsonl = read_summary_jsonl(&run_dir.join("summary.jsonl"));
    for (k, v) in jsonl {
        completed.entry(k).or_insert(v);
    }

    // 3. Build tile states.
    let tasks_dir = run_dir.join("tasks");
    let mut tasks: Vec<TileState> = Vec::with_capacity(task_ids.len());
    let mut failed_count = 0usize;

    for id in &task_ids {
        let log_path = tasks_dir.join(id).join("stdout.log");

        if let Some(rec) = completed.get(id) {
            if !matches!(rec.status, TaskStatus::Success) {
                failed_count += 1;
            }
            tasks.push(TileState {
                id: id.clone(),
                status: TileStatus::Done(rec.status.clone()),
                duration_ms: Some(rec.duration_ms),
                token_usage_input: rec.token_usage.input,
                token_usage_output: rec.token_usage.output,
                exit_code: rec.exit_code,
                log_path,
            });
        } else {
            // Decide between Pending and Running by checking log freshness.
            let status = log_freshness_secs(&log_path).map_or(TileStatus::Pending, |age| {
                if age <= RUNNING_FRESHNESS_SECS {
                    TileStatus::Running
                } else {
                    TileStatus::Pending
                }
            });

            tasks.push(TileState {
                id: id.clone(),
                status,
                duration_ms: None,
                token_usage_input: 0,
                token_usage_output: 0,
                exit_code: None,
                log_path,
            });
        }
    }

    // 4. Tail the focused tile's log.
    let focus_log = focused_id
        .and_then(|fid| tasks.iter().find(|t| t.id == fid))
        .map_or_else(
            || {
                // Fall back to first tile if no focus given.
                tasks
                    .first()
                    .map(|t| tail_log(&t.log_path, TAIL_LINES))
                    .unwrap_or_default()
            },
            |t| tail_log(&t.log_path, TAIL_LINES),
        );

    AppSnapshot {
        tasks,
        focus_log,
        failed_count,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_summary_jsonl(path: &Path) -> std::collections::HashMap<String, TaskRecord> {
    let mut map = std::collections::HashMap::new();
    let Ok(file) = std::fs::File::open(path) else {
        return map;
    };
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<TaskRecord>(&line) {
            map.insert(rec.task_id.clone(), rec);
        }
    }
    map
}

/// Reads the finalized summary.json (present only on clean exit) and returns
/// a map of task records. Empty map if the file is missing or unreadable.
fn read_summary_json(path: &Path) -> std::collections::HashMap<String, TaskRecord> {
    let mut map = std::collections::HashMap::new();
    let Ok(bytes) = std::fs::read(path) else {
        return map;
    };
    if let Ok(summary) = serde_json::from_slice::<mosaic_core::store::RunSummary>(&bytes) {
        for rec in summary.tasks {
            map.insert(rec.task_id.clone(), rec);
        }
    }
    map
}

/// Returns `Some(seconds_since_last_modification)` if the file exists,
/// or `None` if it does not exist or metadata cannot be read.
fn log_freshness_secs(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let elapsed = modified.elapsed().ok()?;
    Some(elapsed.as_secs())
}

/// Reads the last `n` *renderable* lines of a stream-json log file.
///
/// Each raw line is fed through `parse_line`; recognized events are formatted
/// as human-readable text, while noisy/unknown events are silently skipped.
/// Returns an empty vec if the file is missing or unreadable.
fn tail_log(path: &Path, n: usize) -> Vec<String> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut rendered: Vec<String> = Vec::new();
    for raw_line in reader.lines().map_while(Result::ok) {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(display) = format_event(trimmed.as_bytes()) {
            rendered.push(display);
        }
    }
    if rendered.len() <= n {
        rendered
    } else {
        rendered[rendered.len() - n..].to_vec()
    }
}

/// Parse one stream-json line and return a display string, or `None` to skip.
fn format_event(bytes: &[u8]) -> Option<String> {
    let event = parse_line(bytes).ok()?;
    match event {
        Event::AssistantText { text } => {
            // Take the first non-empty line, then cap at 180 chars.
            let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or(&text);
            let capped = cap_str(first_line, 180);
            Some(format!("> {capped}"))
        }
        Event::AssistantToolUse {
            tool_name,
            input_summary,
        } => {
            let summary = cap_str(&input_summary, 80);
            Some(format!("* {tool_name} {summary}"))
        }
        Event::ToolResult { content_summary } => {
            let capped = cap_str(&content_summary, 180);
            Some(format!("< {capped}"))
        }
        Event::Result {
            session_id, usage, ..
        } => {
            let short_session = if session_id.len() > 8 {
                &session_id[..8]
            } else {
                &session_id
            };
            Some(format!(
                "v result (session={short_session}... | in={} out={})",
                usage.input, usage.output
            ))
        }
        Event::RateLimit {
            status,
            rate_limit_type,
            resets_at,
        } => {
            let rtype = rate_limit_type.as_deref().unwrap_or("unknown");
            let resets = resets_at.map_or_else(|| "?".to_string(), |ts| ts.to_string());
            Some(format!("! rate-limit {status} ({rtype}) resets={resets}"))
        }
        // System and Unknown events are too noisy; skip them.
        Event::System { .. } | Event::Unknown { .. } => None,
    }
}

/// Truncate `s` to at most `max_chars` characters, appending "..." if cut.
fn cap_str(s: &str, max_chars: usize) -> &str {
    // Find the byte offset of the `max_chars`-th char boundary.
    let mut chars = s.char_indices();
    if let Some((byte_idx, _)) = chars.nth(max_chars) {
        &s[..byte_idx]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_str_below_limit() {
        assert_eq!(cap_str("hello", 10), "hello");
    }

    #[test]
    fn cap_str_at_limit() {
        assert_eq!(cap_str("hello", 5), "hello");
    }

    #[test]
    fn cap_str_above_limit() {
        assert_eq!(cap_str("hello world", 5), "hello");
    }

    #[test]
    fn cap_str_handles_multibyte_boundary() {
        // "é" is 2 bytes; truncating after 2 chars should cleanly split before
        // the third char, not mid-byte.
        let s = "héllo";
        let capped = cap_str(s, 3);
        assert_eq!(capped, "hél");
    }

    #[test]
    fn format_event_assistant_text() {
        let line =
            br#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#;
        let out = format_event(line).unwrap();
        assert!(out.starts_with("> "));
        assert!(out.contains("hello"));
    }

    #[test]
    fn format_event_tool_use() {
        let line = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"x"}}]}}"#;
        let out = format_event(line).unwrap();
        assert!(out.starts_with("* "));
        assert!(out.contains("Write"));
    }

    #[test]
    fn format_event_tool_result() {
        let line =
            br#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        let out = format_event(line).unwrap();
        assert!(out.starts_with("< "));
        assert!(out.contains("ok"));
    }

    #[test]
    fn format_event_result() {
        let line = br#"{"type":"result","session_id":"sess_abcdef12","usage":{"input_tokens":1,"output_tokens":2}}"#;
        let out = format_event(line).unwrap();
        assert!(out.starts_with("v "));
        assert!(out.contains("sess_abc"));
        assert!(out.contains("in=1"));
        assert!(out.contains("out=2"));
    }

    #[test]
    fn format_event_rate_limit() {
        let line = br#"{"type":"rate_limit_event","rate_limit_info":{"status":"throttled","rateLimitType":"five_hour","resetsAt":1234}}"#;
        let out = format_event(line).unwrap();
        assert!(out.starts_with("! "));
        assert!(out.contains("throttled"));
        assert!(out.contains("five_hour"));
    }

    #[test]
    fn format_event_skips_system() {
        let line = br#"{"type":"system","subtype":"init"}"#;
        assert!(format_event(line).is_none());
    }

    #[test]
    fn format_event_skips_unknown() {
        let line = br#"{"type":"totally_new_event","payload":"x"}"#;
        assert!(format_event(line).is_none());
    }

    #[test]
    fn format_event_skips_malformed() {
        let line = b"not json at all";
        assert!(format_event(line).is_none());
    }

    #[test]
    fn format_event_truncates_long_text() {
        let long = "a".repeat(500);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let out = format_event(line.as_bytes()).unwrap();
        // "> " prefix (2 chars) + at most 180 chars of content
        assert!(out.chars().count() <= 2 + 180);
    }
}
