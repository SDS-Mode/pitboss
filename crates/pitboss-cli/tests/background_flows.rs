//! End-to-end tests for `pitboss dispatch --background` (issue #133-C).
//!
//! These tests drive `pitboss dispatch --background` as a real subprocess
//! and verify the detach contract:
//!   1. Parent returns within ~1s with exit 0.
//!   2. Stdout contains a single JSON line `{run_id, manifest_path,
//!      started_at, child_pid}`.
//!   3. The child runs to completion in the background, writing
//!      `summary.json` with the *same* `run_id` the parent announced.
//!   4. The hidden `--internal-run-id` flag is honored (foreground
//!      dispatch with that flag uses the supplied id).
//!
//! Fake-claude scripts keep these tests deterministic and fast (no
//! Anthropic API calls).

mod support;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::*;
use tempfile::TempDir;

/// The CI-friendly upper bound on how long the parent should take to
/// return. Spawning a child + writing one JSON line should be well under
/// 500ms in practice; 5s gives plenty of slack on a loaded test runner.
const PARENT_RETURN_BUDGET: Duration = Duration::from_secs(5);

/// Wait for `summary.json` to land at the expected path. Background
/// dispatch finishes asynchronously, so tests poll instead of blocking
/// on the spawn handle (which we deliberately dropped in the parent).
fn wait_for_summary(path: &std::path::Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn background_returns_immediately_with_run_id_and_completes_async() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[run]
max_parallel_tasks = 1
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"
"#,
            run_dir = run_dir.path().display(),
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let start = Instant::now();
    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .env("PITBOSS_CLAUDE_BINARY", fake_claude_path())
        .env("PITBOSS_FAKE_SCRIPT", fixture("success.jsonl"))
        .env("PITBOSS_FAKE_EXIT_CODE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().expect("spawn pitboss --background");
    let parent_elapsed = start.elapsed();

    assert!(
        out.status.success(),
        "parent should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        parent_elapsed < PARENT_RETURN_BUDGET,
        "parent must return within {}s; took {}ms",
        PARENT_RETURN_BUDGET.as_secs(),
        parent_elapsed.as_millis()
    );

    // Parse the JSON announcement on stdout.
    let stdout = String::from_utf8(out.stdout).unwrap();
    let announce: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not single JSON line: {stdout:?} ({e})"));
    let announced_run_id = announce["run_id"]
        .as_str()
        .expect("run_id field present")
        .to_string();
    assert!(
        uuid::Uuid::parse_str(&announced_run_id).is_ok(),
        "run_id is a valid UUID: {announced_run_id}"
    );
    assert_eq!(
        announce["manifest_path"].as_str().unwrap(),
        manifest_path.to_string_lossy()
    );
    assert!(announce["started_at"].as_str().is_some());
    assert!(announce["child_pid"].as_u64().is_some());

    // Wait for the detached child to land summary.json. Use the announced
    // run_id to predict the directory name — if these match, the
    // pre-mint → child handoff is working end-to-end.
    let summary = run_dir.path().join(&announced_run_id).join("summary.json");
    assert!(
        wait_for_summary(&summary, Duration::from_secs(30)),
        "background child never wrote summary.json at {} (run_dir contents: {:?})",
        summary.display(),
        std::fs::read_dir(run_dir.path())
            .ok()
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );

    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    // run_id in summary.json must match what the parent announced — that's
    // the whole point of the pre-mint mechanism.
    assert_eq!(
        s["run_id"].as_str().unwrap(),
        announced_run_id,
        "summary.json run_id must match parent's announcement"
    );
    assert_eq!(s["tasks_total"].as_u64().unwrap(), 1);
}

#[test]
fn internal_run_id_flag_is_honored_in_foreground_dispatch() {
    // Sanity check that --internal-run-id (the hidden mechanism --background
    // uses to align parent's announced id with child's on-disk id) actually
    // wires through to the dispatcher when invoked directly. If this breaks,
    // --background's correlation contract silently breaks too.
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[run]
max_parallel_tasks = 1
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"
"#,
            run_dir = run_dir.path().display(),
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let preset_run_id = uuid::Uuid::now_v7().to_string();

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch")
        .arg(&manifest_path)
        .args(["--internal-run-id", &preset_run_id])
        .env("PITBOSS_CLAUDE_BINARY", fake_claude_path())
        .env("PITBOSS_FAKE_SCRIPT", fixture("success.jsonl"))
        .env("PITBOSS_FAKE_EXIT_CODE", "0");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let summary = run_dir.path().join(&preset_run_id).join("summary.json");
    assert!(
        summary.exists(),
        "summary.json must land at the supplied run id, not a fresh one. \
         Looked at {}; run_dir contents: {:?}",
        summary.display(),
        std::fs::read_dir(run_dir.path())
            .ok()
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    assert_eq!(s["run_id"].as_str().unwrap(), preset_run_id);
}

#[test]
fn background_with_dry_run_is_rejected() {
    // --background spawns a detached child to do real work; --dry-run only
    // prints what would run. Combining them is meaningless, so the CLI
    // should reject explicitly rather than silently picking one.
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"
"#,
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .arg("--dry-run")
        .arg("--run-dir")
        .arg(run_dir.path());
    let out = cmd.output().unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--background") && stderr.contains("--dry-run"),
        "stderr should explain the conflict; got: {stderr}"
    );
}

#[test]
fn background_with_internal_run_id_is_rejected() {
    // --internal-run-id is the parent→child handoff token; combining it
    // with --background means the operator is asking us to re-detach an
    // already-detached invocation. Reject explicitly.
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"
"#,
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .args(["--internal-run-id", &uuid::Uuid::now_v7().to_string()])
        .arg("--run-dir")
        .arg(run_dir.path());
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--background") && stderr.contains("--internal-run-id"),
        "stderr should explain the conflict; got: {stderr}"
    );
}
