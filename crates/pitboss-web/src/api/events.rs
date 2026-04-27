//! SSE endpoint that bridges per-run control events to the browser.
//!
//! `GET /api/runs/:id/events` — opens an `text/event-stream` connection,
//! subscribes to the per-run broadcast channel, and forwards each
//! `EventEnvelope` as one SSE `data:` line of JSON. Lagged subscribers
//! receive a typed `lagged` event so the client can resync (refetch
//! state) before resuming.
//!
//! Errors before the stream starts (run not found, dispatcher dead) are
//! returned as standard HTTP statuses via `ApiError`. Errors mid-stream
//! close the connection — the EventSource auto-reconnect default kicks
//! in client-side.

use std::convert::Infallible;

use axum::{
    extract::{Path as AxPath, State},
    response::sse::{Event, KeepAlive, Sse},
};
use futures_util::stream::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;

use crate::{
    control_bridge::BridgeError,
    error::{ApiError, ApiResult},
    state::AppState,
};

pub async fn events(
    State(state): State<AppState>,
    AxPath(run_id): AxPath<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    // Defensive sanitisation. The `runs.rs` helpers enforce the same
    // constraints; this guard exists because `events` doesn't call
    // through them — we want a single audit point per endpoint, not
    // a shared trust boundary.
    if run_id.is_empty()
        || run_id.len() > 128
        || run_id == ".."
        || run_id == "."
        || run_id.contains('/')
        || run_id.contains('\\')
    {
        return Err(ApiError::BadRequest("invalid run id".into()));
    }

    let rx = state
        .bridge()
        .subscribe(&run_id)
        .await
        .map_err(|e| match e {
            BridgeError::NotFound | BridgeError::Dead => ApiError::NotFound,
            BridgeError::Io(io) => ApiError::Io(io),
            BridgeError::Handshake(msg) => {
                ApiError::Io(std::io::Error::other(format!("handshake: {msg}")))
            }
        })?;

    let stream = BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(envelope) => {
                let json = serde_json::to_string(&envelope).ok()?;
                Some(Ok(Event::default().event("control").data(json)))
            }
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                Some(Ok(Event::default()
                    .event("lagged")
                    .data(skipped.to_string())))
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
