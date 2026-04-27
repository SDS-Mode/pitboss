//! Verifies the `?download=1` flag on `GET /api/manifests/:name`
//! emits a `Content-Disposition: attachment` header so browsers save
//! the response instead of rendering it.
//!
//! The full `api::router` declares sibling modules that don't resolve
//! when mounted via `#[path]` from a tests file, so we mount only the
//! handler we need against a fixture `AppState` and exercise it via
//! `tower::ServiceExt::oneshot`.

use std::path::PathBuf;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use tower::ServiceExt;

// Mount only the modules the manifests handler depends on. `#[path]`
// drags whole module trees into the test crate, so silence dead-code
// warnings rather than fragmenting the production module just for the
// test-side compile unit.
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

fn fixture_router(manifests_dir: PathBuf, runs_dir: PathBuf) -> Router {
    let st = AppState::new(runs_dir, manifests_dir, None);
    Router::new()
        .route("/api/manifests/{name}", get(manifests::read_one))
        .with_state(st)
}

#[tokio::test]
async fn download_flag_adds_content_disposition() {
    let manifests = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    std::fs::write(
        manifests.path().join("smoke.toml"),
        b"[run]\nname = \"smoke\"\n",
    )
    .unwrap();

    let app = fixture_router(manifests.path().to_path_buf(), runs.path().to_path_buf());

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/manifests/smoke?download=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cd = res
        .headers()
        .get("content-disposition")
        .expect("Content-Disposition must be set when download=1")
        .to_str()
        .unwrap();
    assert!(cd.contains("attachment"), "got: {cd}");
    assert!(cd.contains("smoke.toml"), "got: {cd}");

    // Without the flag — no attachment header (existing inline-render behaviour).
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/manifests/smoke")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers().get("content-disposition").is_none(),
        "header must NOT be set without download=1"
    );
}
