//! Script action loop for fake-claude.
//!
//! Reads a JSONL script line-by-line from a `BufRead`, executing each
//! action in order. Existing action types (stdout/stderr/sleep_ms/
//! tool_use/mcp_call) preserve their prior behavior exactly. The
//! `usage`, `result`, and `rate_limit_event` actions added for #539
//! emit the corresponding stream-json wire shapes so integration tests
//! can exercise the budget_watch / SessionOutcome paths that those
//! events drive. The `tool_result` action added for #541 emits the
//! `user`-turn wrapper that real claude sends after every `tool_use`,
//! letting tests cover the full assistant→tool_use→user→tool_result
//! cycle through the dispatcher's stream-json pipeline.

#![allow(dead_code)]

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::bindings::{substitute, Bindings};
use crate::mcp_client::McpClient;

/// Monotonic counter used to generate unique tool_use ids within a process.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn random_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Execute the script file, dispatching each action in order. Returns
/// Ok(()) when the script completes without error.
///
/// `client` is only required when the script contains `mcp_call`
/// actions; if None, those actions return an error.
pub async fn execute_script<R: BufRead>(reader: R, mut client: Option<McpClient>) -> Result<()> {
    let mut bindings = Bindings::new();
    let stdout = io::stdout();
    let stderr = io::stderr();

    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
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
        } else if let Some(tr) = action.get("tool_result") {
            // Emit the user-turn wrapper that real claude sends after every
            // tool_use. Pair this with a prior `tool_use` action to exercise
            // the full assistant→tool_use→user→tool_result cycle through
            // the dispatcher's stream-json pipeline (parse_user /
            // Event::ToolResult). See issue #541.
            //
            // `content` accepts either a plain string (rendered as
            // `{"content":"..."}`) or any JSON value (passed through —
            // real claude uses an array of `{type, text}` blocks).
            let tool_use_id = tr
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = tr.get("content").cloned().unwrap_or(Value::Null);
            let is_error = tr
                .get("is_error")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut block = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
            });
            if is_error {
                block["is_error"] = Value::Bool(true);
            }
            let wrapper = serde_json::json!({
                "type": "user",
                "message": {
                    "content": [block]
                }
            });
            let mut out = stdout.lock();
            writeln!(out, "{}", serde_json::to_string(&wrapper)?)?;
            out.flush()?;
        } else if let Some(u) = action.get("usage") {
            // Emit an assistant message whose `message.usage` triggers
            // Event::AssistantUsage in the parser (#253). The `text` block
            // mirrors `tool_use` above: real claude always carries a content
            // block alongside the usage snapshot.
            let text = u.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let wrapper = serde_json::json!({
                "type": "assistant",
                "message": {
                    "content": [{ "type": "text", "text": text }],
                    "usage": {
                        "input_tokens": u.get("input").and_then(|v| v.as_u64()).unwrap_or(0),
                        "output_tokens": u.get("output").and_then(|v| v.as_u64()).unwrap_or(0),
                        "cache_read_input_tokens":
                            u.get("cache_read").and_then(|v| v.as_u64()).unwrap_or(0),
                        "cache_creation_input_tokens":
                            u.get("cache_creation").and_then(|v| v.as_u64()).unwrap_or(0),
                    }
                }
            });
            let mut out = stdout.lock();
            writeln!(out, "{}", serde_json::to_string(&wrapper)?)?;
            out.flush()?;
        } else if let Some(r) = action.get("result") {
            // Emit the terminal stream-json result line so tests can drive
            // the budget-watch finalization path and SessionOutcome's
            // session_id / token_usage fallback (#475 / #549).
            let session_id = r
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("sess-fake");
            let subtype = r
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("success");
            let result_text = r.get("text").and_then(|v| v.as_str()).unwrap_or("done");
            let usage = r
                .get("usage")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "input_tokens": 0, "output_tokens": 0 }));
            let wrapper = serde_json::json!({
                "type": "result",
                "subtype": subtype,
                "session_id": session_id,
                "result": result_text,
                "usage": usage,
            });
            let mut out = stdout.lock();
            writeln!(out, "{}", serde_json::to_string(&wrapper)?)?;
            out.flush()?;
        } else if let Some(rl) = action.get("rate_limit_event") {
            let status = rl
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let mut info = serde_json::json!({ "status": status });
            if let Some(kind) = rl.get("rate_limit_type").and_then(|v| v.as_str()) {
                info["rateLimitType"] = Value::String(kind.to_string());
            }
            if let Some(ts) = rl.get("resets_at").and_then(|v| v.as_u64()) {
                info["resetsAt"] = Value::from(ts);
            }
            let wrapper = serde_json::json!({
                "type": "rate_limit_event",
                "rate_limit_info": info,
            });
            let mut out = stdout.lock();
            writeln!(out, "{}", serde_json::to_string(&wrapper)?)?;
            out.flush()?;
        } else if let Some(call) = action.get("mcp_call") {
            let name = call
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("mcp_call at line {line_no} missing 'name' string"))?
                .to_string();
            let mut args = call.get("args").cloned().unwrap_or(Value::Null);
            let bind = call
                .get("bind")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let allow_err = call
                .get("allow_err")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            substitute(&mut args, &bindings)
                .with_context(|| format!("substitute at line {line_no}"))?;

            let Some(c) = client.as_mut() else {
                anyhow::bail!("mcp_call at line {line_no} requires PITBOSS_FAKE_MCP_SOCKET");
            };

            match c.call_tool(&name, args).await {
                Ok(result) => {
                    if let Some(name) = bind {
                        bindings.insert(name, result);
                    }
                }
                Err(e) => {
                    if allow_err {
                        eprintln!("fake-claude: mcp_call {name} (line {line_no}): {e:#}");
                    } else {
                        return Err(e.context(format!("mcp_call {name} at line {line_no}")));
                    }
                }
            }
        } else {
            anyhow::bail!("unknown action at line {line_no}: {line}");
        }
    }

    Ok(())
}
