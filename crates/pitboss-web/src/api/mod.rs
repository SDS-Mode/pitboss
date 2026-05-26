//! Router composition + auth middleware.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

use crate::{assets, state::AppState};

mod audit;
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
        .route("/runs/{run_id}/events-jsonl", get(runs::events_jsonl))
        .route("/runs/{run_id}/audit", get(audit::audit))
        .route("/runs/{run_id}/tasks/{task_id}", get(runs::task_detail))
        .route("/runs/{run_id}/tasks/{task_id}/log", get(runs::task_log))
        .route(
            "/runs/{run_id}/tasks/{task_id}/events",
            get(runs::task_events),
        )
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
        // #608-sibling: redact `?token=...` from the request-span URI so
        // bearer tokens delivered via the query-param fallback (SSE
        // `EventSource`, manifest download links) don't appear verbatim
        // in stderr/tracing logs. `TraceLayer` wraps the outer router
        // — it fires before `require_token` runs — so the URI must be
        // scrubbed at span-creation time. The default `DefaultMakeSpan`
        // records `uri = %request.uri()`, which captures the full query.
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "request",
                    method = %request.method(),
                    uri = %redact_uri_for_tracing(request.uri()),
                    version = ?request.version(),
                )
            }),
        )
        // Defense-in-depth for the same `?token=` surface: `Referrer-Policy:
        // no-referrer` prevents the SSE/manifest-download URLs (and their
        // embedded token) from leaking via the `Referer` header on any
        // cross-origin navigation the page triggers. Applied globally —
        // pitboss-web is an operator tool, not a content site, so we
        // never need to send a Referer.
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
}

/// Format a request URI for inclusion in a tracing span, redacting the
/// `token` query parameter so bearer tokens don't leak into logs.
///
/// The SPA falls back to `?token=<bearer>` on the SSE endpoint (because
/// the browser `EventSource` API can't set Authorization headers) and
/// for manifest-download navigations. Both paths route through
/// `TraceLayer`'s span before `require_token` runs — auth-stripping
/// inside middleware would be too late, the span is already populated.
/// Redacting at span-construction time covers every consumer in one
/// place keyed on the parameter name rather than the route.
fn redact_uri_for_tracing(uri: &axum::http::Uri) -> String {
    let Some(query) = uri.query() else {
        return uri.to_string();
    };
    if !query.contains("token=") {
        return uri.to_string();
    }
    let redacted = query
        .split('&')
        .map(|pair| {
            if pair.starts_with("token=") {
                "token=REDACTED"
            } else {
                pair
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{}", uri.path(), redacted)
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

#[cfg(test)]
mod tests {
    use super::redact_uri_for_tracing;
    use axum::http::Uri;

    #[test]
    fn redacts_token_only_query_param() {
        let uri: Uri = "/api/runs/abc/events?token=supersecret".parse().unwrap();
        assert_eq!(
            redact_uri_for_tracing(&uri),
            "/api/runs/abc/events?token=REDACTED"
        );
    }

    #[test]
    fn redacts_token_when_first_of_multiple_params() {
        let uri: Uri = "/api/manifests/foo?token=secret&format=toml"
            .parse()
            .unwrap();
        assert_eq!(
            redact_uri_for_tracing(&uri),
            "/api/manifests/foo?token=REDACTED&format=toml"
        );
    }

    #[test]
    fn redacts_token_when_not_first_param() {
        let uri: Uri = "/api/manifests/foo?format=toml&token=secret"
            .parse()
            .unwrap();
        assert_eq!(
            redact_uri_for_tracing(&uri),
            "/api/manifests/foo?format=toml&token=REDACTED"
        );
    }

    #[test]
    fn leaves_query_alone_when_no_token() {
        let uri: Uri = "/api/runs?status=running&since=2026-05-01".parse().unwrap();
        assert_eq!(
            redact_uri_for_tracing(&uri),
            "/api/runs?status=running&since=2026-05-01"
        );
    }

    #[test]
    fn leaves_path_only_uris_alone() {
        let uri: Uri = "/api/runs".parse().unwrap();
        assert_eq!(redact_uri_for_tracing(&uri), "/api/runs");
    }

    #[test]
    fn redacts_empty_token_value() {
        // Defensive: a malformed/empty `token=` shouldn't bypass redaction
        // just because the value happens to be empty.
        let uri: Uri = "/api/runs?token=".parse().unwrap();
        assert_eq!(redact_uri_for_tracing(&uri), "/api/runs?token=REDACTED");
    }

    #[test]
    fn does_not_redact_unrelated_param_starting_with_token_substr() {
        // `tokenize` is not `token` — must not be redacted, otherwise an
        // operator's query for a search field gets clobbered. Exact-prefix
        // match on `token=` enforces this.
        let uri: Uri = "/api/insights?tokenize=true&q=foo".parse().unwrap();
        assert_eq!(
            redact_uri_for_tracing(&uri),
            "/api/insights?tokenize=true&q=foo"
        );
    }

    #[test]
    fn redacts_token_with_url_safe_chars_in_value() {
        // Bearer tokens often contain `-`, `_`, `.`, base64 padding etc.
        // Redaction must not depend on the value shape.
        let uri: Uri = "/api/runs/abc/events?token=eyJhbGciOiJIUzI1NiJ9.payload.sig"
            .parse()
            .unwrap();
        assert_eq!(
            redact_uri_for_tracing(&uri),
            "/api/runs/abc/events?token=REDACTED"
        );
    }
}
