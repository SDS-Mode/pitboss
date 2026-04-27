//! `POST /api/runs/:id/control` — send a `ControlOp` to a live run's
//! dispatcher via the per-run control bridge.
//!
//! The request body is a JSON-tagged `ControlOp` (matching the wire
//! format on the control socket — `{"op":"cancel_worker","task_id":...}`).
//! The dispatcher's `OpAcked` / `OpFailed` reply lands on the SSE event
//! stream; this endpoint only confirms that the bytes reached the
//! socket.
//!
//! Responses:
//! - `202 Accepted` — op was written and flushed to the dispatcher.
//! - `400 Bad Request` — body failed to parse, run id is invalid, or
//!   the op is rejected up front (e.g. `Hello`, which is server-only).
//! - `404 Not Found` — no `control.sock` for this run, or the dispatcher
//!   exited and the socket no longer accepts connections.

use axum::{
    extract::{Path as AxPath, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use pitboss_cli::control::protocol::ControlOp;

use crate::{
    control_bridge::BridgeError,
    error::{ApiError, ApiResult},
    state::AppState,
};

pub async fn send(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
    Json(op): Json<ControlOp>,
) -> ApiResult<impl IntoResponse> {
    sanitize_run_id(&run_id)?;

    state
        .bridge()
        .send_op(&run_id, &op)
        .await
        .map_err(map_bridge_err)?;

    Ok(StatusCode::ACCEPTED)
}

fn sanitize_run_id(run_id: &str) -> ApiResult<()> {
    if run_id.is_empty()
        || run_id.len() > 128
        || run_id == ".."
        || run_id == "."
        || run_id.contains('/')
        || run_id.contains('\\')
    {
        return Err(ApiError::BadRequest("invalid run id".into()));
    }
    if !run_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::BadRequest("invalid run id chars".into()));
    }
    Ok(())
}

fn map_bridge_err(e: BridgeError) -> ApiError {
    match e {
        BridgeError::NotFound | BridgeError::Dead => ApiError::NotFound,
        BridgeError::Io(io) => ApiError::Io(io),
        BridgeError::Handshake(msg) => ApiError::Io(std::io::Error::other(msg)),
        BridgeError::Rejected(msg) => ApiError::BadRequest(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_run_id("..").is_err());
        assert!(sanitize_run_id("a/b").is_err());
        assert!(sanitize_run_id("a\\b").is_err());
        assert!(sanitize_run_id("").is_err());
        assert!(sanitize_run_id(&"a".repeat(200)).is_err());
        assert!(sanitize_run_id("bad chars!").is_err());
    }

    #[test]
    fn sanitize_accepts_uuid() {
        assert!(sanitize_run_id("01950abc-1234-7def-8000-000000000000").is_ok());
    }
}
