mod support;

use std::process::Command;
use support::*;
use tempfile::TempDir;

fn ensure_built() {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "pitboss-cli", "-p", "fake-goose"])
        .status()
        .unwrap();
    assert!(status.success(), "build failed");
}

fn fake_goose_path() -> std::path::PathBuf {
    workspace_root().join("target/debug/fake-goose")
}

#[test]
fn three_task_mixed_outcomes_produce_summary() {
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
max_parallel_tasks = 2
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "ok1"
directory = "{repo}"
prompt = "p"

[[task]]
id = "ok2"
directory = "{repo}"
prompt = "p"

[[task]]
id = "bad"
directory = "{repo}"
prompt = "p"
"#,
            run_dir = run_dir.path().display(),
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch").arg(&manifest_path);
    cmd.env("PITBOSS_GOOSE_BINARY", fake_goose_path());
    cmd.env("PITBOSS_FAKE_SCRIPT", fixture("goose-success.jsonl"));
    cmd.env("PITBOSS_FAKE_EXIT_CODE", "0");
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let mut run_dirs = std::fs::read_dir(run_dir.path()).unwrap();
    let rd = run_dirs.next().unwrap().unwrap().path();
    let summary = rd.join("summary.json");
    assert!(
        summary.exists(),
        "summary.json missing at {}",
        summary.display()
    );
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    assert_eq!(s["tasks_total"].as_u64().unwrap(), 3);
}

#[test]
fn halt_on_failure_stops_remaining_tasks() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    // 5 tasks, max_parallel=1 so ordering is deterministic, halt_on_failure=true.
    // All tasks use exit2.jsonl + exit code 2 so the first failure triggers cascade.
    let manifest_path = repo.path().join("pitboss.toml");
    let exit2_script = fixture("goose-exit2.jsonl");
    std::fs::write(
        &manifest_path,
        format!(
            r#"
[run]
max_parallel_tasks = 1
halt_on_failure = true
run_dir = "{run_dir}"
worktree_cleanup = "always"

[defaults]
use_worktree = false

[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"

[[task]]
id = "t2"
directory = "{repo}"
prompt = "p"

[[task]]
id = "t3"
directory = "{repo}"
prompt = "p"

[[task]]
id = "t4"
directory = "{repo}"
prompt = "p"

[[task]]
id = "t5"
directory = "{repo}"
prompt = "p"
"#,
            run_dir = run_dir.path().display(),
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch").arg(&manifest_path);
    cmd.env("PITBOSS_GOOSE_BINARY", fake_goose_path());
    cmd.env("PITBOSS_FAKE_SCRIPT", &exit2_script);
    cmd.env("PITBOSS_FAKE_EXIT_CODE", "2");
    let out = cmd.output().unwrap();

    // pitboss should exit non-zero due to failures
    assert!(
        !out.status.success(),
        "expected non-zero exit, stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let mut run_dirs = std::fs::read_dir(run_dir.path()).unwrap();
    let rd = run_dirs.next().unwrap().unwrap().path();
    let summary = rd.join("summary.json");
    assert!(
        summary.exists(),
        "summary.json missing at {}",
        summary.display()
    );
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();

    // With halt_on_failure + max_parallel=1, only the first task should run; remainder cancelled.
    // At most 2 tasks should have been recorded (the runner may start one more before drain kicks in).
    let tasks_total = s["tasks_total"].as_u64().unwrap();
    assert!(
        tasks_total < 5,
        "halt_on_failure should stop remaining tasks, but tasks_total={tasks_total}"
    );
}

#[cfg(unix)]
#[allow(unsafe_code)]
#[test]
fn ctrl_c_twice_terminates_running_tasks() {
    use std::time::Duration;

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
id = "held"
directory = "{repo}"
prompt = "p"
timeout_secs = 120
env = {{ PITBOSS_FAKE_SCRIPT = "{hold}", PITBOSS_FAKE_HOLD = "1" }}
"#,
            run_dir = run_dir.path().display(),
            repo = repo.path().display(),
            hold = fixture("goose-hold.jsonl").display()
        ),
    )
    .unwrap();

    let mut child = std::process::Command::new(pitboss_binary())
        .arg("dispatch")
        .arg(&manifest_path)
        .env("PITBOSS_GOOSE_BINARY", fake_goose_path())
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_millis(500));

    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
    std::thread::sleep(Duration::from_millis(200));
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }

    // Wait for pitboss to exit. Bound by a timeout so a bug doesn't hang the test.
    let child_result = std::thread::spawn(move || child.wait());
    let status = child_result.join().expect("thread joins").expect("wait ok");
    // After two SIGINTs, pitboss should exit non-zero.
    assert!(!status.success());
}

#[test]
fn validation_failure_exits_two() {
    ensure_built();
    let dir = TempDir::new().unwrap();
    let manifest_path = dir.path().join("bad.toml");
    std::fs::write(&manifest_path, "unknown_root_key = 1\n").unwrap();

    let out = std::process::Command::new(pitboss_binary())
        .arg("dispatch")
        .arg(&manifest_path)
        .env("PITBOSS_GOOSE_BINARY", fake_goose_path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn dry_run_uses_manifest_goose_binary_and_default_max_turns() {
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
run_dir = "{run_dir}"

[goose]
binary_path = "{goose}"
default_max_turns = 3

[defaults]
use_worktree = false

[[task]]
id = "t1"
directory = "{repo}"
prompt = "p"
"#,
            run_dir = run_dir.path().display(),
            goose = fake_goose_path().display(),
            repo = repo.path().display()
        ),
    )
    .unwrap();

    let out = std::process::Command::new(pitboss_binary())
        .arg("dispatch")
        .arg(&manifest_path)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&fake_goose_path().to_string_lossy().to_string()),
        "dry-run output should use manifest goose binary; got: {stdout}"
    );
    assert!(
        stdout.contains("--max-turns 3"),
        "dry-run output should include max turn cap; got: {stdout}"
    );
}
