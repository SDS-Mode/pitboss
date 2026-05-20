//! Integration tests for `pitboss container-dispatch --background`.
//!
//! These exercise the parent's contract (validate, pre-mint a UUID v7,
//! write `manifest.source.toml`, print a JSON announcement carrying
//! `container: true`, exit 0) without requiring docker or podman to be
//! present on the test host. The detached child will fail in
//! `detect_runtime` when no runtime is installed — that's fine, because
//! `manifest.source.toml` is written by the parent before the child is
//! spawned. The Fork-manifest contract depends only on the parent's
//! artifact write, not on the child reaching exec().

mod support;

use std::process::{Command, Stdio};
use support::*;
use tempfile::TempDir;

#[test]
fn background_writes_source_manifest_and_announces_run_id() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_text = format!(
        r#"
[run]
max_parallel_tasks = 1
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[container]
image = "ghcr.io/example/pitboss:latest"

[[container.mount]]
host = "{repo}"
container = "/project"

[[task]]
id = "t1"
directory = "/project"
prompt = "p"
"#,
        run_dir = run_dir.path().display(),
        repo = repo.path().display()
    );
    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(&manifest_path, &manifest_text).unwrap();

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("container-dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .arg("--run-dir")
        .arg(run_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd
        .output()
        .expect("spawn pitboss container-dispatch --background");

    assert!(
        out.status.success(),
        "parent should exit 0; stdout={:?} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    let announce: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not single JSON line: {stdout:?} ({e})"));
    let announced_run_id = announce["run_id"]
        .as_str()
        .expect("run_id present")
        .to_string();
    let parsed = uuid::Uuid::parse_str(&announced_run_id).expect("valid UUID");
    assert_eq!(parsed.get_version_num(), 7, "run_id must be UUID v7");
    assert_eq!(
        announce["container"].as_bool(),
        Some(true),
        "announcement must carry container: true so /api/runs callers can disambiguate"
    );
    assert!(announce["manifest_path"].as_str().is_some());
    assert!(announce["started_at"].as_str().is_some());
    assert!(announce["child_pid"].as_u64().is_some());

    // manifest.source.toml must be on disk by the time the parent exits.
    // This is the central correctness claim: the Fork-manifest endpoint
    // can rely on the source artifact even if the detached child later
    // fails to find docker/podman.
    let source = run_dir
        .path()
        .join(&announced_run_id)
        .join("manifest.source.toml");
    assert!(
        source.is_file(),
        "manifest.source.toml must land at {} (run_dir contents: {:?})",
        source.display(),
        std::fs::read_dir(run_dir.path())
            .ok()
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    let on_disk = std::fs::read_to_string(&source).unwrap();
    assert_eq!(
        on_disk, manifest_text,
        "source manifest bytes must match the operator-typed input verbatim"
    );
}

#[test]
fn background_rejects_manifest_without_container_section() {
    // The pre-flight inside `run_container_background` mirrors
    // `pitboss container-dispatch` foreground: a manifest with no
    // `[container]` section is rejected before any spawn or announce.
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[defaults]
use_worktree = false

[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"
"#,
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let out = Command::new(pitboss_binary())
        .arg("container-dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .output()
        .unwrap();
    assert!(!out.status.success(), "should fail without [container]");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[container]"),
        "stderr must mention the missing section: {stderr}"
    );
}

#[test]
fn background_rejects_dry_run_combination() {
    // --background and --dry-run are clap-mutually-exclusive at the
    // ContainerDispatch arg level. Combining them must fail before the
    // dispatcher body runs.
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());

    let manifest_path = repo.path().join("pitboss.toml");
    std::fs::write(&manifest_path, "[container]\n").unwrap();

    let out = Command::new(pitboss_binary())
        .arg("container-dispatch")
        .arg(&manifest_path)
        .arg("--background")
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--background") || stderr.contains("--dry-run"),
        "clap should surface the conflict: {stderr}"
    );
}
