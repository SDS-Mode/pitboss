//! Verifies `/api/runs` auto-routes to `pitboss container-dispatch`
//! when the saved manifest declares a `[container]` section, and to
//! `pitboss dispatch` otherwise.
//!
//! We point `PITBOSS_BIN` at a fake-pitboss shell script that dumps its
//! argv into a per-call log file and exits non-zero, so the endpoint
//! surfaces the script's argv through `ApiError::BadRequest` (we don't
//! depend on stderr parsing — we read the log directly).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use serde_json::json;
use tower::ServiceExt;

// `PITBOSS_BIN` is process-global; cargo's default test threading would
// race two tests on the same env var. Serialize through a mutex held
// for the full test body (env set → handler spawn → log read → env
// unset). One mutex per file is plenty.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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

static FAKE_BIN_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Lay down a one-shot shell script that records its argv to a log file
/// and exits 1. Returns `(bin_path, log_path)`.
fn lay_fake_pitboss() -> (PathBuf, PathBuf) {
    let n = FAKE_BIN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("pitboss-routing-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("argv.log");
    let bin = dir.join("fake-pitboss");
    // Record every argv entry on its own line; exit 1 so the handler
    // returns BadRequest (whose body we don't actually need).
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {log:?}; done\nexit 1\n",
        log = log.display()
    );
    std::fs::write(&bin, script).unwrap();
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();
    (bin, log)
}

fn fixture_router(manifests_dir: PathBuf, runs_dir: PathBuf) -> Router {
    let st = AppState::new(runs_dir, manifests_dir, None);
    Router::new()
        .route("/api/runs", post(manifests::dispatch))
        .with_state(st)
}

async fn post_dispatch(app: Router, manifest_name: &str) -> StatusCode {
    let body = serde_json::to_vec(&json!({ "manifest_name": manifest_name })).unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/runs")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    res.status()
}

// `await_holding_lock`: deliberate. The env mutation (`PITBOSS_BIN`)
// must remain in place across the spawn-and-await, and the
// current_thread runtime means there's no second task that could
// contend for the std::sync::Mutex.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn dispatch_routes_container_manifest_to_container_dispatch() {
    let _guard = ENV_LOCK.lock().unwrap();
    let manifests = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let (bin, log) = lay_fake_pitboss();

    std::fs::write(
        manifests.path().join("with-container.toml"),
        b"[container]\nimage = \"x\"\n\n[[container.mount]]\nhost = \"/h\"\ncontainer = \"/c\"\n\n[[task]]\nid = \"t\"\ndirectory = \"/c\"\nprompt = \"p\"\n",
    )
    .unwrap();

    // SAFETY: ENV_LOCK serializes env mutation across tests in this file.
    unsafe { std::env::set_var("PITBOSS_BIN", &bin) };

    let app = fixture_router(manifests.path().to_path_buf(), runs.path().to_path_buf());
    let status = post_dispatch(app, "with-container").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "fake-pitboss exits 1 so the handler surfaces BadRequest"
    );
    let argv = std::fs::read_to_string(&log).expect("fake-pitboss must have run and logged argv");
    let first_arg = argv.lines().next().unwrap_or("");
    assert_eq!(
        first_arg, "container-dispatch",
        "container manifest must route to container-dispatch (full argv: {argv})"
    );

    unsafe { std::env::remove_var("PITBOSS_BIN") };
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "current_thread")]
async fn dispatch_routes_flat_manifest_to_plain_dispatch() {
    let _guard = ENV_LOCK.lock().unwrap();
    let manifests = tempfile::tempdir().unwrap();
    let runs = tempfile::tempdir().unwrap();
    let (bin, log) = lay_fake_pitboss();

    std::fs::write(
        manifests.path().join("flat.toml"),
        b"[[task]]\nid = \"t\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n",
    )
    .unwrap();

    // SAFETY: ENV_LOCK serializes env mutation across tests in this file.
    unsafe { std::env::set_var("PITBOSS_BIN", &bin) };

    let app = fixture_router(manifests.path().to_path_buf(), runs.path().to_path_buf());
    let status = post_dispatch(app, "flat").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let argv = std::fs::read_to_string(&log).expect("fake-pitboss must have run and logged argv");
    let first_arg = argv.lines().next().unwrap_or("");
    assert_eq!(
        first_arg, "dispatch",
        "manifest without [container] must route to flat dispatch (full argv: {argv})"
    );

    unsafe { std::env::remove_var("PITBOSS_BIN") };
}
