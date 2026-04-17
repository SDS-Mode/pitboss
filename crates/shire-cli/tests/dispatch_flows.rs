mod support;

use std::process::Command;
use support::*;
use tempfile::TempDir;

fn ensure_built() {
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "shire-cli", "-p", "fake-claude"])
        .status().unwrap();
    assert!(status.success(), "build failed");
}

#[test]
fn three_task_mixed_outcomes_produce_summary() {
    ensure_built();
    let repo = TempDir::new().unwrap();
    init_git_repo(repo.path());
    let run_dir = TempDir::new().unwrap();

    let manifest_path = repo.path().join("shire.toml");
    std::fs::write(&manifest_path, format!(r#"
[run]
max_parallel = 2
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
"#, run_dir = run_dir.path().display(), repo = repo.path().display())).unwrap();

    let mut cmd = Command::new(shire_binary());
    cmd.arg("dispatch").arg(&manifest_path);
    cmd.env("SHIRE_CLAUDE_BINARY", fake_claude_path());
    cmd.env("MOSAIC_FAKE_SCRIPT", fixture("success.jsonl"));
    cmd.env("MOSAIC_FAKE_EXIT_CODE", "0");
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr));

    let mut run_dirs = std::fs::read_dir(run_dir.path()).unwrap();
    let rd = run_dirs.next().unwrap().unwrap().path();
    let summary = rd.join("summary.json");
    assert!(summary.exists(), "summary.json missing at {}", summary.display());
    let s: serde_json::Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    assert_eq!(s["tasks_total"].as_u64().unwrap(), 3);
}
