//! Dogfood integration tests — fake-claude subprocess dispatch.
//!
//! These tests drive `pitboss dispatch` as a real subprocess (like
//! `dispatch_flows.rs`) against the manifests in
//! `examples/dogfood/fake/`. They prove that the operator-facing CLI
//! path works end-to-end for each spotlight feature group.
//!
//! Fake-claude is used as the Claude binary so tests are deterministic
//! and never call the Anthropic API. The lead script is a pre-baked
//! JSONL file from the spotlight directory.
//!
//! ## MCP note for spotlight #01
//!
//! `spawn_sublead_session` is a stub in v0.6 (Task 2.3 wires real sub-lead
//! sessions). The spotlight #01 lead script emits a clean stream-json
//! success result without connecting to the pitboss MCP socket. This
//! proves the manifest schema, dispatch pipeline, and summary generation
//! for depth-2 manifests. The `spawn_sublead` MCP call lifecycle is
//! covered end-to-end by `crates/pitboss-cli/tests/e2e_sublead_flows.rs`.

mod support;

use std::process::Command;
use support::*;
use tempfile::TempDir;

/// Locate the dogfood directory relative to the workspace root.
fn dogfood_dir(subpath: &str) -> std::path::PathBuf {
    workspace_root().join("examples/dogfood/fake").join(subpath)
}

// ── Spotlight #01: smoke-hello-sublead ────────────────────────────────────────

/// Proves that a depth-2 manifest with `allow_subleads = true` dispatches
/// cleanly end-to-end:
///   - `pitboss dispatch` exits 0
///   - `summary.json` is written with `tasks_failed = 0`,
///     `tasks_total = 1`, and `was_interrupted = false`
///   - The lead task record shows `status = "Success"` and
///     `model = "claude-haiku-4-5"`
///
/// This is spotlight #01 of an eventual suite of 6 fake + 3 real dogfood
/// manifests. It establishes the directory structure, file templates, and
/// test harness pattern that subsequent spotlights follow.
#[test]
fn dogfood_smoke_hello_sublead() {
    ensure_built();

    let spotlight = dogfood_dir("01-smoke-hello-sublead");
    let manifest_path = spotlight.join("manifest.toml");
    assert!(
        manifest_path.exists(),
        "manifest.toml not found at {}",
        manifest_path.display()
    );
    let script_path = spotlight.join("lead-script.jsonl");
    assert!(
        script_path.exists(),
        "lead-script.jsonl not found at {}",
        script_path.display()
    );

    // Use a temp directory as the run_dir so we can find summary.json
    // deterministically without relying on ~/.local/share/pitboss/runs.
    let run_dir = TempDir::new().expect("create temp run_dir");

    let mut cmd = Command::new(pitboss_binary());
    cmd.arg("dispatch").arg(&manifest_path);
    // Override the run directory so artifacts land in our temp dir.
    // pitboss dispatch reads PITBOSS_RUN_DIR for this purpose; however
    // that env var may not exist — instead we write a temporary manifest
    // snapshot with run_dir overridden. Simpler: pass --run-dir flag if
    // available. Actually, looking at the CLI the run_dir is a flag
    // `--run-dir <PATH>`.
    cmd.arg("--run-dir").arg(run_dir.path());
    cmd.env("PITBOSS_CLAUDE_BINARY", fake_claude_path());
    cmd.env("PITBOSS_FAKE_SCRIPT", &script_path);

    let out = cmd.output().expect("spawn pitboss dispatch");
    assert!(
        out.status.success(),
        "pitboss dispatch exited non-zero (code={:?}).\nstdout={}\nstderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Locate the run subdirectory written under run_dir (one UUID-named entry).
    let run_subdirs: Vec<_> = std::fs::read_dir(run_dir.path())
        .expect("read run_dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        run_subdirs.len(),
        1,
        "expected exactly one run subdirectory in {}, got {}",
        run_dir.path().display(),
        run_subdirs.len()
    );
    let run_subdir = run_subdirs[0].path();

    // Read and parse summary.json.
    let summary_path = run_subdir.join("summary.json");
    assert!(
        summary_path.exists(),
        "summary.json not written at {}",
        summary_path.display()
    );
    let summary_bytes = std::fs::read(&summary_path).expect("read summary.json");
    let summary: serde_json::Value =
        serde_json::from_slice(&summary_bytes).expect("parse summary.json");

    // Assert structured subset against expected-summary.json.
    assert_eq!(
        summary["tasks_failed"].as_u64(),
        Some(0),
        "expected tasks_failed=0, got summary: {}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    assert_eq!(
        summary["tasks_total"].as_u64(),
        Some(1),
        "expected tasks_total=1 (just the lead), got: {}",
        summary["tasks_total"]
    );
    assert_eq!(
        summary["was_interrupted"].as_bool(),
        Some(false),
        "expected was_interrupted=false"
    );

    // Lead task record assertions.
    let tasks = summary["tasks"]
        .as_array()
        .expect("summary.tasks should be an array");
    assert_eq!(tasks.len(), 1, "expected exactly one task record");
    let lead = &tasks[0];
    // The single-lead manifest format (`[lead]` single table) resolves with
    // an empty `id` (set at runtime from the CWD context). Assert it's a
    // string (not null) rather than asserting a specific value.
    assert!(
        lead["task_id"].is_string(),
        "lead task_id should be a string, got: {}",
        lead["task_id"]
    );
    assert_eq!(
        lead["status"].as_str(),
        Some("Success"),
        "lead status should be Success"
    );
    assert_eq!(
        lead["exit_code"].as_i64(),
        Some(0),
        "lead exit_code should be 0"
    );
    assert_eq!(
        lead["model"].as_str(),
        Some("claude-haiku-4-5"),
        "lead model should be claude-haiku-4-5"
    );
}
