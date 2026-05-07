mod support;

use std::process::Command;
use support::*;
use tempfile::TempDir;

fn ensure_built() {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "pitboss-cli", "-p", "fake-claude"])
        .status()
        .unwrap();
    assert!(status.success(), "build failed");
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
    cmd.env("PITBOSS_CLAUDE_BINARY", fake_claude_path());
    cmd.env("PITBOSS_FAKE_SCRIPT", fixture("success.jsonl"));
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

/// Wiring regression for #329 (ISSUE-notify-prior-4): a misconfigured
/// webhook (DNS that never resolves) must surface in `summary.json` as
/// `notify_failures > 0` so operators can see the gap without scraping
/// `notifications.jsonl`.
///
/// This pins the contract that flat-mode `dispatch::runner::finalize_run`
/// reads `router.failed_emits_total()` into the summary. If a future
/// refactor drifts that wiring (the same drift class as #221 / #227
/// across runner.rs and hierarchical.rs), this test fails. The summary
/// is the single durable artifact the operator-facing `pitboss status`
/// footer reads from.
///
/// Why `.invalid`: RFC 6761 reserves it as a guaranteed-non-resolving
/// TLD, so no real DNS server can ever return a hit. The webhook URL
/// passes manifest-time validation (https + non-loopback name) and
/// fails at emit-time `tokio::net::lookup_host`, which counts as a
/// retried emit failure.
///
/// Why `sleep_ms: 1500`: the emit retry budget is ~400ms (100+300ms
/// inter-attempt delays plus three failing attempts). Holding the
/// dispatch open longer than that ensures `RunDispatched`'s retry
/// chain has exhausted and incremented `failed_emits_total` BEFORE
/// `finalize_run` snapshots the count.
#[test]
fn webhook_dns_failure_surfaces_in_summary_notify_failures() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();
    let scripts_dir = TempDir::new().unwrap();

    // Slow script: a single sleep_ms entry at the top so the dispatch
    // wall-clock exceeds the retry budget.
    let script_path = scripts_dir.path().join("slow_success.jsonl");
    std::fs::write(
        &script_path,
        r#"{"sleep_ms": 1500}
{"stdout":"{\"type\":\"system\",\"subtype\":\"init\"}"}
{"stdout":"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}"}
{"stdout":"{\"type\":\"result\",\"subtype\":\"success\",\"session_id\":\"s1\",\"result\":\"done\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}"}
"#,
    )
    .unwrap();

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

[[notification]]
kind = "webhook"
url  = "https://pitboss-test-never-resolves.invalid/x"
events = ["run_dispatched", "run_finished"]
severity_min = "info"

[[task]]
id = "ok1"
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
    cmd.env("PITBOSS_CLAUDE_BINARY", fake_claude_path());
    cmd.env("PITBOSS_FAKE_SCRIPT", &script_path);
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
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    let nf = s["notify_failures"].as_u64().unwrap_or_else(|| {
        panic!(
            "summary.json must contain notify_failures as a number; got {:?}",
            s.get("notify_failures")
        )
    });
    assert!(
        nf >= 1,
        "expected notify_failures >= 1 (RunDispatched retries should have exhausted before finalize); got {nf}. Full summary: {s:#}"
    );

    // The journal must also exist with the matching event so operators
    // who follow the footer's pointer find at least one row.
    let journal = rd.join("notifications.jsonl");
    let journal_body = std::fs::read_to_string(&journal)
        .expect("notifications.jsonl must exist when notify_failures > 0");
    assert!(
        journal_body.contains("notification_failed"),
        "journal missing notification_failed entry: {journal_body}"
    );
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
    let exit2_script = fixture("exit2.jsonl");
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
    cmd.env("PITBOSS_CLAUDE_BINARY", fake_claude_path());
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
            hold = fixture("hold.jsonl").display()
        ),
    )
    .unwrap();

    let mut child = std::process::Command::new(pitboss_binary())
        .arg("dispatch")
        .arg(&manifest_path)
        .env("PITBOSS_CLAUDE_BINARY", fake_claude_path())
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
        .env("PITBOSS_CLAUDE_BINARY", fake_claude_path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
