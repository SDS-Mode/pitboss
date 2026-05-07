//! Verifies `POST /api/manifests/validate` returns the capability
//! matrix in its JSON response when validation succeeds (#391 slice 3).
//!
//! Same `#[path]`-mount pattern as `manifests_download_integration.rs` —
//! the full `api::router` drags sibling modules that don't resolve
//! from a tests file, so we mount only what the validate handler
//! needs.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use tower::ServiceExt;

#[allow(dead_code, unused_imports)]
#[path = "../src/control_bridge.rs"]
mod control_bridge;
#[allow(dead_code, unused_imports)]
#[path = "../src/error.rs"]
mod error;
#[allow(dead_code, unused_imports)]
#[path = "../src/insights/mod.rs"]
mod insights;
#[allow(dead_code, unused_imports)]
#[path = "../src/api/manifests.rs"]
mod manifests;
#[allow(dead_code, unused_imports)]
#[path = "../src/state.rs"]
mod state;

use state::AppState;

fn fixture_router() -> Router {
    let runs = tempfile::tempdir().unwrap().keep();
    let mans = tempfile::tempdir().unwrap().keep();
    let st = AppState::new(runs, mans, None);
    Router::new()
        .route("/api/manifests/validate", post(manifests::validate))
        .with_state(st)
}

async fn body_to_json(res: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(res.into_body(), 256 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).expect("body is valid JSON")
}

const TYPED_MANIFEST: &str = r#"
[run]
name = "typed-fixture"

[lead]
id = "lead"
directory = "/tmp"
prompt = "drive"
model = "claude-haiku-4-5"

[[mcp_server]]
id = "pitboss"
command = "/bin/true"

[[mcp_server]]
id = "fs-writer"
command = "/bin/true"
scope = "type:writer"

[[worker_type]]
id = "writer"
tools = ["Read", "Write"]

[[worker_type]]
id = "reader"
tools = ["Read"]
"#;

/// On a clean validate, the response includes a `capability_matrix`
/// field with one row per declared profile (plus the untyped row).
/// The SPA renders this as a table on the manifest detail page; if
/// the field disappeared from the JSON shape, the table would
/// silently render as empty.
#[tokio::test]
async fn validate_returns_capability_matrix_on_success() {
    let app = fixture_router();
    let payload = serde_json::json!({ "contents": TYPED_MANIFEST }).to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/manifests/validate")
                .header("content-type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = body_to_json(res).await;
    assert_eq!(body["ok"], true, "validate should succeed: {body:?}");

    let matrix = body["capability_matrix"]
        .as_array()
        .expect("capability_matrix must be present and an array");
    // Untyped + 2 worker_type rows = 3.
    assert_eq!(
        matrix.len(),
        3,
        "expected untyped + 2 worker rows, got: {matrix:?}"
    );

    // Untyped row anchors first; only sees the unscoped server.
    assert_eq!(matrix[0]["kind"], "untyped");
    assert!(matrix[0]["actor_type"].is_null());
    let untyped_servers: Vec<&str> = matrix[0]["server_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(untyped_servers, vec!["pitboss"]);

    // Writer row sees the writer-scoped server.
    let writer = matrix
        .iter()
        .find(|r| r["actor_type"] == "writer")
        .expect("writer row missing");
    assert_eq!(writer["kind"], "worker_type");
    let writer_servers: Vec<&str> = writer["server_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        writer_servers.contains(&"pitboss") && writer_servers.contains(&"fs-writer"),
        "writer should see both servers, got: {writer_servers:?}"
    );
}

/// Validation failure must omit the `capability_matrix` key entirely
/// (the resolved manifest doesn't exist, so there's nothing to compute
/// against). Pin absence-of-key, not just JSON-null — the
/// `#[serde(skip_serializing_if = "Option::is_none")]` attribute on
/// the Rust side is the contract, and a future refactor that drops
/// the attribute would silently flip wire shape from "key absent" to
/// "key present, value null" without breaking current TS consumers
/// (both are falsy). This test catches that drift.
#[tokio::test]
async fn validate_omits_capability_matrix_on_failure() {
    let app = fixture_router();
    let payload = serde_json::json!({ "contents": "this is not toml :::" }).to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/manifests/validate")
                .header("content-type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = body_to_json(res).await;
    assert_eq!(body["ok"], false);
    let obj = body.as_object().expect("response is a JSON object");
    assert!(
        !obj.contains_key("capability_matrix"),
        "capability_matrix key must be omitted entirely on failure, got: {body:?}"
    );
}
