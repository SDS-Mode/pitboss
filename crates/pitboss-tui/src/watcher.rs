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
/// Number of parsed focus-pane lines to keep. Deep scroll-back on long
/// runs is worth a few extra megabytes of in-memory state per focus
/// change (each line is ~1 KB on average).
const TAIL_LINES: usize = 2000;
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
        // Dynamic workers aren't in resolved.json, and TaskRecord doesn't
        // carry the model. For those the model_map returns None and the
        // Detail view would render "model ?". Pull the model out of the
        // first assistant event via an early-out scan — cheap relative
        // to the full scan_live_stats (which sums usage across every
        // assistant message and scales with log size).
        let model = model.or_else(|| scan_first_model(&log_path));
        TileState {
            id: id.to_string(),
            status: TileStatus::Done(rec.status.clone()),
            duration_ms: Some(rec.duration_ms),
            token_usage_input: rec.token_usage.input,
            token_usage_output: rec.token_usage.output,
            cache_read: rec.token_usage.cache_read,
            cache_creation: rec.token_usage.cache_creation,
            exit_code: rec.exit_code,
            log_path: log_path.clone(),
            model,
            parent_task_id: rec.parent_task_id.clone(),
            // Prefer TaskRecord.worktree_path (canonical for completed
            // tasks); fall back to the sidecar if the record didn't
            // capture it (unlikely, but defensive).
            worktree_path: rec
                .worktree_path
                .clone()
                .or_else(|| log_path.parent().and_then(read_worktree_sidecar)),
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
        // Mid-flight tiles don't have a summary record yet. Scan the log
        // directly to surface running token totals and (for dynamic
        // workers not in resolved.json) the model. Without this scan the
        // Detail view showed all zeros + "model ?" for up to the full
        // worker runtime on slow tasks.
        let (live_model, live_usage) = scan_live_stats(&log_path);
        // The task dir holds a `worktree.path` sidecar written at spawn
        // time by the dispatcher (mcp/tools.rs + dispatch/hierarchical.rs),
        // so the Detail view can run mid-flight git-diff against it
        // without waiting for the TaskRecord to land on settle.
        let task_dir = log_path.parent().map(std::path::Path::to_path_buf);
        let worktree_path = task_dir.as_ref().and_then(|d| read_worktree_sidecar(d));
        TileState {
            id: id.to_string(),
            status,
            duration_ms: None,
            token_usage_input: live_usage.input,
            token_usage_output: live_usage.output,
            cache_read: live_usage.cache_read,
            cache_creation: live_usage.cache_creation,
            exit_code: None,
            log_path,
            // Prefer the manifest-declared model (present for static tasks
            // + lead) and fall back to the log-derived one (dynamic workers).
            model: model.or(live_model),
            worktree_path,
            parent_task_id: if is_dynamic {
                parent_task_id_fallback.map(str::to_string)
            } else {
                None
            },
        }
    }
}

/// Read the `worktree.path` sidecar file written at spawn time.
/// Returns `None` when the file is missing (`use_worktree=false` task,
/// or an old run from before the sidecar was introduced) or empty.
fn read_worktree_sidecar(task_dir: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(task_dir.join("worktree.path")).ok()?;
    let s = String::from_utf8(bytes).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// Early-out scan: return the model from the first assistant message
/// in the log, or `None`. O(prefix-of-file) — stops as soon as the
/// first usable `{"type":"assistant", ...}` line is seen. Used for
/// completed tiles where the full usage is already in the `TaskRecord`
/// but the model wasn't carried.
fn scan_first_model(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if val.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(m) = val
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|v| v.as_str())
        {
            return Some(m.to_string());
        }
    }
    None
}

/// Full-file scan of a worker's stdout.log for mid-flight stats:
///   - model: from the first assistant message's `message.model`
///   - usage: summed token counts across every assistant message's
///     `message.usage` (each entry is per-turn, not cumulative).
///
/// O(log-size) on every snapshot tick — typical worker logs are well
/// under 1 MB so this stays in the tens of microseconds. If logs ever
/// grow past ~10 MB this should be promoted to a streaming scan with
/// a per-tile watermark so we only parse new bytes each tick.
fn scan_live_stats(path: &Path) -> (Option<String>, pitboss_core::parser::TokenUsage) {
    let Ok(file) = std::fs::File::open(path) else {
        return (None, pitboss_core::parser::TokenUsage::default());
    };
    let reader = BufReader::new(file);
    let mut model: Option<String> = None;
    let mut usage = pitboss_core::parser::TokenUsage::default();
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if val.get("type").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        let Some(msg) = val.get("message") else {
            continue;
        };
        if model.is_none() {
            if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                model = Some(m.to_string());
            }
        }
        if let Some(u) = msg.get("usage") {
            usage.input += u
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            usage.output += u
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            usage.cache_read += u
                .get("cache_read_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            usage.cache_creation += u
                .get("cache_creation_input_tokens")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
        }
    }
    (model, usage)
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

/// Per-event display caps. Originally tight for pre-word-wrap terminals;
/// word-wrap + visual-row scroll now handle long lines correctly, so the
/// caps can be generous. Truncated events get a `… +N chars` marker so
/// operators know there's more content than shown — full untruncated
/// stream-json is always in `<run-dir>/tasks/<id>/stdout.log`.
const CAP_ASSISTANT_TEXT: usize = 2000;
const CAP_TOOL_INPUT: usize = 1000;
const CAP_TOOL_RESULT: usize = 3000;

/// Parse one stream-json line and return a display string, or `None` to skip.
fn format_event(bytes: &[u8]) -> Option<String> {
    let event = parse_line(bytes).ok()?;
    match event {
        Event::AssistantText { text } => {
            let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or(&text);
            let capped = cap_with_marker(first_line, CAP_ASSISTANT_TEXT);
            Some(format!("> {capped}"))
        }
        Event::AssistantToolUse {
            tool_name,
            input_summary,
        } => {
            let summary = cap_with_marker(&input_summary, CAP_TOOL_INPUT);
            Some(format!("* {tool_name} {summary}"))
        }
        Event::ToolResult { content_summary } => {
            let capped = cap_with_marker(&content_summary, CAP_TOOL_RESULT);
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

/// Truncate `s` to at most `max_chars` characters (unchanged if shorter).
/// Primary use is `cap_with_marker`; kept as a raw-truncate primitive.
fn cap_str(s: &str, max_chars: usize) -> &str {
    // Find the byte offset of the `max_chars`-th char boundary.
    let mut chars = s.char_indices();
    if let Some((byte_idx, _)) = chars.nth(max_chars) {
        &s[..byte_idx]
    } else {
        s
    }
}

/// Truncate with an explicit ` … +N chars` marker when content is cut, so
/// operators see that more content exists without having to guess.
/// Returns an owned String so callers can format without lifetime acrobatics.
fn cap_with_marker(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let head = cap_str(s, max_chars);
    let extra = char_count - max_chars;
    format!("{head} … +{extra} chars")
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
    fn cap_with_marker_below_limit_is_unchanged() {
        assert_eq!(cap_with_marker("hello", 10), "hello");
    }

    #[test]
    fn cap_with_marker_above_limit_appends_count() {
        // "hello world" is 11 chars, cap at 5 → 6 trimmed.
        assert_eq!(cap_with_marker("hello world", 5), "hello … +6 chars");
    }

    #[test]
    fn cap_with_marker_counts_chars_not_bytes() {
        // Non-ASCII: each "→" is 1 char but 3 bytes.
        let s = "→→→→→"; // 5 chars, 15 bytes
        assert_eq!(cap_with_marker(s, 3), "→→→ … +2 chars");
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
        // Build content longer than the cap so we know truncation fires.
        let over = CAP_ASSISTANT_TEXT + 100;
        let long = "a".repeat(over);
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let out = format_event(line.as_bytes()).unwrap();
        // Output should contain the `… +N chars` marker and report the
        // correct delta (100 trimmed chars).
        assert!(out.contains("… +100 chars"), "expected marker, got: {out}");
        assert!(out.starts_with("> "));
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
    fn scan_live_stats_extracts_model_and_summed_usage() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stdout.log");
        // Two assistant turns with per-turn usage — expect sum. Model
        // taken from the first assistant message.
        let body = [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-7","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":3}}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-7","content":[{"type":"text","text":"b"}],"usage":{"input_tokens":2,"output_tokens":4,"cache_read_input_tokens":200,"cache_creation_input_tokens":1}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();

        let (model, usage) = scan_live_stats(&path);
        assert_eq!(model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(usage.input, 12);
        assert_eq!(usage.output, 9);
        assert_eq!(usage.cache_read, 300);
        assert_eq!(usage.cache_creation, 4);
    }

    #[test]
    fn scan_live_stats_missing_file_returns_defaults() {
        let (model, usage) = scan_live_stats(Path::new("/nonexistent/file.log"));
        assert!(model.is_none());
        assert_eq!(usage.input, 0);
        assert_eq!(usage.output, 0);
    }

    #[test]
    fn scan_first_model_returns_first_seen_model() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stdout.log");
        let body = [
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6","content":[]}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-7","content":[]}}"#,
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();
        assert_eq!(
            scan_first_model(&path).as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn scan_first_model_missing_file_returns_none() {
        assert!(scan_first_model(Path::new("/nonexistent.log")).is_none());
    }

    #[test]
    fn read_worktree_sidecar_returns_path_when_present() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("worktree.path"), "/some/worktree/path").unwrap();
        let got = read_worktree_sidecar(dir.path()).unwrap();
        assert_eq!(got, PathBuf::from("/some/worktree/path"));
    }

    #[test]
    fn read_worktree_sidecar_trims_trailing_whitespace() {
        // Dispatcher writes the raw path without a newline, but be
        // forgiving — handwritten or shell-piped values might have one.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("worktree.path"), "/some/path\n").unwrap();
        assert_eq!(
            read_worktree_sidecar(dir.path()),
            Some(PathBuf::from("/some/path"))
        );
    }

    #[test]
    fn read_worktree_sidecar_none_when_missing_or_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(read_worktree_sidecar(dir.path()).is_none());
        std::fs::write(dir.path().join("worktree.path"), "").unwrap();
        assert!(read_worktree_sidecar(dir.path()).is_none());
    }

    #[test]
    fn scan_live_stats_skips_malformed_and_non_assistant_lines() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("stdout.log");
        let body = [
            "not json at all",
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"user","message":{"content":[]}}"#,
            r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","content":[],"usage":{"input_tokens":7,"output_tokens":3}}}"#,
            "",
        ]
        .join("\n");
        std::fs::write(&path, body).unwrap();

        let (model, usage) = scan_live_stats(&path);
        assert_eq!(model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(usage.input, 7);
        assert_eq!(usage.output, 3);
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
