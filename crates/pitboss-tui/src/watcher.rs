//! Background watcher thread — polls the run directory every 500ms and
//! emits `AppSnapshot` updates via an mpsc channel.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use pitboss_core::parser::{parse_line, Event};
use pitboss_core::store::{TaskRecord, TaskStatus};
use serde::Deserialize;

use crate::state::{AppSnapshot, TileState, TileStatus};

const POLL_INTERVAL_MS: u64 = 250;
/// Number of parsed focus-pane lines to keep.
const TAIL_LINES: usize = 40;
/// Maximum bytes to read off the end of a log file when tailing. Chosen so
/// that even a verbose stream-json run has >> `TAIL_LINES` rendered events
/// within this window, without re-parsing multi-megabyte logs every poll.
const TAIL_BYTES: u64 = 256 * 1024;
/// A task is considered "running" if its log was modified within this many seconds.
const RUNNING_FRESHNESS_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// resolved.json schema (only the fields we care about)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ResolvedTask {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolvedLead {
    pub id: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResolvedManifest {
    #[serde(default)]
    pub tasks: Vec<ResolvedTask>,
    #[serde(default)]
    pub lead: Option<ResolvedLead>,
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
            // Drain any pending focus updates (non-blocking).
            while let Ok(id) = focus_rx.try_recv() {
                focused_id = Some(id);
            }

            let snapshot = build_snapshot(&run_dir, focused_id.as_deref());
            // If the receiver is gone (app quit), exit thread.
            if snapshot_tx.try_send(snapshot).is_err() {
                break;
            }

            // Wait up to POLL_INTERVAL_MS for the next tick, OR wake early
            // when the user changes focus so the new tile's log tails
            // without an up-to-half-second delay.
            match focus_rx.recv_timeout(Duration::from_millis(POLL_INTERVAL_MS)) {
                Ok(id) => focused_id = Some(id),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Snapshot construction
// ---------------------------------------------------------------------------

fn build_snapshot(run_dir: &Path, focused_id: Option<&str>) -> AppSnapshot {
    // 1. Read resolved.json → get static task ids, models, and lead (if any).
    let (resolved_tasks, resolved_lead) = read_resolved_manifest(run_dir);
    let model_map = build_model_map(&resolved_tasks, resolved_lead.as_ref());
    let static_ids = collect_static_ids(&resolved_tasks, resolved_lead.as_ref());

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

    // 3. Dynamic worker ids come from two sources:
    //    - `summary.jsonl`/`summary.json` records for completed workers not in
    //      the static set.
    //    - `tasks/<id>/` filesystem subdirectories for workers that have been
    //      spawned but may not yet have a summary record (still running).
    let tasks_dir = run_dir.join("tasks");
    let dynamic_ids = collect_dynamic_ids(&completed, &static_ids, &tasks_dir);

    // 4. All tile ids = static (tasks + lead) then dynamic (sorted).
    let all_ids: Vec<String> = static_ids
        .iter()
        .cloned()
        .chain(dynamic_ids.iter().cloned())
        .collect();

    // For dynamic tiles without a completed record, default parent_task_id to
    // the lead id — most useful display.
    let parent_task_id_fallback = resolved_lead.as_ref().map(|l| l.id.clone());

    // 5. Build tile states.
    let mut tasks: Vec<TileState> = Vec::with_capacity(all_ids.len());
    let mut failed_count = 0usize;
    let mut run_started_at: Option<chrono::DateTime<chrono::Utc>> = None;

    for id in &all_ids {
        let log_path = tasks_dir.join(id).join("stdout.log");
        let model = model_map.get(id).and_then(Option::clone);
        let is_dynamic = dynamic_ids.iter().any(|d| d == id);
        let tile = build_tile(
            id,
            log_path,
            model,
            is_dynamic,
            completed.get(id),
            parent_task_id_fallback.as_deref(),
            &mut failed_count,
            &mut run_started_at,
        );
        tasks.push(tile);
    }

    // 6. Tail the focused tile's log.
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
        run_started_at,
    }
}

/// Read and parse `resolved.json`, returning the static tasks and optional
/// lead. Missing or malformed files yield empty values.
fn read_resolved_manifest(run_dir: &Path) -> (Vec<ResolvedTask>, Option<ResolvedLead>) {
    let resolved_path = run_dir.join("resolved.json");
    match std::fs::read(&resolved_path) {
        Ok(bytes) => match serde_json::from_slice::<ResolvedManifest>(&bytes) {
            Ok(m) => (m.tasks, m.lead),
            Err(_) => (Vec::new(), None),
        },
        Err(_) => (Vec::new(), None),
    }
}

/// Build a map from task/lead id → model for quick lookup when constructing
/// tiles.
fn build_model_map(
    tasks: &[ResolvedTask],
    lead: Option<&ResolvedLead>,
) -> std::collections::HashMap<String, Option<String>> {
    let mut map: std::collections::HashMap<String, Option<String>> = tasks
        .iter()
        .map(|t| (t.id.clone(), t.model.clone()))
        .collect();
    if let Some(lead) = lead {
        map.insert(lead.id.clone(), lead.model.clone());
    }
    map
}

/// Static ids = tasks from `resolved.json` plus the lead (if present).
fn collect_static_ids(tasks: &[ResolvedTask], lead: Option<&ResolvedLead>) -> Vec<String> {
    let mut ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    if let Some(lead) = lead {
        ids.push(lead.id.clone());
    }
    ids
}

/// Dynamic ids come from completed records and `tasks/<id>/` subdirs, minus any
/// id already in `static_ids`. The result is sorted for stable display order.
fn collect_dynamic_ids(
    completed: &std::collections::HashMap<String, TaskRecord>,
    static_ids: &[String],
    tasks_dir: &Path,
) -> Vec<String> {
    let mut ids: Vec<String> = completed
        .keys()
        .filter(|k| !static_ids.iter().any(|s| s == *k))
        .cloned()
        .collect();
    if let Ok(entries) = std::fs::read_dir(tasks_dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    let name = name.to_string();
                    if !static_ids.iter().any(|s| s == &name) && !ids.iter().any(|s| s == &name) {
                        ids.push(name);
                    }
                }
            }
        }
    }
    ids.sort();
    ids
}

/// Build a single tile, updating `failed_count` and `run_started_at` as side
/// effects when a completed record is present.
#[allow(clippy::too_many_arguments)]
fn build_tile(
    id: &str,
    log_path: PathBuf,
    model: Option<String>,
    is_dynamic: bool,
    rec: Option<&TaskRecord>,
    parent_task_id_fallback: Option<&str>,
    failed_count: &mut usize,
    run_started_at: &mut Option<chrono::DateTime<chrono::Utc>>,
) -> TileState {
    if let Some(rec) = rec {
        if !matches!(rec.status, TaskStatus::Success) {
            *failed_count += 1;
        }
        // Track earliest started_at across all completed tiles.
        match *run_started_at {
            None => *run_started_at = Some(rec.started_at),
            Some(existing) if rec.started_at < existing => {
                *run_started_at = Some(rec.started_at);
            }
            _ => {}
        }
        TileState {
            id: id.to_string(),
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
        }
    } else {
        // Decide between Pending and Running by checking log freshness.
        let status = log_freshness_secs(&log_path).map_or(TileStatus::Pending, |age| {
            if age <= RUNNING_FRESHNESS_SECS {
                TileStatus::Running
            } else {
                TileStatus::Pending
            }
        });
        TileState {
            id: id.to_string(),
            status,
            duration_ms: None,
            token_usage_input: 0,
            token_usage_output: 0,
            cache_read: 0,
            cache_creation: 0,
            exit_code: None,
            log_path,
            model,
            parent_task_id: if is_dynamic {
                parent_task_id_fallback.map(str::to_string)
            } else {
                None
            },
        }
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
    if let Ok(summary) = serde_json::from_slice::<pitboss_core::store::RunSummary>(&bytes) {
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
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    let file_size = meta.len();

    // Seek to the last TAIL_BYTES of the file. If the file is smaller than
    // that, read from the beginning. When we seek mid-file we drop the first
    // line — it's almost certainly partial (we landed inside an existing
    // stream-json record).
    let start = file_size.saturating_sub(TAIL_BYTES);
    let seeked_partial = start > 0;
    if seeked_partial && file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }

    let reader = BufReader::new(file);
    let mut lines = reader.lines().map_while(Result::ok);
    if seeked_partial {
        let _ = lines.next();
    }

    let mut rendered: Vec<String> = Vec::new();
    for raw_line in lines {
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

    #[test]
    fn watcher_sees_lead_as_tile_in_hierarchical_run() {
        let dir = tempfile::TempDir::new().unwrap();
        let run_dir = dir.path().to_path_buf();

        // Write resolved.json with a lead and no tasks.
        let resolved = serde_json::json!({
            "max_parallel": 4,
            "halt_on_failure": false,
            "run_dir": run_dir.to_str(),
            "worktree_cleanup": "OnSuccess",
            "emit_event_stream": false,
            "tasks": [],
            "lead": {"id": "triage-lead", "model": "claude-haiku-4-5"},
            "max_workers": 4,
            "budget_usd": 5.0,
            "lead_timeout_secs": 900
        });
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::to_vec(&resolved).unwrap(),
        )
        .unwrap();

        let snap = build_snapshot(&run_dir, None);
        assert_eq!(snap.tasks.len(), 1);
        assert_eq!(snap.tasks[0].id, "triage-lead");
    }

    #[test]
    fn watcher_discovers_dynamic_workers_from_summary_jsonl() {
        let dir = tempfile::TempDir::new().unwrap();
        let run_dir = dir.path().to_path_buf();

        // Write resolved.json with just a lead.
        let resolved = serde_json::json!({
            "max_parallel": 4,
            "halt_on_failure": false,
            "run_dir": run_dir.to_str(),
            "worktree_cleanup": "OnSuccess",
            "emit_event_stream": false,
            "tasks": [],
            "lead": {"id": "lead", "model": "claude-haiku-4-5"},
            "max_workers": 4,
            "budget_usd": 5.0,
            "lead_timeout_secs": 900
        });
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::to_vec(&resolved).unwrap(),
        )
        .unwrap();

        // Write a summary.jsonl entry for a dynamically-spawned worker.
        let worker_rec = serde_json::json!({
            "task_id": "worker-abc",
            "status": "Success",
            "exit_code": 0,
            "started_at": "2026-04-17T00:00:00Z",
            "ended_at": "2026-04-17T00:00:30Z",
            "duration_ms": 30000,
            "worktree_path": null,
            "log_path": run_dir.join("tasks/worker-abc/stdout.log").to_str(),
            "token_usage": {"input": 100, "output": 200, "cache_read": 0, "cache_creation": 0},
            "claude_session_id": null,
            "final_message_preview": null,
            "parent_task_id": "lead"
        });
        let mut jsonl_line = serde_json::to_vec(&worker_rec).unwrap();
        jsonl_line.push(b'\n');
        std::fs::write(run_dir.join("summary.jsonl"), jsonl_line).unwrap();

        let snap = build_snapshot(&run_dir, None);
        let ids: Vec<&str> = snap.tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"lead"), "lead tile missing: {ids:?}");
        assert!(
            ids.contains(&"worker-abc"),
            "dynamic worker tile missing: {ids:?}"
        );
        let worker_tile = snap.tasks.iter().find(|t| t.id == "worker-abc").unwrap();
        assert_eq!(worker_tile.parent_task_id.as_deref(), Some("lead"));
    }

    #[test]
    fn watcher_discovers_live_workers_from_tasks_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let run_dir = dir.path().to_path_buf();

        // Write resolved.json with just a lead.
        let resolved = serde_json::json!({
            "max_parallel": 4,
            "halt_on_failure": false,
            "run_dir": run_dir.to_str(),
            "worktree_cleanup": "OnSuccess",
            "emit_event_stream": false,
            "tasks": [],
            "lead": {"id": "lead", "model": "claude-haiku-4-5"},
            "max_workers": 4,
            "budget_usd": 5.0,
            "lead_timeout_secs": 900
        });
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::to_vec(&resolved).unwrap(),
        )
        .unwrap();

        // Create a tasks/<worker-id>/ directory with no summary record yet.
        std::fs::create_dir_all(run_dir.join("tasks/worker-live")).unwrap();

        let snap = build_snapshot(&run_dir, None);
        let ids: Vec<&str> = snap.tasks.iter().map(|t| t.id.as_str()).collect();
        assert!(
            ids.contains(&"worker-live"),
            "live worker tile missing: {ids:?}"
        );
        let live_tile = snap.tasks.iter().find(|t| t.id == "worker-live").unwrap();
        assert_eq!(live_tile.parent_task_id.as_deref(), Some("lead"));
    }

    #[test]
    fn tail_log_small_file_returns_all_events() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stdout.log");
        let body = [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"second"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"third"}]}}"#,
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();

        let out = tail_log(&path, 40);
        assert_eq!(out.len(), 3, "expected 3 events, got {out:?}");
        assert!(out[0].contains("first"));
        assert!(out[2].contains("third"));
    }

    #[test]
    fn tail_log_large_file_returns_tail_and_drops_partial_first_line() {
        use std::fmt::Write;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stdout.log");

        // Build >TAIL_BYTES of events. Each line here is ~100 bytes; 3000
        // lines puts us solidly past the 256 KiB window.
        let mut body = String::new();
        for i in 0..3000 {
            writeln!(
                &mut body,
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"event-{i:05}"}}]}}}}"#
            ).unwrap();
        }
        // Add a final distinctive event so we can assert it's in the tail.
        body.push_str(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"LAST"}]}}"#,
        );
        std::fs::write(&path, &body).unwrap();

        let out = tail_log(&path, 40);
        assert_eq!(
            out.len(),
            40,
            "expected exactly TAIL_LINES=40, got {}",
            out.len()
        );
        // The tail must include the final LAST event.
        assert!(
            out.last().unwrap().contains("LAST"),
            "last rendered event: {:?}",
            out.last()
        );
        // The first rendered event must NOT be from the very start of the file
        // — we seeked past it. event-00000 is at byte 0, and 256 KiB of events
        // later we'll be well into the middle.
        assert!(
            !out.iter().any(|s| s.contains("event-00000")),
            "tail unexpectedly included the file head: {:?}",
            out.first()
        );
    }

    #[test]
    fn tail_log_missing_file_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let out = tail_log(&dir.path().join("does-not-exist.log"), 40);
        assert!(out.is_empty());
    }
}
