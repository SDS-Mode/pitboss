//! `pitboss validate --container` (#255) — integration tests for the
//! flag that lets operators pre-flight a container-mode manifest
//! without the host-side directory check failing on the in-container
//! mount paths.

mod support;

use std::process::Command;
use support::*;
use tempfile::TempDir;

fn ensure_built() {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "pitboss-cli"])
        .status()
        .unwrap();
    assert!(status.success(), "build failed");
}

/// `--container` against a container-mode manifest whose `[lead].directory`
/// points at an in-container path that doesn't exist on the host.
/// Without the flag this fails (which is what the issue reproed); with
/// the flag it succeeds because the dir-existence check is skipped.
#[test]
fn validate_with_container_flag_skips_host_dir_check() {
    ensure_built();
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("container.toml");
    std::fs::write(
        &manifest_path,
        r#"
[run]
name = "container-fixture"

[container]
image = "ghcr.io/example/img:latest"

[lead]
id = "lead"
directory = "/workspace"   # container-side path; doesn't exist on host
prompt = "p"
model = "claude-haiku-4-5"
use_worktree = false
max_workers = 1
budget_usd = 0.50
lead_timeout_secs = 60
"#,
    )
    .unwrap();

    let out = Command::new(pitboss_binary())
        .arg("validate")
        .arg("--container")
        .arg(&manifest_path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "validate --container should succeed; stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("OK (hierarchical)"),
        "expected hierarchical OK line; got stdout={stdout}"
    );
}

/// Without `--container`, the same manifest must STILL fail (existing
/// behavior preserved) AND the error must point at the new flag so the
/// operator knows the right escape hatch. The hint is the user-visible
/// half of #255 — without it, operators reach for `container-dispatch
/// --dry-run` (which works but is not what they typed first).
#[test]
fn validate_without_flag_on_container_manifest_emits_hint() {
    ensure_built();
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("container.toml");
    std::fs::write(
        &manifest_path,
        r#"
[run]
name = "container-fixture"

[container]
image = "ghcr.io/example/img:latest"

[lead]
id = "lead"
directory = "/workspace"
prompt = "p"
model = "claude-haiku-4-5"
use_worktree = false
max_workers = 1
budget_usd = 0.50
lead_timeout_secs = 60
"#,
    )
    .unwrap();

    let out = Command::new(pitboss_binary())
        .arg("validate")
        .arg(&manifest_path)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "should still fail without --container"
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "validation failure exit code is 2 (#157) so CI can hard-gate"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("directory does not exist"),
        "underlying error must still surface; got stderr={stderr}"
    );
    assert!(
        stderr.contains("--container"),
        "stderr must point operator at --container; got: {stderr}"
    );
}

/// `--container` on a manifest that has NO `[container]` block: the
/// manifest is otherwise valid, validate succeeds, and we emit a
/// stderr advisory so the operator notices the typoed/redundant flag.
/// Pin both the success exit code AND the advisory text so a future
/// refactor that drops the advisory gets caught.
#[test]
fn validate_with_container_flag_on_non_container_manifest_emits_advisory() {
    ensure_built();
    let dir = TempDir::new().unwrap();
    init_git_repo(dir.path());
    let manifest_path = dir.path().join("flat.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[run]
max_parallel_tasks = 1

[defaults]
use_worktree = false

[[task]]
id = "t1"
directory = "{}"
prompt = "p"
"#,
            dir.path().display()
        ),
    )
    .unwrap();

    let out = Command::new(pitboss_binary())
        .arg("validate")
        .arg("--container")
        .arg(&manifest_path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("manifest has no [container] block"),
        "expected no-container advisory on stderr; got: {stderr}"
    );
}

/// `--skip-dir-check` is registered as an alias of `--container` so
/// operators who type the more literal name (matching the internal
/// `validate_skip_dir_check` helper) get the same behavior. Pin the
/// alias so a future clap rewrite that drops it gets caught.
#[test]
fn skip_dir_check_alias_resolves_to_container_flag() {
    ensure_built();
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("container.toml");
    std::fs::write(
        &manifest_path,
        r#"
[run]
name = "container-fixture"

[container]
image = "ghcr.io/example/img:latest"

[lead]
id = "lead"
directory = "/workspace"
prompt = "p"
model = "claude-haiku-4-5"
use_worktree = false
max_workers = 1
budget_usd = 0.50
lead_timeout_secs = 60
"#,
    )
    .unwrap();

    let out = Command::new(pitboss_binary())
        .arg("validate")
        .arg("--skip-dir-check")
        .arg(&manifest_path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--skip-dir-check should resolve to --container behavior; \
         stdout={stdout} stderr={stderr}"
    );
}
