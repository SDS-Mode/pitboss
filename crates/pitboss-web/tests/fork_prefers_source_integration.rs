//! Verifies that `POST /api/runs/:id/fork` prefers `manifest.source.toml`
//! over `manifest.snapshot.toml` when both are present (the
//! container-dispatch case), and falls back to `manifest.snapshot.toml`
//! when only the snapshot exists (pre-fix runs and flat-mode runs).

use std::path::PathBuf;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use serde_json::json;
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

const SOURCE_BYTES: &[u8] = b"[container]\nimage = \"src\"\n";
const SNAPSHOT_BYTES: &[u8] = b"# stripped snapshot\n";

fn fixture_router(manifests_dir: PathBuf, runs_dir: PathBuf) -> Router {
    let st = AppState::new(runs_dir, manifests_dir, None);
    Router::new()
        .route("/api/runs/{run_id}/fork", post(manifests::fork_run))
        .with_state(st)
}

async fn post_fork(app: Router, run_id: &str, new_name: &str) -> (StatusCode, Vec<u8>) {
    let body = serde_json::to_vec(&json!({ "new_name": new_name })).unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/runs/{run_id}/fork"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

#[tokio::test]
async fn fork_prefers_source_manifest_when_both_present() {
    let manifests = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let run_id = uuid::Uuid::now_v7().to_string();
    let run_subdir = runs.path().join(&run_id);
    std::fs::create_dir_all(&run_subdir).unwrap();
    std::fs::write(run_subdir.join("manifest.source.toml"), SOURCE_BYTES).unwrap();
    std::fs::write(run_subdir.join("manifest.snapshot.toml"), SNAPSHOT_BYTES).unwrap();

    let app = fixture_router(manifests.path().to_path_buf(), runs.path().to_path_buf());
    let (status, _body) = post_fork(app, &run_id, "forked").await;
    assert_eq!(status, StatusCode::OK, "fork should succeed");
    let workspace = manifests.path().join("forked.toml");
    let bytes = std::fs::read(&workspace).expect("fork must write workspace file");
    assert_eq!(
        bytes, SOURCE_BYTES,
        "fork must prefer manifest.source.toml when both exist (got snapshot bytes back)"
    );
}

#[tokio::test]
async fn fork_falls_back_to_snapshot_when_no_source_present() {
    // Pre-fix runs (and flat-mode runs) only have manifest.snapshot.toml.
    // Fork must continue working against them — the artifact preference
    // is opportunistic, not load-bearing.
    let manifests = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let run_id = uuid::Uuid::now_v7().to_string();
    let run_subdir = runs.path().join(&run_id);
    std::fs::create_dir_all(&run_subdir).unwrap();
    std::fs::write(run_subdir.join("manifest.snapshot.toml"), SNAPSHOT_BYTES).unwrap();

    let app = fixture_router(manifests.path().to_path_buf(), runs.path().to_path_buf());
    let (status, _body) = post_fork(app, &run_id, "forked-flat").await;
    assert_eq!(status, StatusCode::OK);
    let workspace = manifests.path().join("forked-flat.toml");
    let bytes = std::fs::read(&workspace).expect("fork must write workspace file");
    assert_eq!(bytes, SNAPSHOT_BYTES);
}

#[tokio::test]
async fn fork_returns_404_when_neither_artifact_present() {
    let manifests = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let run_id = uuid::Uuid::now_v7().to_string();
    let run_subdir = runs.path().join(&run_id);
    std::fs::create_dir_all(&run_subdir).unwrap();

    let app = fixture_router(manifests.path().to_path_buf(), runs.path().to_path_buf());
    let (status, _body) = post_fork(app, &run_id, "missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
