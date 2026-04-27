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
}
