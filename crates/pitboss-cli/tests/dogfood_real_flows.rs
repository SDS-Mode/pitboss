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

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
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

// ── R2: Real root lead kill-with-reason side-channel ─────────────────────────

/// R2 smoke test: a real claude-haiku-4-5 root lead spawns a worker; an
/// external operator (this test, via a concurrent FakeMcpClient) kills the
/// worker with `reason="use CSV format instead of JSON"`; the dispatcher's
/// `cancel_actor_with_reason` mechanism fires and delivers a synthetic
/// `[SYSTEM]` reprompt to the lead's layer.
///
/// ## Orchestration pattern
///
/// Subprocess-driven dispatch (like R1): runs `pitboss dispatch` as a
/// child process, then races a side-channel that:
///
/// 1. Discovers the MCP socket created under `--run-dir` (unsetting
///    `XDG_RUNTIME_DIR` forces the socket to `<run_dir>/<run_id>/mcp.sock`).
/// 2. Connects a `FakeMcpClient` and polls `list_workers` until a non-lead
///    worker appears (real claude called `spawn_worker`).
/// 3. Calls `cancel_worker` with `target=<worker_id>` and
///    `reason="use CSV format instead of JSON"`.
/// 4. Waits for `pitboss dispatch` to exit.
///
/// ## What is asserted
///
/// - `pitboss dispatch` exits with code 0 (the run completed cleanly even
///   though a worker was cancelled mid-flight).
/// - The reason text appears in the combined output/stderr (the
///   `send_synthetic_reprompt` tracing log at `info` level confirms the
///   kill-with-reason mechanism routed the reason to the root layer).
/// - At least one worker appeared in `list_workers` before the cancel
///   (real claude actually called `spawn_worker`).
///
/// ## Caveat: reprompt delivery is currently a stub
///
/// `LayerState::send_synthetic_reprompt` logs the reason at `info` level but
/// does NOT inject it into the running claude session (real session wiring is
/// deferred to a future task). The "lead adapts" assertion is therefore omitted
/// here. What R2 validates is the kill-with-reason *routing* primitive: the
/// cancel fires, the reason is routed to the right layer, and the mechanism
/// is observable in the tracing log.
///
/// ## Cost
///
/// ~$0.10-$0.20 per run (haiku, lead + worker spawn round-trip).
#[tokio::test]
#[ignore = "real-claude smoke — set PITBOSS_DOGFOOD_REAL=1 and run with --ignored"]
async fn real_kill_with_reason() {
    if should_skip("real_kill_with_reason") {
        return;
    }

    let pitboss = pitboss_release_binary();

    let manifest_path =
        workspace_root().join("examples/dogfood/real/R2-real-kill-with-reason/manifest.toml");
    assert!(
        manifest_path.exists(),
        "R2 manifest not found at {}",
        manifest_path.display()
    );

    let run_dir = TempDir::new().expect("create temp run_dir");

    // Spawn `pitboss dispatch` as a child process. We unset XDG_RUNTIME_DIR so
    // the MCP socket lands at <run_dir>/<run_id>/mcp.sock (discoverable from
    // the test without knowing the run_id in advance).
    let child = Command::new(&pitboss)
        .arg("dispatch")
        .arg(&manifest_path)
        .arg("--run-dir")
        .arg(run_dir.path())
        .env("RUST_LOG", "pitboss_cli=info,pitboss_core=info")
        .env_remove("XDG_RUNTIME_DIR")
        // Forward ANTHROPIC_API_KEY from the outer environment.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn pitboss dispatch");

    // ── Side-channel: discover socket, connect, wait for worker, cancel ──────

    let run_dir_path = run_dir.path().to_path_buf();

    let cancel_outcome =
        tokio::time::timeout(Duration::from_secs(120), r2_side_channel(run_dir_path)).await;

    // ── Wait for `pitboss dispatch` to exit ───────────────────────────────────

    let out = child
        .wait_with_output()
        .expect("wait_with_output on pitboss dispatch");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");

    // ── ASSERTION 1: side-channel completed without timeout ───────────────────
    //
    // If the side-channel timed out, real claude probably never called
    // spawn_worker. Report the stdout/stderr for diagnosis.
    let side_result = cancel_outcome.unwrap_or_else(|_| {
        panic!(
            "R2 side-channel timed out waiting for a worker to appear.\n\
             This likely means real claude did not call spawn_worker within 120s.\n\
             stdout={stdout}\nstderr={stderr}"
        )
    });

    // Warn (but don't fail) if the cancel itself encountered an error —
    // the worker may have already finished by the time we tried to cancel it.
    if let Err(ref e) = side_result {
        eprintln!("real_kill_with_reason: cancel_worker returned error (may be benign): {e}");
    }

    // ── ASSERTION 2: dispatch exited 0 ───────────────────────────────────────
    assert!(
        out.status.success(),
        "pitboss dispatch exited non-zero (code={:?}).\nstdout={stdout}\nstderr={stderr}",
        out.status.code(),
    );

    // ── ASSERTION 3: reason text visible in tracing log ──────────────────────
    //
    // `cancel_actor_with_reason` calls `layer.send_synthetic_reprompt(&msg)`
    // which logs at `info` level:
    //   "synthetic reprompt (no session wired): [SYSTEM] Actor <id> was killed ..."
    // We check for the reason keyword to confirm the routing mechanism fired.
    //
    // If the cancel failed (worker already done), the reprompt may not have
    // fired. We only assert when the cancel succeeded.
    if side_result.is_ok() {
        let reason_keyword = "csv format";
        assert!(
            combined.to_lowercase().contains(reason_keyword),
            "expected kill reason '{reason_keyword}' to appear in tracing log, \
             confirming cancel_actor_with_reason fired.\n\
             stdout={stdout}\nstderr={stderr}"
        );
    }
}

/// Side-channel task: discovers the MCP socket, waits for a worker, cancels it.
///
/// Returns `Ok(())` when `cancel_worker` was called (whether or not the
/// worker was still alive), or `Err` if the MCP call failed unexpectedly.
async fn r2_side_channel(run_dir: PathBuf) -> anyhow::Result<()> {
    // ── Step 1: wait for the run subdirectory to appear ──────────────────────
    let run_subdir = wait_for_run_subdir(&run_dir, Duration::from_secs(30)).await?;

    // ── Step 2: wait for mcp.sock to appear ──────────────────────────────────
    let mcp_sock = run_subdir.join("mcp.sock");
    wait_for_path(&mcp_sock, Duration::from_secs(30)).await?;

    // ── Step 3: connect FakeMcpClient ────────────────────────────────────────
    // Connect with root_lead identity so cancel_worker is accepted without
    // role-authz issues (cancel_worker has no role check, but the _meta field
    // is required by some tool handlers; root_lead is always safe).
    let mut client =
        fake_mcp_client::FakeMcpClient::connect_as(&mcp_sock, "r2-operator", "root_lead").await?;

    // ── Step 4: poll list_workers until a non-lead worker appears ─────────────
    let worker_id = wait_for_first_worker_via_mcp(&mut client, Duration::from_secs(90)).await?;
    eprintln!("real_kill_with_reason: worker appeared: {worker_id}");

    // ── Step 5: cancel with reason ───────────────────────────────────────────
    let cancel_result = client
        .call_tool(
            "cancel_worker",
            serde_json::json!({
                "target": worker_id,
                "reason": "use CSV format instead of JSON"
            }),
        )
        .await;
    eprintln!("real_kill_with_reason: cancel_worker result: {cancel_result:?}");

    // An error here is benign if the worker already finished before we arrived;
    // propagate so the caller can decide whether to assert on the log.
    cancel_result.map(|_| ())
}

/// Poll `run_dir` until a subdirectory (the run_id dir) appears.
/// Returns the path of the first subdirectory found.
async fn wait_for_run_subdir(
    run_dir: &std::path::Path,
    timeout: Duration,
) -> anyhow::Result<PathBuf> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(mut entries) = tokio::fs::read_dir(run_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let p = entry.path();
                if p.is_dir() {
                    return Ok(p);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for run subdir to appear in {}",
                run_dir.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Poll until `path` exists on the filesystem.
async fn wait_for_path(path: &std::path::Path, timeout: Duration) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if path.exists() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for {} to appear", path.display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Poll `list_workers` via an open MCP client until at least one worker
/// (non-lead entry) is reported, then return its task_id.
async fn wait_for_first_worker_via_mcp(
    client: &mut fake_mcp_client::FakeMcpClient,
    timeout: Duration,
) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let resp = client
            .call_tool("list_workers", serde_json::json!({}))
            .await?;
        if let Some(workers) = resp["workers"].as_array() {
            // Any non-Done worker is a candidate for cancellation.
            for w in workers {
                let state = w["state"].as_str().unwrap_or("");
                let task_id = w["task_id"].as_str().unwrap_or("").to_string();
                // Exclude workers that already completed/failed before we arrived.
                if !state.starts_with("done") && !state.starts_with("Done") && !task_id.is_empty() {
                    return Ok(task_id);
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for a live worker to appear in list_workers");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
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
