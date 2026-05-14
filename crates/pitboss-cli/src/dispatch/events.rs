//! Per-worker events.jsonl writer. Captures pause/continue/reprompt/approval
//! events as an append-only JSONL stream. Not loaded by the dispatcher; a
//! post-hoc audit trail only.

#![allow(dead_code)]

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Why a Path-B `permission_prompt` returned `behavior = "deny"`.
/// Surfaced to the model as the `message` field of
/// `PermissionPromptResponse::Deny` and recorded as the `reason_kind`
/// on `TaskEvent::ToolDenied` rows of the per-actor `events.jsonl`.
///
/// This was previously duplicated as
/// `mcp::tools::approval::PermissionDenialReason` (with a `From` impl
/// connecting the two) — collapsed into a single canonical type in
/// #370 (item 4) since the duplication couldn't actually diverge:
/// adding a variant on either side broke the `From` impl at compile
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeniedReasonKind {
    /// An operator-declared `[[approval_policy]]` rule with
    /// `action = "auto_reject"` matched.
    DeniedByRule,
    /// `default_approval_policy = "auto_reject"` fired inside the
    /// bridge — no operator was involved. Distinguished from
    /// `OperatorRejected` so audit logs reflect what actually decided
    /// the denial. (#373)
    DeniedByPolicy,
    /// The caller's `[[worker_type]]` / `[[sublead_type]]` profile
    /// does not list the requested tool in its `tools` allowlist.
    /// The manifest is the consent signal — anything not declared on
    /// the profile auto-denies without an operator round-trip (#252).
    /// Distinguished from `DeniedByRule` so operators can tell at audit
    /// time whether a deny came from the cross-cutting rule machinery
    /// or from a per-class profile cap.
    DeniedByProfile,
    /// The requested tool resolves to `mcp__<server>__<tool>` for a
    /// `[[mcp_server]]` whose declared `tools = […]` allowlist does
    /// not include `<tool>`. Path-B-only enforcement: under Path A
    /// (`bypassPermissions`) claude skips the entire permission layer
    /// and the gate has no effect. (#391 / #399)
    DeniedByMcpServerAllowlist,
    /// Operator (TUI / web console) responded with reject.
    OperatorRejected,
    /// TTL on the queued approval expired and the fallback fired.
    TtlExpired,
}

impl DeniedReasonKind {
    /// Short, model-readable explanation. Phrasing follows the
    /// convention "denied: <kind> ..." so a Claude session can detect
    /// the prefix and adapt without parsing structured fields.
    ///
    /// `DeniedByProfile` returns a generic message; callers with a
    /// concrete actor-type id should prefer
    /// [`Self::profile_message`] for the more specific
    /// "not in worker_type 'X' allowlist" phrasing the plan called for.
    pub fn message(self, tool_name: &str) -> String {
        match self {
            Self::DeniedByRule => {
                format!("denied: tool '{tool_name}' rejected by [[approval_policy]] rule")
            }
            Self::DeniedByPolicy => {
                format!("denied: tool '{tool_name}' blocked by default approval policy")
            }
            Self::DeniedByProfile => {
                format!("denied: tool '{tool_name}' not in actor profile allowlist")
            }
            Self::DeniedByMcpServerAllowlist => {
                format!("denied: tool '{tool_name}' not in mcp_server allowlist")
            }
            Self::OperatorRejected => {
                format!("denied: tool '{tool_name}' rejected by operator")
            }
            Self::TtlExpired => {
                format!("denied: tool '{tool_name}' approval timed out before operator response")
            }
        }
    }

    /// Specific phrasing for [`Self::DeniedByProfile`] when the caller
    /// knows the profile id and role (worker / sublead). The model
    /// sees the type id and can reason about which profile to comply
    /// with — `denied: tool 'Bash' not in worker_type 'extraction'
    /// allowlist`.
    pub fn profile_message(tool_name: &str, role: &str, type_id: &str) -> String {
        format!("denied: tool '{tool_name}' not in {role}_type '{type_id}' allowlist")
    }

    /// Specific phrasing for [`Self::DeniedByMcpServerAllowlist`]. The
    /// model sees both the server id and the tool name so it can
    /// rephrase its request without an operator round-trip — e.g.,
    /// `denied: tool 'mcp__fs-writer__delete_all' not in mcp_server
    /// 'fs-writer' allowlist`.
    pub fn mcp_server_allowlist_message(
        tool_name: &str,
        server_id: &str,
        bare_tool: &str,
    ) -> String {
        format!(
            "denied: tool '{tool_name}' (mcp_server '{server_id}' tool '{bare_tool}') \
             not in server allowlist"
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// A Path-B `permission_prompt` returned `behavior = "deny"`. The
    /// model receives a structured `{behavior, message, interrupt}`
    /// denial it can adapt to without an operator round-trip; this row
    /// gives the operator post-hoc visibility into what was
    /// attempted-and-blocked.
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
    /// A Path-B `permission_prompt` returned `behavior = "allow"` via
    /// the typed-profile short-circuit (#252) — the requested tool was
    /// in the caller's `[[worker_type]]` / `[[sublead_type]]` `tools`
    /// allowlist, so pitboss approved without prompting the operator.
    /// Symmetric to `ToolDenied` so audit logs show both sides of the
    /// profile-driven gate, not only the rejections.
    ToolAutoApproved {
        at: DateTime<Utc>,
        /// Tool the model invoked (e.g. `"Read"`, `"Glob"`).
        tool_name: String,
        /// Caller actor id (sublead / worker; lead is never typed).
        actor_id: String,
        /// Resolved profile id (`worker_type` or `sublead_type`).
        actor_type: String,
    },
}

/// Wrapper for a row in `<run_subdir>/audit.jsonl` (#414).
///
/// `actor_id` is the run-wide attribution key; `event` carries the
/// existing [`TaskEvent`] payload verbatim.
///
/// (A `#[serde(flatten)]` design was rejected because the
/// `ToolDenied` and `ToolAutoApproved` variants already carry an
/// inner `actor_id` field — flatten would consume the wrapper's
/// `actor_id` and leave the variant unable to deserialize its
/// required payload field. The nested `event` shape avoids the
/// collision and round-trips cleanly through serde.)
#[derive(Debug, Serialize)]
struct AuditEntry<'a> {
    actor_id: &'a str,
    event: &'a TaskEvent,
}

/// Append one event to `<run_subdir>/tasks/<task_id>/events.jsonl`.
/// Creates the directory if absent.
///
/// PR #414 of issue #414 also tees a wrapped record to
/// `<run_subdir>/audit.jsonl` so operators can query a single run-wide
/// audit trail without walking the per-actor tree. The tee is
/// best-effort: a failure on the run-wide write logs and falls
/// through; the per-actor record (the durable per-task audit source
/// used by the TUI denial counter and `pitboss status`) is still
/// written.
pub async fn append_event(run_subdir: &Path, task_id: &str, event: &TaskEvent) -> Result<()> {
    let dir = run_subdir.join("tasks").join(task_id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("events.jsonl");
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    write_jsonl_line(&path, line.as_bytes()).await?;

    // #414: run-wide audit log tee. Logged-and-swallowed so a
    // transient failure on the aggregated file doesn't break the
    // per-actor record. Re-serialise rather than re-using `line`
    // because the audit row has the extra `actor_id` field.
    let audit_path = run_subdir.join("audit.jsonl");
    match serde_json::to_string(&AuditEntry {
        actor_id: task_id,
        event,
    }) {
        Ok(mut audit_line) => {
            audit_line.push('\n');
            if let Err(e) = write_jsonl_line(&audit_path, audit_line.as_bytes()).await {
                tracing::warn!(
                    error = %e,
                    path = %audit_path.display(),
                    "audit.jsonl: tee append failed (per-actor record still written)",
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "audit.jsonl: serialize failed; tee skipped");
        }
    }
    Ok(())
}

/// Open-create-append + flush. Single-shot write of an already-serialised
/// JSONL line (caller supplies the trailing newline). Factored so the
/// per-actor write and the #414 audit tee share one syscall sequence.
async fn write_jsonl_line(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    f.write_all(bytes).await?;
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

    /// #414: `append_event` writes the per-actor file (durable trail
    /// the TUI / `pitboss status` already consume) AND tees a wrapped
    /// row with explicit `actor_id` into the run-wide audit log.
    #[tokio::test]
    async fn append_tees_into_run_wide_audit_log() {
        let dir = TempDir::new().unwrap();
        let now = Utc::now();
        append_event(
            dir.path(),
            "lead",
            &TaskEvent::Pause {
                at: now,
                reason: Some("operator".into()),
            },
        )
        .await
        .unwrap();
        append_event(
            dir.path(),
            "worker-7",
            &TaskEvent::ToolDenied {
                at: now,
                tool_name: "Bash".into(),
                actor_id: "worker-7".into(),
                reason_kind: DeniedReasonKind::DeniedByRule,
                reason: "shell access denied".into(),
            },
        )
        .await
        .unwrap();

        let audit = tokio::fs::read_to_string(dir.path().join("audit.jsonl"))
            .await
            .unwrap();
        let lines: Vec<&str> = audit.lines().collect();
        assert_eq!(lines.len(), 2, "tee'd into two rows: {audit}");
        assert!(
            lines[0].contains("\"actor_id\":\"lead\""),
            "first row must carry lead actor_id: {}",
            lines[0]
        );
        assert!(lines[0].contains("\"event\":{\"kind\":\"pause\""));
        assert!(
            lines[1].starts_with("{\"actor_id\":\"worker-7\""),
            "second row must lead with worker-7 actor_id: {}",
            lines[1]
        );
        assert!(lines[1].contains("\"event\":{\"kind\":\"tool_denied\""));
        assert!(lines[1].contains("\"reason_kind\":\"denied_by_rule\""));

        // Per-actor files still got their copy (the tee doesn't replace
        // them; both writes are durable).
        let lead_per_actor = dir.path().join("tasks").join("lead").join("events.jsonl");
        assert!(lead_per_actor.exists());
        let worker_per_actor = dir
            .path()
            .join("tasks")
            .join("worker-7")
            .join("events.jsonl");
        assert!(worker_per_actor.exists());
    }
}
