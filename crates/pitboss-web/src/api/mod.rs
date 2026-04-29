//! Router composition + auth middleware.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use serde::Deserialize;

use crate::{assets, state::AppState};

mod control;
mod events;
mod insights;
mod manifests;
mod runs;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/runs", get(runs::list).post(manifests::dispatch))
        .route("/runs/{run_id}", get(runs::detail))
        .route("/runs/{run_id}/manifest", get(runs::manifest))
        .route("/runs/{run_id}/resolved", get(runs::resolved))
        .route("/runs/{run_id}/summary-jsonl", get(runs::summary_jsonl))
        .route("/runs/{run_id}/tasks/{task_id}", get(runs::task_detail))
        .route("/runs/{run_id}/tasks/{task_id}/log", get(runs::task_log))
        .route("/runs/{run_id}/events", get(events::events))
        .route("/runs/{run_id}/control", post(control::send))
        .route("/runs/{run_id}/fork", post(manifests::fork_run))
        .route("/schema", get(manifests::schema))
        .route("/manifests", get(manifests::list).post(manifests::save))
        .route("/manifests/validate", post(manifests::validate))
        .route("/manifests/{name}", get(manifests::read_one))
        .route("/insights/runs", get(insights::runs))
        .route("/insights/failures", get(insights::failures))
        .route("/insights/clusters", get(insights::clusters))
        .route("/insights/manifests", get(insights::manifests))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .nest("/api", api)
        .fallback(assets::handler)
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// Bearer-token auth. When `state.token()` is None, all requests pass.
///
/// Accepted credentials, in order:
/// - `Authorization: Bearer <token>` header — preferred for fetch-based
///   API calls (the SPA's `request()` helper sets this).
/// - `?token=<token>` query parameter — fallback for endpoints whose
///   client cannot set headers, namely the SSE `events` route consumed
///   by the browser's `EventSource` (which has no header API). Lower
///   security profile (token can leak via referer/log/history) so the
///   SPA only sends it on SSE; everything else uses the header.
async fn require_token(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected) = state.token() else {
        return Ok(next.run(request).await);
    };
    let from_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if from_header == Some(expected) {
        return Ok(next.run(request).await);
    }
    if q.token.as_deref() == Some(expected) {
        return Ok(next.run(request).await);
    }
    Err(StatusCode::UNAUTHORIZED)
}
