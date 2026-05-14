//! `pitboss audit <run-id>` — print the per-run aggregated audit log
//! (#414).
//!
//! Reads `<run-dir>/audit.jsonl`, which the dispatcher tees from every
//! `TaskEvent` emission (see [`crate::dispatch::events::append_event`]).
//! Each line is a [`TaskEvent`] payload plus a leading `actor_id`
//! field — `pitboss audit` is the run-wide companion to `pitboss
//! events`, which reads run-wide CONTROL envelopes; this one reads
//! run-wide TASK / approval / tool-permission events.
//!
//! Filters are applied in-memory after the file is read. Audit logs
//! are small (single-digit-K rows on a busy multi-worker run); a
//! full-file scan with structured-filter is the right v1 tradeoff for
//! simplicity.
//!
//! Resolution: the `<run-id>` argument accepts the full UUID or any
//! unique prefix (same convention as `pitboss events` / `pitboss
//! status`).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dispatch::events::TaskEvent;

/// One row of `audit.jsonl`. Mirrors the write-side
/// `dispatch::events::AuditEntry`: `actor_id` is the run-wide
/// attribution key, `event` is the nested [`TaskEvent`] payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub actor_id: String,
    pub event: TaskEvent,
}

/// Filter predicate evaluated against each parsed row. `None`-valued
/// fields are no-op (the row passes that filter dimension).
#[derive(Debug, Default, Clone)]
pub struct AuditFilter {
    pub actor: Option<String>,
    pub kind: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub reason_kind: Option<String>,
}

impl AuditFilter {
    pub fn matches(&self, entry: &AuditEntry) -> bool {
        if let Some(actor) = &self.actor {
            if &entry.actor_id != actor {
                return false;
            }
        }
        if let Some(kind) = &self.kind {
            if event_kind(&entry.event) != kind.as_str() {
                return false;
            }
        }
        if let Some(since) = self.since {
            if event_at(&entry.event) < since {
                return false;
            }
        }
        if let Some(rk) = &self.reason_kind {
            match event_reason_kind(&entry.event) {
                Some(have) if have == rk.as_str() => {}
                _ => return false,
            }
        }
        true
    }
}

/// Entry point for the `audit` subcommand.
pub fn run(
    run_id_prefix: &str,
    filter: AuditFilter,
    json: bool,
    run_dir_override: Option<PathBuf>,
) -> Result<i32> {
    let base = run_dir_override.unwrap_or_else(default_runs_dir);
    let run_dir = crate::runs::resolve_run_dir_by_prefix(&base, run_id_prefix)?;
    let audit_path = run_dir.join("audit.jsonl");

    let file = match std::fs::File::open(&audit_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "no audit.jsonl in {}; the run produced no TaskEvent rows \
                 (no approvals, no pauses, no tool denials). The audit log is \
                 written live as events flow — if the run is still in progress, \
                 wait for the first event, or check the per-actor files under \
                 {}/tasks/<actor>/events.jsonl.",
                run_dir.display(),
                run_dir.display(),
            );
        }
        Err(e) => return Err(e.into()),
    };

    render(
        BufReader::new(file),
        &filter,
        json,
        &mut std::io::stdout().lock(),
    )
}

/// Stream + filter + render into `out`. Factored so tests can drive
/// the render path against an in-memory buffer without touching disk
/// resolution.
pub fn render<R: BufRead, W: Write>(
    reader: R,
    filter: &AuditFilter,
    json: bool,
    out: &mut W,
) -> Result<i32> {
    let mut parsed = 0usize;
    let mut shown = 0usize;
    let mut skipped = 0usize;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<AuditEntry>(trimmed) {
            Ok(entry) => {
                parsed += 1;
                if !filter.matches(&entry) {
                    continue;
                }
                if json {
                    // Re-serialise from the typed form so passthrough
                    // and filtered output share the same canonical
                    // shape. (Alternative: pass `trimmed` through
                    // verbatim. Re-serialising costs a few µs per row
                    // and gives a stable wire shape on filter rounds.)
                    let canonical = serde_json::to_string(&entry)?;
                    writeln!(out, "{canonical}")?;
                } else {
                    writeln!(out, "{}", format_row(&entry))?;
                }
                shown += 1;
            }
            Err(_) => skipped += 1,
        }
    }

    if skipped > 0 {
        eprintln!(
            "pitboss audit: skipped {skipped} malformed line(s); rendered {shown} of {parsed} parsed row(s)",
        );
    }
    Ok(0)
}

/// Render one row as a single fixed-width line:
///
/// ```text
/// <ts>  <actor_id>          <kind>              <detail>
/// ```
fn format_row(entry: &AuditEntry) -> String {
    let ts = event_at(&entry.event).format("%Y-%m-%dT%H:%M:%SZ");
    let kind = event_kind(&entry.event);
    let detail = event_detail(&entry.event);
    format!(
        "{ts}  {actor_id:<24}  {kind:<18}  {detail}",
        ts = ts,
        actor_id = entry.actor_id,
        kind = kind,
        detail = detail,
    )
}

/// Stable string discriminator. Mirrors `#[serde(tag = "kind",
/// rename_all = "snake_case")]` on `TaskEvent`. Centralised here so
/// `--kind` filter values stay in lockstep with what the JSON
/// shows.
pub fn event_kind(event: &TaskEvent) -> &'static str {
    match event {
        TaskEvent::Pause { .. } => "pause",
        TaskEvent::Continue { .. } => "continue",
        TaskEvent::Reprompt { .. } => "reprompt",
        TaskEvent::ApprovalRequest { .. } => "approval_request",
        TaskEvent::ApprovalResponse { .. } => "approval_response",
        TaskEvent::NotificationFailed { .. } => "notification_failed",
        TaskEvent::ToolDenied { .. } => "tool_denied",
        TaskEvent::ToolAutoApproved { .. } => "tool_auto_approved",
    }
}

fn event_at(event: &TaskEvent) -> DateTime<Utc> {
    match event {
        TaskEvent::Pause { at, .. }
        | TaskEvent::Continue { at, .. }
        | TaskEvent::Reprompt { at, .. }
        | TaskEvent::ApprovalRequest { at, .. }
        | TaskEvent::ApprovalResponse { at, .. }
        | TaskEvent::NotificationFailed { at, .. }
        | TaskEvent::ToolDenied { at, .. }
        | TaskEvent::ToolAutoApproved { at, .. } => *at,
    }
}

/// Extract the `reason_kind` value where it exists (only on
/// `ToolDenied`). Returns `None` for variants without a reason kind;
/// `--reason-kind` filtering then excludes them entirely.
fn event_reason_kind(event: &TaskEvent) -> Option<&'static str> {
    match event {
        TaskEvent::ToolDenied { reason_kind, .. } => Some(match reason_kind {
            crate::dispatch::events::DeniedReasonKind::DeniedByRule => "denied_by_rule",
            crate::dispatch::events::DeniedReasonKind::DeniedByPolicy => "denied_by_policy",
            crate::dispatch::events::DeniedReasonKind::DeniedByProfile => "denied_by_profile",
            crate::dispatch::events::DeniedReasonKind::DeniedByMcpServerAllowlist => {
                "denied_by_mcp_server_allowlist"
            }
            crate::dispatch::events::DeniedReasonKind::OperatorRejected => "operator_rejected",
            crate::dispatch::events::DeniedReasonKind::TtlExpired => "ttl_expired",
        }),
        _ => None,
    }
}

fn event_detail(event: &TaskEvent) -> String {
    match event {
        TaskEvent::Pause { reason, .. } => reason
            .as_deref()
            .map(|r| format!("reason={r}"))
            .unwrap_or_default(),
        TaskEvent::Continue {
            new_session_id,
            prompt_preview,
            ..
        } => format!("session={new_session_id} prompt={prompt_preview:?}"),
        TaskEvent::Reprompt {
            prompt_preview,
            prior_session_id,
            ..
        } => format!("prior_session={prior_session_id} prompt={prompt_preview:?}"),
        TaskEvent::ApprovalRequest {
            request_id,
            summary_preview,
            ..
        } => format!("request_id={request_id} summary={summary_preview:?}"),
        TaskEvent::ApprovalResponse {
            request_id,
            approved,
            edited,
            ..
        } => format!("request_id={request_id} approved={approved} edited={edited}",),
        TaskEvent::NotificationFailed {
            sink_id,
            event_kind,
            error,
            ..
        } => format!("sink={sink_id} event_kind={event_kind} error={error:?}"),
        TaskEvent::ToolDenied {
            tool_name,
            reason_kind,
            reason,
            ..
        } => format!("tool={tool_name} reason_kind={reason_kind:?} reason={reason:?}",),
        TaskEvent::ToolAutoApproved {
            tool_name,
            actor_type,
            ..
        } => format!("tool={tool_name} actor_type={actor_type}"),
    }
}

fn default_runs_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".local/share/pitboss/runs")
}

/// Test helper exposed so integration tests can drive `render` against
/// a fixture path without re-implementing the open + render pipeline.
#[doc(hidden)]
pub fn render_to_string(audit_path: &Path, filter: &AuditFilter, json: bool) -> Result<String> {
    let file = std::fs::File::open(audit_path)?;
    let mut buf = Vec::new();
    render(BufReader::new(file), filter, json, &mut buf)?;
    Ok(String::from_utf8(buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::events::DeniedReasonKind;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn entry_pause(actor: &str, at: DateTime<Utc>) -> AuditEntry {
        AuditEntry {
            actor_id: actor.into(),
            event: TaskEvent::Pause {
                at,
                reason: Some("op".into()),
            },
        }
    }

    fn entry_denied(actor: &str, at: DateTime<Utc>, kind: DeniedReasonKind) -> AuditEntry {
        AuditEntry {
            actor_id: actor.into(),
            event: TaskEvent::ToolDenied {
                at,
                tool_name: "Bash".into(),
                actor_id: actor.into(),
                reason_kind: kind,
                reason: "test".into(),
            },
        }
    }

    fn jsonl(entries: &[AuditEntry]) -> String {
        let mut s = String::new();
        for e in entries {
            s.push_str(&serde_json::to_string(e).unwrap());
            s.push('\n');
        }
        s
    }

    #[test]
    fn columnar_render_includes_actor_kind_and_ts() {
        let now = Utc::now();
        let body = jsonl(&[entry_pause("w-1", now)]);
        let out = render_to_string_buf(&body, &AuditFilter::default(), false);
        assert!(out.contains("w-1"), "actor_id present: {out}");
        assert!(out.contains("pause"), "kind present: {out}");
        // No `--reason-kind` filter on a pause row → it passes
        // (event_reason_kind returns None only when the filter is set;
        // unset filter is always pass).
        assert!(!out.is_empty());
    }

    #[test]
    fn json_mode_round_trips_through_canonical_form() {
        let now = Utc::now();
        let body = jsonl(&[entry_pause("lead", now)]);
        let out = render_to_string_buf(&body, &AuditFilter::default(), true);
        let parsed: AuditEntry = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(parsed.actor_id, "lead");
        assert!(matches!(parsed.event, TaskEvent::Pause { .. }));
    }

    #[test]
    fn filter_actor_matches_exact_id_only() {
        let now = Utc::now();
        let body = jsonl(&[
            entry_pause("w-1", now),
            entry_pause("w-2", now),
            entry_pause("lead", now),
        ]);
        let out = render_to_string_buf(
            &body,
            &AuditFilter {
                actor: Some("w-1".into()),
                ..Default::default()
            },
            false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "filter to w-1 keeps one row: {out}");
        assert!(lines[0].contains("w-1"));
    }

    #[test]
    fn filter_kind_matches_serde_discriminator() {
        let now = Utc::now();
        let body = jsonl(&[
            entry_pause("w-1", now),
            entry_denied("w-1", now, DeniedReasonKind::DeniedByRule),
        ]);
        let out = render_to_string_buf(
            &body,
            &AuditFilter {
                kind: Some("tool_denied".into()),
                ..Default::default()
            },
            false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("tool_denied"));
    }

    #[test]
    fn filter_since_drops_earlier_rows() {
        let early = Utc::now() - chrono::Duration::minutes(5);
        let late = Utc::now();
        let body = jsonl(&[entry_pause("w-1", early), entry_pause("w-1", late)]);
        let cutoff = Utc::now() - chrono::Duration::minutes(2);
        let out = render_to_string_buf(
            &body,
            &AuditFilter {
                since: Some(cutoff),
                ..Default::default()
            },
            false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "only the late row passes: {out}");
    }

    #[test]
    fn filter_reason_kind_only_matches_tool_denied_rows() {
        let now = Utc::now();
        let body = jsonl(&[
            entry_denied("w-1", now, DeniedReasonKind::DeniedByRule),
            entry_denied("w-1", now, DeniedReasonKind::OperatorRejected),
            entry_pause("w-1", now),
        ]);
        let out = render_to_string_buf(
            &body,
            &AuditFilter {
                reason_kind: Some("denied_by_rule".into()),
                ..Default::default()
            },
            false,
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "denied_by_rule matches exactly one row: {out}"
        );
        assert!(lines[0].contains("DeniedByRule"));
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let now = Utc::now();
        let valid = serde_json::to_string(&entry_pause("w-1", now)).unwrap();
        let body = format!("{{\"actor_id\":\"truncated\"\n{valid}\n");
        let out = render_to_string_buf(&body, &AuditFilter::default(), false);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "valid line renders, torn line skipped: {out}"
        );
    }

    #[test]
    fn missing_file_gives_operator_friendly_error() {
        let tmp = TempDir::new().unwrap();
        let run_id = uuid::Uuid::now_v7().to_string();
        std::fs::create_dir_all(tmp.path().join(&run_id)).unwrap();
        let err = run(
            &run_id,
            AuditFilter::default(),
            false,
            Some(tmp.path().to_path_buf()),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("audit.jsonl"),
            "error should reference the file: {msg}",
        );
    }

    fn render_to_string_buf(body: &str, filter: &AuditFilter, json: bool) -> String {
        let mut out = Vec::new();
        render(Cursor::new(body), filter, json, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }
}
