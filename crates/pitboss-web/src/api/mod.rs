//! Router composition + auth middleware.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};

use crate::{assets, state::AppState};

mod runs;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/runs", get(runs::list))
        .route("/runs/{run_id}", get(runs::detail))
        .route("/runs/{run_id}/manifest", get(runs::manifest))
        .route("/runs/{run_id}/resolved", get(runs::resolved))
        .route("/runs/{run_id}/summary-jsonl", get(runs::summary_jsonl))
        .route("/runs/{run_id}/tasks/{task_id}/log", get(runs::task_log))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), require_token));

    Router::new()
        .nest("/api", api)
        .fallback(assets::handler)
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

/// Bearer-token auth. When `state.token()` is None, all requests pass.
async fn require_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected) = state.token() else {
        return Ok(next.run(request).await);
    };
    let supplied = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if supplied == Some(expected) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
