//! Real-claude smoke tests — gated by PITBOSS_DOGFOOD_REAL=1.
//!
//! These tests actually invoke the Anthropic API via the `claude` CLI and
//! cost real money (~$0.05 per haiku run). They are:
//!
//! - Skipped by default (`#[ignore]` requires `cargo test -- --ignored`)
//! - Belt-and-braces gated by `PITBOSS_DOGFOOD_REAL=1` env var
//! - Gated by presence of the `claude` binary in PATH
//! - Gated by presence of `ANTHROPIC_API_KEY` in the environment
//!
//! Intended for manual validation of LLM-adaptive behaviour that
//! fake-claude cannot represent: real model variance, tool discoverability
//! at runtime, and actual MCP round-trips through the stdio bridge.
//!
//! ## Running
//!
//! ```
//! PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows -- --ignored
//! ```

mod support;

use std::process::Command;
use support::workspace_root;
use tempfile::TempDir;

// ── Preflight helpers ────────────────────────────────────────────────────────

/// Returns true when the test should be skipped, printing a reason to stderr.
fn should_skip(test_name: &str) -> bool {
    if std::env::var("PITBOSS_DOGFOOD_REAL").is_err() {
        eprintln!("{test_name}: skipping — set PITBOSS_DOGFOOD_REAL=1 to run");
        return true;
    }

    // Check that the `claude` binary is on PATH by attempting to run it.
    // We use `--version` which is a fast, non-interactive flag available on
    // all recent Claude CLI builds. We deliberately avoid the `which` crate
    // to keep dev-dependencies lean.
    let claude_check = Command::new("claude").arg("--version").output();
    if claude_check.is_err() || !claude_check.unwrap().status.success() {
        eprintln!("{test_name}: skipping — `claude` CLI not in PATH or not executable");
        return true;
    }

    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("{test_name}: skipping — ANTHROPIC_API_KEY not set");
        return true;
    }

    false
}

/// Locate the pitboss release binary, building it first if needed.
///
/// R1 uses the release binary (same as run.sh) so the test reflects what an
/// operator would actually execute.
fn pitboss_release_binary() -> std::path::PathBuf {
    let root = workspace_root();
    let bin = root.join("target/release/pitboss");
    if !bin.exists() {
        eprintln!("pitboss release binary not found; building…");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--workspace", "--release"])
            .current_dir(&root)
            .status()
            .expect("cargo build failed to spawn");
        assert!(status.success(), "cargo build --release failed");
    }
    bin
}

// ── R1: Real root lead uses spawn_sublead ────────────────────────────────────

/// R1 smoke test: a real claude-haiku-4-5 root lead, given a prompt that
/// explicitly asks it to decompose a two-phase job into sub-leads, actually
/// calls the `spawn_sublead` MCP tool at least once.
///
/// ## What is asserted (loose — real-model variance)
///
/// - `pitboss dispatch` exits with code 0
/// - `summary.json` is written with `status = "Success"` for the lead task
/// - At least one `sublead-` token appears in the combined process output,
///   confirming that the MCP `spawn_sublead` tool was called and a sublead_id
///   was minted and returned to the model
///
/// ## What is NOT asserted
///
/// - Exact number of sub-leads (haiku may spawn 1 or 2)
/// - Exact prompt text passed to `spawn_sublead`
/// - That sub-lead sessions run (stub in v0.6 — Task 2.3 wires real sessions)
/// - That `wait_actor` returns successfully (it may timeout due to stub sessions)
///
/// ## Cost
///
/// ~$0.05 per run (haiku, small prompts).
#[tokio::test]
#[ignore = "real-claude smoke — set PITBOSS_DOGFOOD_REAL=1 and run with --ignored"]
async fn real_root_spawns_sublead() {
    if should_skip("real_root_spawns_sublead") {
        return;
    }

    let pitboss = pitboss_release_binary();

    let manifest_path =
        workspace_root().join("examples/dogfood/real/R1-real-root-spawns-sublead/manifest.toml");
    assert!(
        manifest_path.exists(),
        "R1 manifest not found at {}",
        manifest_path.display()
    );

    // Use a temporary run directory so we can locate summary.json
    // deterministically without relying on ~/.local/share/pitboss/runs.
    let run_dir = TempDir::new().expect("create temp run_dir");

    let mut cmd = Command::new(&pitboss);
    cmd.arg("dispatch")
        .arg(&manifest_path)
        .arg("--run-dir")
        .arg(run_dir.path())
        // Enable debug-level tracing so the sublead-<uuid> token appears in
        // stderr when spawn_sublead returns its result to the model.
        .env("RUST_LOG", "pitboss_cli=debug,pitboss_core=debug");

    let out = cmd.output().expect("spawn pitboss dispatch");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // ── ASSERTION 1: Exit code 0 ─────────────────────────────────────────────
    assert!(
        out.status.success(),
        "pitboss dispatch exited non-zero (code={:?}).\nstdout={stdout}\nstderr={stderr}",
        out.status.code(),
    );

    // ── ASSERTION 2: summary.json written with Success status ────────────────
    let run_subdirs: Vec<_> = std::fs::read_dir(run_dir.path())
        .expect("read run_dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        run_subdirs.len(),
        1,
        "expected exactly one run subdirectory in {}, got {}.\nstdout={stdout}\nstderr={stderr}",
        run_dir.path().display(),
        run_subdirs.len()
    );
    let run_subdir = run_subdirs[0].path();

    let summary_path = run_subdir.join("summary.json");
    assert!(
        summary_path.exists(),
        "summary.json not written at {}.\nstdout={stdout}\nstderr={stderr}",
        summary_path.display()
    );

    let summary_bytes = std::fs::read(&summary_path).expect("read summary.json");
    let summary: serde_json::Value =
        serde_json::from_slice(&summary_bytes).expect("parse summary.json");

    assert_eq!(
        summary["tasks_failed"].as_u64(),
        Some(0),
        "expected tasks_failed=0, got:\n{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    assert_eq!(
        summary["was_interrupted"].as_bool(),
        Some(false),
        "expected was_interrupted=false"
    );

    let tasks = summary["tasks"]
        .as_array()
        .expect("summary.tasks should be an array");
    let lead = tasks
        .iter()
        .find(|t| t["parent_task_id"].is_null())
        .expect("at least one task with no parent (the lead)");
    assert_eq!(
        lead["status"].as_str(),
        Some("Success"),
        "lead task status should be Success"
    );

    // ── ASSERTION 3: At least one sub-lead was spawned ───────────────────────
    //
    // The `spawn_sublead` handler in the MCP server returns
    // `{ "sublead_id": "sublead-<uuid>" }` to the model. This token
    // propagates through the MCP bridge and appears in the process output
    // (or in the tracing stream with RUST_LOG=debug). Checking for
    // `"sublead-"` is the most reliable subprocess-observable signal that
    // the MCP tool was actually called and returned successfully.
    assert!(
        combined.contains("sublead-"),
        "expected at least one 'sublead-' token in combined output, indicating \
         spawn_sublead was called.\nstdout={stdout}\nstderr={stderr}"
    );
}

// ── R2: Real root lead adapts after worker is killed-with-reason ─────────────

/// R2 smoke test: a real claude-haiku-4-5 root lead spawns a worker; an
/// external operator kills the worker with reason="output should be CSV not
/// JSON"; the synthetic [SYSTEM] reprompt is injected into the lead's session;
/// the lead's next turn visibly references the kill reason.
///
/// ## What is asserted (loose — real-model variance)
///
/// Ideally:
/// - `pitboss dispatch` exits 0
/// - The lead's final message contains "csv", "format", "reason", or "adjust"
///   indicating the model processed the synthetic reprompt
///
/// ## Current status: stub
///
/// Full R2 requires a side-channel operator that connects to the MCP socket
/// while the real-claude dispatch is mid-flight and calls `cancel_worker` with
/// a reason. The Option A pattern (in-process DispatchState + real claude
/// subprocess + FakeMcpClient side-channel) requires additional test
/// infrastructure beyond the R1 dispatch-subprocess pattern. This is deferred.
///
/// The stub skips immediately with a message. A future implementation should
/// wire the side-channel cancel using Option A from the R2 design doc.
///
/// ## Cost
///
/// ~$0.10-$0.20 per run when fully implemented (haiku, lead + worker round-trips).
#[tokio::test]
#[ignore = "real-claude smoke — set PITBOSS_DOGFOOD_REAL=1 and run with --ignored"]
async fn real_kill_with_reason() {
    if should_skip("real_kill_with_reason") {
        return;
    }

    // R2 not yet implemented — requires real-claude + side-channel cancel
    // orchestration (Option A: in-process DispatchState + real claude
    // subprocess + FakeMcpClient concurrent cancel_worker call).
    //
    // Tracked as follow-up work. See:
    //   examples/dogfood/real/R2-real-kill-with-reason/README.md
    eprintln!(
        "real_kill_with_reason: SKIPPED — R2 not yet implemented; \
         requires real-claude + side-channel cancel orchestration. \
         See examples/dogfood/real/R2-real-kill-with-reason/README.md"
    );
}

// ── R3: Real root lead adapts after request_approval is rejected ──────────────

/// R3 smoke test: a real claude-haiku-4-5 root lead, given a prompt that
/// asks it to call `request_approval` before writing files, receives an
/// automatic rejection (via `approval_policy = "auto_reject"` in the manifest).
/// The model's next turn adapts its output format, demonstrating LLM-adaptive
/// behaviour in response to a tool-call rejection.
///
/// ## What is asserted (loose — real-model variance)
///
/// - `pitboss dispatch` exits with code 0
/// - `summary.json` is written with `status = "Success"` for the lead task
/// - The lead's output (stdout.log or final_message_preview) contains at least
///   one of: "csv", "reject", "not approved", "format", "instead" — indicating
///   the model read the `approved: false` response and adapted its plan
///
/// ## What is NOT asserted
///
/// - Exact phrasing of the adaptation response (real-model variance)
/// - That a CSV file is written (the prompt asks for stdout output only)
/// - That the model uses the exact phrase "auto-rejected by policy"
/// - Token counts or cost
///
/// ## Cost
///
/// ~$0.10-$0.20 per run (haiku, approval tool round-trip + adaptation turn).
#[tokio::test]
#[ignore = "real-claude smoke — set PITBOSS_DOGFOOD_REAL=1 and run with --ignored"]
async fn real_reject_with_reason() {
    if should_skip("real_reject_with_reason") {
        return;
    }

    let pitboss = pitboss_release_binary();

    let manifest_path =
        workspace_root().join("examples/dogfood/real/R3-real-reject-with-reason/manifest.toml");
    assert!(
        manifest_path.exists(),
        "R3 manifest not found at {}",
        manifest_path.display()
    );

    let run_dir = TempDir::new().expect("create temp run_dir");

    let mut cmd = Command::new(&pitboss);
    cmd.arg("dispatch")
        .arg(&manifest_path)
        .arg("--run-dir")
        .arg(run_dir.path())
        .env("RUST_LOG", "pitboss_cli=debug,pitboss_core=debug");

    let out = cmd.output().expect("spawn pitboss dispatch");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // ── ASSERTION 1: Exit code 0 ─────────────────────────────────────────────
    assert!(
        out.status.success(),
        "pitboss dispatch exited non-zero (code={:?}).\nstdout={stdout}\nstderr={stderr}",
        out.status.code(),
    );

    // ── ASSERTION 2: summary.json written ───────────────────────────────────
    let run_subdirs: Vec<_> = std::fs::read_dir(run_dir.path())
        .expect("read run_dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    assert_eq!(
        run_subdirs.len(),
        1,
        "expected exactly one run subdirectory, got {}.\nstdout={stdout}\nstderr={stderr}",
        run_subdirs.len()
    );
    let run_subdir = run_subdirs[0].path();

    let summary_path = run_subdir.join("summary.json");
    assert!(
        summary_path.exists(),
        "summary.json not written at {}.\nstdout={stdout}\nstderr={stderr}",
        summary_path.display()
    );

    let summary_bytes = std::fs::read(&summary_path).expect("read summary.json");
    let summary: serde_json::Value =
        serde_json::from_slice(&summary_bytes).expect("parse summary.json");

    let tasks = summary["tasks"]
        .as_array()
        .expect("summary.tasks should be an array");
    let lead = tasks
        .iter()
        .find(|t| t["parent_task_id"].is_null())
        .expect("at least one task with no parent (the lead)");
    assert_eq!(
        lead["status"].as_str(),
        Some("Success"),
        "lead task status should be Success"
    );

    // ── ASSERTION 3: Lead adapted to the rejection ───────────────────────────
    //
    // The prompt instructs the lead: "if approval returns approved=false,
    // produce CSV output instead and say so". With auto_reject, the MCP tool
    // returns { approved: false, comment: "auto-rejected by policy" }. The
    // real model should read the tool response and adapt.
    //
    // We check:
    //   (a) final_message_preview in summary.json (if set by the session)
    //   (b) stdout.log of the lead task (full transcript)
    //
    // A loose keyword match is sufficient to demonstrate adaptive behaviour.
    let final_preview = lead["final_message_preview"]
        .as_str()
        .unwrap_or("")
        .to_lowercase();

    let log_path = run_subdir.join("tasks").join("lead").join("stdout.log");
    let log_contents = std::fs::read_to_string(&log_path)
        .unwrap_or_default()
        .to_lowercase();

    let combined_output = format!("{final_preview}\n{log_contents}");

    let adaptation_keywords = ["csv", "reject", "not approved", "format", "instead"];
    let adapted = adaptation_keywords
        .iter()
        .any(|kw| combined_output.contains(kw));

    assert!(
        adapted,
        "expected lead output to reference the rejection (one of {:?}), \
         but none were found.\nfinal_preview={final_preview:?}\n\
         log path={}\nlog snippet={:.500}",
        adaptation_keywords,
        log_path.display(),
        &combined_output[..combined_output.len().min(500)]
    );
}
