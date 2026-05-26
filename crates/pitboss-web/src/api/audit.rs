//! `GET /api/runs/:id/audit` — per-run aggregated audit log (#414).
//!
//! Reads `<run-dir>/audit.jsonl`, filters by query-string parameters,
//! and streams the result as NDJSON. Symmetric with the CLI
//! (`pitboss audit`) — same filter surface, same wire shape.
//!
//! Filter dimensions (all optional; combine multiplicatively):
//! - `actor` — exact match on `actor_id`.
//! - `kind` — exact match on the inner `event.kind` discriminator.
//! - `since` — RFC3339 cutoff; rows with `event.at < since` are dropped.
//! - `reason_kind` — exact match on `event.reason_kind`; implicitly
//!   filters out variants that lack a `reason_kind` field.
//!
//! 404 when the run never produced an audit row (file absent — every
//! run that runs a worker through Pitboss emits at least one
//! `tool_auto_approved` row, so this is rare in practice but possible
//! on freshly-dispatched in-progress runs).

use std::path::PathBuf;

use axum::{
    body::Body,
    extract::{Path as AxPath, Query, State},
    http::{header, StatusCode},
    response::Response,
};
use chrono::{DateTime, Utc};
use pitboss_core::audit::{AuditEntry, AuditFilter};
use serde::Deserialize;

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

#[derive(Debug, Default, Deserialize)]
pub struct AuditQuery {
    pub actor: Option<String>,
    pub kind: Option<String>,
    /// RFC3339 timestamp. Parsed lazily — a malformed value returns 400.
    pub since: Option<String>,
    pub reason_kind: Option<String>,
}

/// Per-run audit log handler. Reads `<run-dir>/audit.jsonl`, applies
/// query-string filters, and returns NDJSON.
pub async fn audit(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Response> {
    let run_dir = resolve_run_dir(state.runs_dir(), &run_id)?;
    let audit_path = run_dir.join("audit.jsonl");

    let since: Option<DateTime<Utc>> = match q.since.as_deref() {
        None => None,
        Some(s) => Some(
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| ApiError::BadRequest(format!("invalid `since` (RFC3339): {e}")))?,
        ),
    };

    let filter = AuditFilter {
        actor: q.actor,
        kind: q.kind,
        since,
        reason_kind: q.reason_kind,
    };

    let bytes = match tokio::fs::read(&audit_path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ApiError::NotFound),
        Err(e) => return Err(e.into()),
    };

    // CPU-light: filter in-process. Audit logs are single-digit-K rows
    // even on busy runs; a full-file parse is well within request-time
    // budget. If we ever see a real run that pushes this past ~100ms,
    // move to `tokio::task::spawn_blocking`.
    let filtered = filter_ndjson(&bytes, &filter);
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from(filtered))
        .expect("ndjson response"))
}

/// Filter a buffer of NDJSON audit rows down to those matching `filter`.
/// Re-emits the surviving rows as NDJSON. Malformed lines are skipped
/// (matches the CLI's tolerance).
fn filter_ndjson(bytes: &[u8], filter: &AuditFilter) -> Vec<u8> {
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::with_capacity(bytes.len());
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let entry: AuditEntry = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !filter.matches(&entry) {
            continue;
        }
        // Re-serialise from the typed form so the response shape is
        // canonical even if the on-disk row had whitespace / key
        // ordering quirks. Matches `audit::render`'s `--json` path.
        if let Ok(canonical) = serde_json::to_string(&entry) {
            out.extend_from_slice(canonical.as_bytes());
            out.push(b'\n');
        }
    }
    out
}

/// Resolve `<runs_dir>/<run_id>` defensively. Same shape as the
/// existing `runs::run_dir` helper in `runs.rs` — duplicated here so
/// this module doesn't depend on a sibling's private fn.
fn resolve_run_dir(runs_dir: &std::path::Path, run_id: &str) -> ApiResult<PathBuf> {
    if run_id.is_empty()
        || run_id.len() > 128
        || run_id == ".."
        || run_id == "."
        || run_id.contains('/')
        || run_id.contains('\\')
    {
        return Err(ApiError::BadRequest("invalid run id".into()));
    }
    Ok(runs_dir.join(run_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use pitboss_core::task_events::{DeniedReasonKind, TaskEvent};

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

    fn jsonl(entries: &[AuditEntry]) -> Vec<u8> {
        let mut s = String::new();
        for e in entries {
            s.push_str(&serde_json::to_string(e).unwrap());
            s.push('\n');
        }
        s.into_bytes()
    }

    #[test]
    fn no_filters_returns_every_row() {
        let now = Utc::now();
        let body = jsonl(&[entry_pause("w-1", now), entry_pause("w-2", now)]);
        let out = filter_ndjson(&body, &AuditFilter::default());
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn actor_filter_keeps_only_matching_id() {
        let now = Utc::now();
        let body = jsonl(&[entry_pause("w-1", now), entry_pause("w-2", now)]);
        let out = filter_ndjson(
            &body,
            &AuditFilter {
                actor: Some("w-2".into()),
                ..Default::default()
            },
        );
        let text = std::str::from_utf8(&out).unwrap();
        assert!(text.contains("\"actor_id\":\"w-2\""));
        assert!(!text.contains("\"actor_id\":\"w-1\""));
    }

    #[test]
    fn kind_filter_matches_serde_discriminator() {
        let now = Utc::now();
        let body = jsonl(&[
            entry_pause("w-1", now),
            entry_denied("w-1", now, DeniedReasonKind::DeniedByRule),
        ]);
        let out = filter_ndjson(
            &body,
            &AuditFilter {
                kind: Some("tool_denied".into()),
                ..Default::default()
            },
        );
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("tool_denied"));
    }

    #[test]
    fn since_filter_drops_earlier_rows() {
        let early = Utc::now() - Duration::minutes(5);
        let late = Utc::now();
        let body = jsonl(&[entry_pause("w-1", early), entry_pause("w-1", late)]);
        let cutoff = Utc::now() - Duration::minutes(2);
        let out = filter_ndjson(
            &body,
            &AuditFilter {
                since: Some(cutoff),
                ..Default::default()
            },
        );
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn reason_kind_filter_excludes_non_denied_variants() {
        let now = Utc::now();
        let body = jsonl(&[
            entry_denied("w-1", now, DeniedReasonKind::OperatorRejected),
            entry_pause("w-1", now),
        ]);
        let out = filter_ndjson(
            &body,
            &AuditFilter {
                reason_kind: Some("operator_rejected".into()),
                ..Default::default()
            },
        );
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("\"reason_kind\":\"operator_rejected\""),
            "wire form is snake_case: {}",
            lines[0]
        );
    }

    #[test]
    fn malformed_lines_skipped_not_fatal() {
        let now = Utc::now();
        let valid = serde_json::to_string(&entry_pause("w-1", now)).unwrap();
        let body = format!("{{\"truncated\":\n{valid}\n").into_bytes();
        let out = filter_ndjson(&body, &AuditFilter::default());
        let lines: Vec<&str> = std::str::from_utf8(&out).unwrap().lines().collect();
        assert_eq!(lines.len(), 1);
    }
}
