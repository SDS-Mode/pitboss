//! End-to-end hierarchical integration tests. Unlike `hierarchical_flows.rs`
//! which drives the MCP server directly as a client (via FakeMcpClient),
//! these tests spawn fake-claude as a real subprocess via SessionHandle +
//! TokioSpawner. The subprocess connects to the MCP socket directly and
//! issues real tool calls via its mcp_call script action.
//!
//! Decisions locked in
//! docs/superpowers/specs/2026-04-17-pitboss-v041-fake-claude-mcp-e2e-design.md.

#![allow(dead_code)]
#![allow(unused_imports)] // Helpers + types re-used by Tasks 7-10.

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
use pitboss_cli::mcp::{socket_path_for_run, McpServer};
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::{ProcessSpawner, SpawnCmd, TokioSpawner};
use pitboss_core::session::{CancelToken, SessionHandle, SessionOutcome};
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use uuid::Uuid;

use support::fake_claude_path;

/// Build a DispatchState configured with a FakeSpawner producing short,
/// successful worker runs. Lead is NOT run here — callers spawn fake-claude
/// themselves via a separate TokioSpawner.
fn mk_state(
    run_dir: &std::path::Path,
    approval_policy: ApprovalPolicy,
) -> (Uuid, Arc<DispatchState>) {
    let run_id = Uuid::now_v7();
    let lead = ResolvedLead {
        id: "lead".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "lead prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: run_dir.to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
        approval_policy: Some(approval_policy),
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.to_path_buf()));
    // Worker-side spawner: emits a complete stream-json run then exits 0.
    let worker_script = FakeScript::new()
        .stdout_line(r#"{"type":"system","subtype":"init","session_id":"worker-sess"}"#)
        .stdout_line(
            r#"{"type":"result","session_id":"worker-sess","usage":{"input_tokens":10,"output_tokens":20}}"#,
        )
        .exit_code(0);
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(worker_script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = run_dir.join(run_id.to_string());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        approval_policy,
    ));
    (run_id, state)
}

/// Build + spawn fake-claude as the lead subprocess, connecting to `mcp_sock`.
/// Returns the SessionOutcome.
async fn run_fake_claude_lead(
    cwd: &std::path::Path,
    script_path: &std::path::Path,
    mcp_sock: &std::path::Path,
    cancel: CancelToken,
    timeout: Duration,
) -> SessionOutcome {
    let mut env = HashMap::new();
    env.insert(
        "MOSAIC_FAKE_SCRIPT".to_string(),
        script_path.to_string_lossy().to_string(),
    );
    env.insert(
        "PITBOSS_FAKE_MCP_SOCKET".to_string(),
        mcp_sock.to_string_lossy().to_string(),
    );
    let cmd = SpawnCmd {
        program: fake_claude_path(),
        args: vec![],
        cwd: cwd.to_path_buf(),
        env,
    };
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    SessionHandle::new("lead", spawner, cmd)
        .run_to_completion(cancel, timeout)
        .await
}

#[tokio::test]
async fn fake_claude_smoke_prints_version_stderr() {
    // Sanity-check that the fake-claude binary was built and can run without
    // the MCP env vars set. Uses --version fast-path which prints to stdout
    // and exits 0, doesn't touch any script.
    support::ensure_built();
    let output = std::process::Command::new(fake_claude_path())
        .arg("--version")
        .output()
        .expect("exec fake-claude --version");
    assert!(
        output.status.success(),
        "fake-claude --version failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fake-claude"),
        "expected --version output to mention fake-claude, got: {stdout:?}"
    );
}
