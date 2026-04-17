//! Dispatcher-side control-socket integration tests. Uses
//! fake-control-client to drive the flow end-to-end.

use pitboss_cli::control::control_socket_path;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn control_socket_path_uses_xdg_or_run_dir() {
    // Ensure the helper at least produces a valid path (regression guard for
    // Phase 1 wiring).
    std::env::remove_var("XDG_RUNTIME_DIR");
    let dir = TempDir::new().unwrap();
    let p = control_socket_path(Uuid::now_v7(), dir.path());
    assert!(p.starts_with(dir.path()));
    assert_eq!(p.file_name().unwrap(), "control.sock");
}
