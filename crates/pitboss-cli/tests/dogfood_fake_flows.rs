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
//!
//! ## In-process spotlights (#02+)
//!
//! Spotlights that exercise MCP tools (spawn_sublead, kv_set, kv_get) cannot
//! be driven via subprocess fake-claude because PITBOSS_FAKE_MCP_SOCKET is not
//! injected into the lead's subprocess env in v0.6. These spotlights follow the
//! in-process pattern: spin up DispatchState + McpServer in-test, drive via
//! FakeMcpClient. See spotlight #02 (dogfood_isolation_strict_tree) as the
//! canonical example.

mod support;

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use support::*;
use tempfile::TempDir;

// ── Imports for in-process spotlights (#02+) ─────────────────────────────────
use fake_mcp_client::FakeMcpClient;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{Effort, WorktreeCleanup};
use pitboss_cli::mcp::{socket_path_for_run, McpServer};
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use uuid::Uuid;

/// Build a DispatchState with allow_subleads=true for in-process spotlights.
///
/// Duplicated from `sublead_flows.rs::mk_state_with_subleads` to keep
/// each test file self-contained. The two copies should stay in sync
/// (both use FakeScript::hold_until_signal and a $20 root budget).
fn mk_state_with_subleads() -> (TempDir, Arc<DispatchState>) {
    let dir = TempDir::new().unwrap();
    let lead = ResolvedLead {
        id: "root".into(),
        directory: PathBuf::from("/tmp"),
        prompt: "root prompt".into(),
        branch: None,
        model: "claude-haiku-4-5".into(),
        effort: Effort::High,
        tools: vec![],
        timeout_secs: 3600,
        use_worktree: false,
        env: Default::default(),
        resume_session_id: None,
        allow_subleads: true,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_workers_across_tree: None,
        sublead_defaults: None,
    };
    let manifest = ResolvedManifest {
        max_parallel: 8,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: Some(lead),
        max_workers: Some(20),
        budget_usd: Some(20.0),
        lead_timeout_secs: None,
        approval_policy: None,
        notifications: vec![],
        dump_shared_store: false,
        require_plan_approval: false,
        approval_rules: vec![],
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let run_id = Uuid::now_v7();
    let script = FakeScript::new().hold_until_signal();
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
    let wt_mgr = Arc::new(WorktreeManager::new());
    let run_subdir = dir.path().join(run_id.to_string());
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "root".into(),
        spawner,
        PathBuf::from("claude"),
        wt_mgr,
        CleanupPolicy::Never,
        run_subdir,
        ApprovalPolicy::Block,
        None,
        std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
    ));
    (dir, state)
}

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

// ── Spotlight #02: strict-tree isolation ─────────────────────────────────────

/// Dogfood spotlight #02: per-layer KvStore isolation + strict peer visibility.
///
/// Scenario: an operator runs a depth-2 dispatch where a root lead decomposes
/// a multi-phase job into two parallel sub-trees. Each sub-tree writes progress
/// updates to /shared/progress. This test proves:
///
/// 1. **KV isolation**: S1's /shared/progress and S2's /shared/progress live in
///    separate layer stores. Each sub-lead reads back its own write.
/// 2. **Root isolation**: root's /shared/progress is in a third store, empty
///    after S1 and S2 write to their own layers.
/// 3. **Strict peer visibility**: workers W1 and W2 share the root layer. W2
///    cannot read W1's /peer/W1/status slot. The server rejects the read with a
///    "strict peer visibility" error.
/// 4. **Layer-lead privilege**: root (the layer lead) CAN read W1's peer slot.
///
/// ## Why workers for assertions 3 & 4 (not sub-leads)?
///
/// Peer-visibility enforcement fires when actors share the same coordination
/// layer. Sub-leads each get their *own* layer (S1 is the lead of S1's layer,
/// S2 the lead of S2's). An attempt by S2 to read /peer/<s1_id>/status routes
/// to S2's layer where that key simply doesn't exist — isolation is enforced
/// by layer routing, not the peer-slot authz predicate. The strict-peer error
/// fires for actors (workers) competing within the *same* layer, which is the
/// root-layer worker case demonstrated here.
///
/// ## In-process pattern
///
/// This test spins up DispatchState + McpServer in-process and drives them via
/// FakeMcpClient. Subprocess fake-claude cannot exercise MCP tools in v0.6
/// because PITBOSS_FAKE_MCP_SOCKET is not injected into the lead's subprocess
/// environment (architectural limitation — future work).
#[tokio::test]
async fn dogfood_isolation_strict_tree() {
    use serde_json::json;

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // ── Act as root lead and spawn two sub-leads ──────────────────────────────
    let mut root = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();

    let s1_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "phase 1: gather inputs",
                "model": "claude-haiku-4-5",
                "budget_usd": 1.0,
                "max_workers": 1,
            }),
        )
        .await
        .unwrap();
    let s1_id = s1_resp["sublead_id"]
        .as_str()
        .expect("spawn_sublead should return sublead_id for S1")
        .to_string();

    let s2_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "phase 2: process outputs",
                "model": "claude-haiku-4-5",
                "budget_usd": 1.0,
                "max_workers": 1,
            }),
        )
        .await
        .unwrap();
    let s2_id = s2_resp["sublead_id"]
        .as_str()
        .expect("spawn_sublead should return sublead_id for S2")
        .to_string();

    // Each sub-lead connects to the MCP server with its own identity.
    let mut s1_client = FakeMcpClient::connect_as(&socket, &s1_id, "sublead")
        .await
        .unwrap();
    let mut s2_client = FakeMcpClient::connect_as(&socket, &s2_id, "sublead")
        .await
        .unwrap();

    // ── Each sub-lead writes progress to its own /shared/progress ─────────────
    // Values are UTF-8 bytes for the string content.
    let phase1_bytes: Vec<u8> = b"phase 1 complete".to_vec();
    let phase2_bytes: Vec<u8> = b"phase 2 in progress".to_vec();

    s1_client
        .call_tool(
            "kv_set",
            json!({ "path": "/shared/progress", "value": phase1_bytes }),
        )
        .await
        .unwrap();

    s2_client
        .call_tool(
            "kv_set",
            json!({ "path": "/shared/progress", "value": phase2_bytes }),
        )
        .await
        .unwrap();

    // ── ASSERTION 1: S1 and S2's /shared stores are isolated ──────────────────
    // S1 should see its own write ("phase 1 complete"), NOT S2's.
    let s1_view = s1_client
        .call_tool("kv_get", json!({ "path": "/shared/progress" }))
        .await
        .unwrap();
    let s1_bytes = s1_view["entry"]["value"]
        .as_array()
        .expect("S1 entry.value should be a byte array")
        .iter()
        .map(|b| b.as_u64().unwrap_or(0) as u8)
        .collect::<Vec<u8>>();
    let s1_text = String::from_utf8_lossy(&s1_bytes);
    assert!(
        s1_text.contains("phase 1"),
        "S1 should see its own write, not S2's: got {s1_view:?}"
    );

    // S2 should see its own write ("phase 2 in progress"), NOT S1's.
    let s2_view = s2_client
        .call_tool("kv_get", json!({ "path": "/shared/progress" }))
        .await
        .unwrap();
    let s2_bytes = s2_view["entry"]["value"]
        .as_array()
        .expect("S2 entry.value should be a byte array")
        .iter()
        .map(|b| b.as_u64().unwrap_or(0) as u8)
        .collect::<Vec<u8>>();
    let s2_text = String::from_utf8_lossy(&s2_bytes);
    assert!(
        s2_text.contains("phase 2"),
        "S2 should see its own write, not S1's: got {s2_view:?}"
    );

    // ── ASSERTION 2: Root's /shared is separate from both ─────────────────────
    // Root never wrote to /shared/progress, so its layer should return null.
    let root_view = root
        .call_tool("kv_get", json!({ "path": "/shared/progress" }))
        .await
        .unwrap();
    assert!(
        root_view["entry"].is_null(),
        "root layer should not see either sub-lead's /shared/progress write (separate stores); got: {root_view:?}"
    );

    // ── ASSERTION 3: Strict peer visibility — W2 cannot read W1's peer slot ───
    // Two root-layer workers share the root layer. W1 writes its peer slot.
    // W2 attempts to read it — must be rejected with a strict-peer-visibility
    // error. (Workers are the correct actors here; see test docstring for why
    // sub-leads are not used for this assertion.)
    let mut worker1 = FakeMcpClient::connect_as(&socket, "worker-W1", "worker")
        .await
        .unwrap();
    worker1
        .call_tool(
            "kv_set",
            json!({
                "path": "/peer/self/status",
                "value": b"halfway".to_vec(),
            }),
        )
        .await
        .unwrap();

    let mut worker2 = FakeMcpClient::connect_as(&socket, "worker-W2", "worker")
        .await
        .unwrap();
    let sibling_read = worker2
        .call_tool("kv_get", json!({ "path": "/peer/worker-W1/status" }))
        .await;
    assert!(
        sibling_read.is_err(),
        "strict peer visibility: W2 must NOT be able to read W1's peer slot; got: {sibling_read:?}"
    );
    let err_msg = format!("{:?}", sibling_read.unwrap_err());
    assert!(
        err_msg.contains("strict peer visibility") || err_msg.contains("forbidden"),
        "error should mention strict peer visibility or forbidden; got: {err_msg}"
    );

    // ── ASSERTION 4: Root (layer lead) CAN read W1's peer slot ────────────────
    // Root is the layer lead of the root layer (lead_id = "root"), so it has
    // full visibility over all /peer/* slots in that layer.
    let lead_read = root
        .call_tool("kv_get", json!({ "path": "/peer/worker-W1/status" }))
        .await
        .unwrap();
    assert!(
        !lead_read["entry"].is_null(),
        "root (layer lead) should be able to read W1's peer slot; got: {lead_read:?}"
    );

    // ── Cleanup: reconcile both sub-leads ─────────────────────────────────────
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(&state, &s1_id)
        .await
        .unwrap();
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(&state, &s2_id)
        .await
        .unwrap();
}
