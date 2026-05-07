//! Per-worker events.jsonl writer. Captures pause/continue/reprompt/approval
//! events as an append-only JSONL stream. Not loaded by the dispatcher; a
//! post-hoc audit trail only.

#![allow(dead_code)]

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Why a Path-B `permission_prompt` ended in `decision = "deny"`.
/// Mirrors `crate::mcp::tools::approval::PermissionDenialReason` but
/// lives in the events module so the audit log file owns its own
/// serialization shape (independent of any future refactor of the
/// MCP-side enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedReasonKind {
    /// An operator-declared `[[approval_policy]]` rule with
    /// `action = "auto_reject"` matched.
    DeniedByRule,
    /// Operator (TUI / web console) responded with reject.
    OperatorRejected,
    /// TTL on the queued approval expired and the fallback fired.
    TtlExpired,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEvent {
    Pause {
        at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Continue {
        at: DateTime<Utc>,
        new_session_id: String,
        prompt_preview: String,
    },
    Reprompt {
        at: DateTime<Utc>,
        prompt_preview: String,
        prior_session_id: String,
    },
    ApprovalRequest {
        at: DateTime<Utc>,
        request_id: String,
        summary_preview: String,
    },
    ApprovalResponse {
        at: DateTime<Utc>,
        request_id: String,
        approved: bool,
        edited: bool,
    },
    NotificationFailed {
        at: DateTime<Utc>,
        sink_id: String,
        event_kind: String,
        error: String,
    },
    /// A Path-B `permission_prompt` returned `decision = "deny"`. The
    /// model receives a denial it can adapt to without an operator
    /// round-trip; this row gives the operator post-hoc visibility into
    /// what was attempted-and-blocked.
    ToolDenied {
        at: DateTime<Utc>,
        /// Name of the claude tool the model wanted to invoke
        /// (e.g. `"Bash"`, `"Write"`).
        tool_name: String,
        /// Caller actor id (lead / sublead / worker).
        actor_id: String,
        /// Categorical reason for the denial.
        reason_kind: DeniedReasonKind,
        /// Free-form text returned to claude (and to the model). Carries
        /// either the canned per-kind message or, when the operator
        /// declined with a free-form comment, the operator's text.
        reason: String,
    },
}

/// Append one event to `<run_subdir>/tasks/<task_id>/events.jsonl`.
/// Creates the directory if absent.
pub async fn append_event(run_subdir: &Path, task_id: &str, event: &TaskEvent) -> Result<()> {
    let dir = run_subdir.join("tasks").join(task_id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("events.jsonl");
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    f.write_all(line.as_bytes()).await?;
    f.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn append_creates_file_and_writes_event() {
        let dir = TempDir::new().unwrap();
        let ev = TaskEvent::Pause {
            at: Utc::now(),
            reason: Some("op requested".into()),
        };
        append_event(dir.path(), "w-1", &ev).await.unwrap();
        let path = dir.path().join("tasks").join("w-1").join("events.jsonl");
        let content = tokio::fs::read_to_string(path).await.unwrap();
        assert!(content.contains("\"kind\":\"pause\""));
        assert!(content.contains("\"reason\":\"op requested\""));
    }

    #[tokio::test]
    async fn multiple_appends_are_jsonl() {
        let dir = TempDir::new().unwrap();
        append_event(
            dir.path(),
            "w-2",
            &TaskEvent::Pause {
                at: Utc::now(),
                reason: None,
            },
        )
        .await
        .unwrap();
        append_event(
            dir.path(),
            "w-2",
            &TaskEvent::Continue {
                at: Utc::now(),
                new_session_id: "sess".into(),
                prompt_preview: "next".into(),
            },
        )
        .await
        .unwrap();
        let path = dir.path().join("tasks").join("w-2").join("events.jsonl");
        let content = tokio::fs::read_to_string(path).await.unwrap();
        assert_eq!(content.lines().count(), 2);
    }
}
