//! Run-scoped read endpoints (Phase 1 — filesystem only).
//!
//! All endpoints reject `run_id` values containing path separators or
//! `..` — every read is anchored inside `state.runs_dir()` and we never
//! join an attacker-controlled segment without re-validating it as a
//! single path component.

use std::path::{Path, PathBuf};

use axum::{
    body::Body,
    extract::{Path as AxPath, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::{
    error::{ApiError, ApiResult},
    runs_index::{list_runs, RunDto},
    state::AppState,
};

pub async fn list(State(state): State<AppState>) -> Json<Vec<RunDto>> {
    Json(list_runs(state.runs_dir()))
}

/// `GET /api/runs/:id` — returns the full `summary.json` if present, or
/// a synthesised stub from the run dir's metadata. Pass-through bytes so
/// the schema is whatever pitboss-core writes today; the frontend doesn't
/// need a typed mirror to render.
pub async fn detail(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
) -> ApiResult<Response> {
    let run_dir = run_dir(state.runs_dir(), &run_id)?;
    let summary_path = run_dir.join("summary.json");
    match tokio::fs::read(&summary_path).await {
        Ok(bytes) => Ok(json_response(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No finalized summary yet — synthesise a thin stub from the
            // run-list classifier so the frontend has *something* to show.
            let entries = pitboss_cli::runs::collect_run_entries(state.runs_dir());
            let entry = entries
                .iter()
                .find(|e| e.run_id == run_id)
                .ok_or(ApiError::NotFound)?;
            let dto: RunDto = entry.into();
            Ok(Json(serde_json::json!({
                "in_progress": true,
                "run": dto,
            }))
            .into_response())
        }
        Err(e) => Err(e.into()),
    }
}

/// `GET /api/runs/:id/manifest` — raw `manifest.snapshot.toml`. Returned
/// as `application/toml`. Used by the frontend for both display and
/// (Phase 5) fork.
pub async fn manifest(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
) -> ApiResult<Response> {
    let run_dir = run_dir(state.runs_dir(), &run_id)?;
    let path = run_dir.join("manifest.snapshot.toml");
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/toml; charset=utf-8")
            .body(Body::from(bytes))
            .expect("toml response")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::NotFound),
        Err(e) => Err(e.into()),
    }
}

/// `GET /api/runs/:id/resolved` — parsed `ResolvedManifest` JSON.
pub async fn resolved(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
) -> ApiResult<Response> {
    let run_dir = run_dir(state.runs_dir(), &run_id)?;
    let path = run_dir.join("resolved.json");
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(json_response(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::NotFound),
        Err(e) => Err(e.into()),
    }
}

/// `GET /api/runs/:id/summary-jsonl` — line-delimited JSON of task records,
/// tail of `summary.jsonl`. Frontend uses this to render the task-by-task
/// timeline before a run is finalized (no `summary.json` yet).
pub async fn summary_jsonl(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
) -> ApiResult<Response> {
    let run_dir = run_dir(state.runs_dir(), &run_id)?;
    let path = run_dir.join("summary.jsonl");
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/x-ndjson")
            .body(Body::from(bytes))
            .expect("ndjson response")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::NotFound),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct LogQuery {
    /// Maximum bytes returned. Default 1 MiB; capped at 8 MiB.
    #[serde(default)]
    pub limit: Option<u64>,
    /// Tail-only mode: return the last `limit` bytes of the log instead
    /// of the first `limit`. Defaults to true (operators usually want
    /// the bottom of the log).
    #[serde(default)]
    pub tail: Option<bool>,
}

/// `GET /api/runs/:run_id/tasks/:task_id` — single `TaskRecord` for one
/// actor (lead, sub-lead, or worker). Reads `summary.jsonl` line by line
/// and returns the first record whose `task_id` matches. `summary.jsonl`
/// is the source of truth — it captures every actor across every layer
/// (root + sub-leads + workers), unlike `summary.json` which is written
/// once at finalize and aggregates only what the dispatcher had visible
/// at that moment (#221).
///
/// 404 when the run dir exists but no record carries the given task_id.
pub async fn task_detail(
    State(state): State<AppState>,
    AxPath((run_id, task_id)): AxPath<(String, String)>,
) -> ApiResult<Response> {
    let run_dir = run_dir(state.runs_dir(), &run_id)?;
    // task_id is rendered into a JSON string match below, not into a
    // path, so the path-segment sanitizer doesn't apply here. We still
    // bound the length to keep the substring scan cheap.
    if task_id.is_empty() || task_id.len() > 256 {
        return Err(ApiError::BadRequest("invalid task_id length".into()));
    }
    let path = run_dir.join("summary.jsonl");
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ApiError::NotFound),
        Err(e) => return Err(e.into()),
    };
    // Lossy decode: a partial-write tail (the dispatcher appends as
    // each actor finishes) shouldn't 500 the request — better to skip
    // a malformed line than to fail a perfectly-readable lookup.
    let text = String::from_utf8_lossy(&bytes);
    let needle = format!("\"task_id\":\"{task_id}\"");
    for line in text.lines() {
        if !line.contains(&needle) {
            continue;
        }
        // Substring match isn't authoritative (the task_id could appear
        // inside another field — a parent_task_id reference, the log
        // path, etc.). Confirm with a real parse.
        let Ok(rec) = serde_json::from_str::<pitboss_core::store::TaskRecord>(line) else {
            continue;
        };
        if rec.task_id == task_id {
            return Ok(Json(rec).into_response());
        }
    }
    Err(ApiError::NotFound)
}

/// `GET /api/runs/:id/tasks/:task_id/log` — task `stdout.log`.
pub async fn task_log(
    State(state): State<AppState>,
    AxPath((run_id, task_id)): AxPath<(String, String)>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Response> {
    let run_dir = run_dir(state.runs_dir(), &run_id)?;
    let task_seg = sanitize_id(&task_id)?;
    let path = run_dir.join("tasks").join(task_seg).join("stdout.log");
    let limit = q.limit.unwrap_or(1 << 20).min(8 << 20) as usize;
    let tail = q.tail.unwrap_or(true);

    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ApiError::NotFound),
        Err(e) => return Err(e.into()),
    };
    let slice = if bytes.len() > limit {
        if tail {
            &bytes[bytes.len() - limit..]
        } else {
            &bytes[..limit]
        }
    } else {
        &bytes[..]
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header("X-Total-Size", bytes.len().to_string())
        .body(Body::from(slice.to_vec()))
        .expect("log response"))
}

// ---- helpers -------------------------------------------------------------

fn json_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .expect("json response")
}

/// Validate a path segment supplied by the URL. Rejects any value that
/// would let the caller escape the run dir (or, for tasks, the run's
/// `tasks/` subdir). The pitboss run-id is a UUIDv7 string in practice;
/// task ids are operator-supplied but constrained by the manifest schema.
/// We accept ASCII-letter/digit/dash/underscore/dot, length 1..=128.
fn sanitize_id(s: &str) -> ApiResult<&str> {
    if s.is_empty() || s.len() > 128 {
        return Err(ApiError::BadRequest("invalid id length".into()));
    }
    if s == "." || s == ".." || s.contains('/') || s.contains('\\') {
        return Err(ApiError::BadRequest("invalid id".into()));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::BadRequest("invalid id chars".into()));
    }
    Ok(s)
}

fn run_dir(base: &Path, run_id: &str) -> ApiResult<PathBuf> {
    let seg = sanitize_id(run_id)?;
    let dir = base.join(seg);
    // Belt-and-suspenders: the dir must canonicalise to a child of `base`.
    // We don't canonicalise (could fail on non-existent paths or symlinks);
    // the segment check above already prevents traversal.
    if !dir.is_dir() {
        return Err(ApiError::NotFound);
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_id("..").is_err());
        assert!(sanitize_id(".").is_err());
        assert!(sanitize_id("a/b").is_err());
        assert!(sanitize_id("a\\b").is_err());
        assert!(sanitize_id("").is_err());
    }

    #[test]
    fn sanitize_accepts_uuid_and_task_ids() {
        assert!(sanitize_id("01950abc-1234-7def-8000-000000000000").is_ok());
        assert!(sanitize_id("worker-1").is_ok());
        assert!(sanitize_id("lead.assistant_2").is_ok());
    }

    #[test]
    fn sanitize_rejects_overlong_ids() {
        let big = "a".repeat(129);
        assert!(sanitize_id(&big).is_err());
    }

    // ── #225: task_detail endpoint ─────────────────────────────────────────

    use crate::state::AppState;
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use tempfile::TempDir;

    /// Lay down a run dir with a `summary.jsonl` containing the given
    /// raw lines. Returns the AppState anchored at that dir's parent.
    fn build_run_with_summary_jsonl(run_id: &str, lines: &[&str]) -> (TempDir, AppState) {
        let tmp = TempDir::new().unwrap();
        let runs_dir = tmp.path().to_path_buf();
        let run_dir = runs_dir.join(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        let mut content = String::new();
        for l in lines {
            content.push_str(l);
            content.push('\n');
        }
        std::fs::write(run_dir.join("summary.jsonl"), content).unwrap();
        let manifests_dir = tmp.path().join("manifests");
        std::fs::create_dir_all(&manifests_dir).unwrap();
        let state = AppState::new(runs_dir, manifests_dir, None);
        (tmp, state)
    }

    /// Minimal valid `TaskRecord` JSON line. `pitboss-core` is allowed
    /// to evolve the shape; we keep the fixture in sync with whatever
    /// `serde_json::to_string(&TaskRecord)` currently emits to avoid
    /// hand-rolled drift.
    fn task_record_line(task_id: &str, status: &str, parent: Option<&str>) -> String {
        use pitboss_core::store::TaskRecord;
        let rec = TaskRecord {
            task_id: task_id.to_string(),
            status: serde_json::from_str(&format!("\"{status}\"")).unwrap(),
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 1000,
            worktree_path: None,
            log_path: PathBuf::from(format!("/tmp/pitboss/{task_id}/stdout.log")),
            token_usage: pitboss_core::parser::TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            final_message: None,
            parent_task_id: parent.map(String::from),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            provider: pitboss_core::provider::Provider::Anthropic,
            model: Some("claude-haiku-4-5".to_string()),
            failure_reason: None,
            cost_usd: None,
        };
        serde_json::to_string(&rec).unwrap()
    }

    async fn body_to_value(resp: Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn task_detail_returns_record_for_matching_id() {
        let run_id = "01950000-0000-7000-8000-000000000001";
        let (_tmp, state) = build_run_with_summary_jsonl(
            run_id,
            &[
                &task_record_line("worker-A", "Success", Some("lead-1")),
                &task_record_line("worker-B", "Failed", Some("lead-1")),
            ],
        );

        let resp = task_detail(
            State(state),
            AxPath((run_id.to_string(), "worker-B".to_string())),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_to_value(resp).await;
        assert_eq!(v["task_id"], "worker-B");
        assert_eq!(v["status"], "Failed");
        assert_eq!(v["parent_task_id"], "lead-1");
    }

    /// Hierarchical-run regression: a sub-lead's record must be findable
    /// via the same endpoint as a worker. summary.jsonl is the source of
    /// truth across all layers (#221).
    #[tokio::test]
    async fn task_detail_finds_sublead_record() {
        let run_id = "01950000-0000-7000-8000-000000000002";
        let (_tmp, state) = build_run_with_summary_jsonl(
            run_id,
            &[
                &task_record_line("worker-A", "Success", Some("sublead-1")),
                &task_record_line("sublead-1", "Success", Some("lead-1")),
                &task_record_line("lead-1", "Success", None),
            ],
        );

        let resp = task_detail(
            State(state),
            AxPath((run_id.to_string(), "sublead-1".to_string())),
        )
        .await
        .unwrap();
        let v = body_to_value(resp).await;
        assert_eq!(v["task_id"], "sublead-1");
        assert_eq!(v["parent_task_id"], "lead-1");
    }

    /// The substring scan (`"task_id":"<X>"`) is only an optimization —
    /// records that mention `<X>` in some other field (e.g.,
    /// `parent_task_id`) must NOT be returned for that lookup.
    #[tokio::test]
    async fn task_detail_does_not_match_parent_task_id_substring() {
        let run_id = "01950000-0000-7000-8000-000000000003";
        let (_tmp, state) = build_run_with_summary_jsonl(
            run_id,
            // worker-X mentions "needle" only as parent_task_id.
            &[&task_record_line("worker-X", "Success", Some("needle"))],
        );

        let resp = task_detail(
            State(state),
            AxPath((run_id.to_string(), "needle".to_string())),
        )
        .await;
        assert!(matches!(resp, Err(ApiError::NotFound)));
    }

    #[tokio::test]
    async fn task_detail_returns_404_when_summary_jsonl_missing() {
        let run_id = "01950000-0000-7000-8000-000000000004";
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(run_id)).unwrap();
        let manifests_dir = tmp.path().join("manifests");
        std::fs::create_dir_all(&manifests_dir).unwrap();
        let state = AppState::new(tmp.path().to_path_buf(), manifests_dir, None);

        let resp = task_detail(
            State(state),
            AxPath((run_id.to_string(), "worker-A".to_string())),
        )
        .await;
        assert!(matches!(resp, Err(ApiError::NotFound)));
    }

    #[tokio::test]
    async fn task_detail_rejects_overlong_task_id() {
        let run_id = "01950000-0000-7000-8000-000000000005";
        let (_tmp, state) = build_run_with_summary_jsonl(run_id, &[]);
        let huge = "a".repeat(257);
        let resp = task_detail(State(state), AxPath((run_id.to_string(), huge))).await;
        assert!(matches!(resp, Err(ApiError::BadRequest(_))));
    }

    /// A run that exists but contains no record for the requested task
    /// returns 404 (vs 200 with empty body or 500). Pre-fix the issue
    /// suggested this case in the spec.
    #[tokio::test]
    async fn task_detail_returns_404_when_task_id_not_in_summary() {
        let run_id = "01950000-0000-7000-8000-000000000006";
        let (_tmp, state) =
            build_run_with_summary_jsonl(run_id, &[&task_record_line("worker-A", "Success", None)]);
        let resp = task_detail(
            State(state),
            AxPath((run_id.to_string(), "worker-Z".to_string())),
        )
        .await;
        assert!(matches!(resp, Err(ApiError::NotFound)));
    }

    /// Malformed JSON lines (e.g., a partial-write tail caught mid-flush)
    /// must not 500 the request — the scan skips and continues.
    #[tokio::test]
    async fn task_detail_skips_malformed_lines() {
        let run_id = "01950000-0000-7000-8000-000000000007";
        let lines = [
            "{\"truncated\":".to_string(), // malformed
            task_record_line("worker-good", "Success", None),
        ];
        let (_tmp, state) = build_run_with_summary_jsonl(
            run_id,
            &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        );

        let resp = task_detail(
            State(state),
            AxPath((run_id.to_string(), "worker-good".to_string())),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
