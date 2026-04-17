//! Script action loop for fake-claude.
//!
//! Reads a JSONL script line-by-line from a `BufRead`, executing each
//! action in order. Existing action types (stdout/stderr/sleep_ms/
//! tool_use) preserve their pre-v0.4.1 behavior exactly. The new
//! `mcp_call` action lands in Task 5.

#![allow(dead_code)]

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

/// Monotonic counter used to generate unique tool_use ids within a process.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn random_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Execute the script file, dispatching each action in order. Returns
/// Ok(()) when the script completes without error.
///
/// For v0.4.1: the only action that needs async is `mcp_call` (Task 5).
/// Existing actions are sync; we call them directly from this async fn.
pub async fn execute_script<R: BufRead>(reader: R) -> Result<()> {
    let stdout = io::stdout();
    let stderr = io::stderr();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read error at line {line_no}"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let action: Value = serde_json::from_str(line)
            .with_context(|| format!("invalid JSON at line {line_no}: {line}"))?;

        if let Some(text) = action.get("stdout").and_then(|v| v.as_str()) {
            let mut out = stdout.lock();
            writeln!(out, "{text}")?;
            out.flush()?;
        } else if let Some(text) = action.get("stderr").and_then(|v| v.as_str()) {
            let mut err = stderr.lock();
            writeln!(err, "{text}")?;
            err.flush()?;
        } else if let Some(ms) = action.get("sleep_ms").and_then(|v| v.as_u64()) {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        } else if let Some(tu) = action.get("tool_use") {
            // Emit a stream-json tool_use event wrapper, mirroring how real
            // claude emits `{"type":"assistant","message":{"content":[...]}}`.
            let wrapper = serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{
                        "type": "tool_use",
                        "id": format!("call-{}", random_id()),
                        "name": tu.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                        "input": tu.get("input").cloned().unwrap_or(Value::Null),
                    }]
                }
            });
            let mut out = stdout.lock();
            writeln!(out, "{}", serde_json::to_string(&wrapper)?)?;
            out.flush()?;
        } else {
            eprintln!("fake-claude: unknown action at line {line_no}: {line}");
        }
    }

    Ok(())
}
