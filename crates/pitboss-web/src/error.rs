//! Common error type for API handlers. Renders as JSON
//! `{"error": "...", "detail": "..."}` with an appropriate status code.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[allow(dead_code)] // Wired up by Phase 4 manifest sandbox enforcement.
    #[error("forbidden")]
    Forbidden,

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, detail) = match &self {
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", String::new()),
            Self::BadRequest(d) => (StatusCode::BAD_REQUEST, "bad_request", d.clone()),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", String::new()),
            Self::Io(e) => (StatusCode::INTERNAL_SERVER_ERROR, "io_error", e.to_string()),
            Self::Serde(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "serde_error",
                e.to_string(),
            ),
        };
        let body = Json(json!({
            "error": error,
            "detail": detail,
        }));
        (status, body).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
