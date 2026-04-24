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
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
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
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(
        &state,
        &s1_id,
        pitboss_cli::dispatch::sublead::SubleadOutcome::Success,
    )
    .await
    .unwrap();
    pitboss_cli::dispatch::sublead::reconcile_terminated_sublead(
        &state,
        &s2_id,
        pitboss_cli::dispatch::sublead::SubleadOutcome::Success,
    )
    .await
    .unwrap();
}

// ── Spotlight #03: kill-cascade-drain ────────────────────────────────────────

/// Dogfood spotlight #03: depth-first cascade cancellation within the drain
/// grace window.
///
/// Scenario: an operator kicks off a long-running depth-2 dispatch — a root
/// lead with two active sub-leads (S1 for "phase 1", S2 for "phase 2"), each
/// sub-lead having two active workers. Partway through, the operator presses
/// cancel. Within the drain grace window, the cascade from root reaches every
/// sub-tree cancel token and every sub-tree worker cancel token.
///
/// This test proves:
///
/// 1. **Pre-cancel**: 2 sub-leads registered in `state.subleads`; each has 2
///    worker cancel tokens; none are draining.
/// 2. **Root cancel triggers cascade**: `state.root.cancel.drain()` wakes the
///    `install_cascade_cancel_watcher` task.
/// 3. **Drain window**: within 200 ms (more than sufficient for a tokio-local
///    task), every sub-tree cancel token and every worker cancel token reaches
///    the draining state.
/// 4. **Root is also draining**: the token that triggered the cascade.
///
/// ## In-process pattern
///
/// Same as spotlight #02: DispatchState + McpServer constructed in-process,
/// driven via FakeMcpClient. Worker cancel tokens are injected directly into
/// each sub-tree's `worker_cancels` map to simulate the state the cascade must
/// handle (Phase 4+ will wire real sub-tree workers).
#[tokio::test]
async fn dogfood_kill_cascade_drain() {
    use serde_json::json;

    // ── Scenario: operator cancels a deep dispatch mid-flight ──
    // Two sub-leads, each with two workers. Cancel is triggered at root;
    // depth-first drain reaches every worker within the grace window.

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // ── Install the cascade watcher (normally done by run_hierarchical) ──
    pitboss_cli::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    // ── Spawn two sub-leads via MCP ───────────────────────────────────────────
    let mut root = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();

    let s1_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "phase 1",
                "model": "claude-haiku-4-5",
                "budget_usd": 1.0,
                "max_workers": 2,
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
                "prompt": "phase 2",
                "model": "claude-haiku-4-5",
                "budget_usd": 1.0,
                "max_workers": 2,
            }),
        )
        .await
        .unwrap();
    let s2_id = s2_resp["sublead_id"]
        .as_str()
        .expect("spawn_sublead should return sublead_id for S2")
        .to_string();

    // ── Inject two workers into each sub-tree ─────────────────────────────────
    // Phase 4+ will have real sub-tree workers; for Phase 2/3 dogfood we
    // inject cancel tokens directly to simulate the state the cascade must
    // handle.
    for sublead_id in [&s1_id, &s2_id] {
        let subleads = state.subleads.read().await;
        let sub = subleads.get(sublead_id.as_str()).unwrap();
        let mut workers = sub.workers.write().await;
        let mut cancels = sub.worker_cancels.write().await;
        for worker_n in 0..2 {
            let worker_id = format!("{sublead_id}-w{worker_n}");
            workers.insert(
                worker_id.clone(),
                pitboss_cli::dispatch::state::WorkerState::Pending,
            );
            cancels.insert(worker_id, CancelToken::new());
        }
    }

    // ── OBSERVE pre-cancel state ──────────────────────────────────────────────
    {
        let subleads = state.subleads.read().await;
        assert_eq!(subleads.len(), 2, "both sub-leads should be registered");
        for (sublead_id, sub) in subleads.iter() {
            let worker_cancels = sub.worker_cancels.read().await;
            assert_eq!(
                worker_cancels.len(),
                2,
                "sub-lead {sublead_id} should have 2 worker cancel tokens pre-cancel"
            );
            for (wid, tok) in worker_cancels.iter() {
                assert!(
                    !tok.is_draining(),
                    "pre-cancel: worker token {wid} under {sublead_id} should not be draining"
                );
            }
            assert!(
                !sub.cancel.is_draining(),
                "pre-cancel: sub-tree {sublead_id} cancel should not be draining"
            );
        }
        assert!(
            !state.root.cancel.is_draining(),
            "pre-cancel: root cancel should not be draining"
        );
    }

    // ── ACT: operator cancels root ────────────────────────────────────────────
    state.root.cancel.drain();

    // Wait for the cascade watcher task to fire and propagate to all sub-trees.
    // The watcher runs on the tokio runtime; 200 ms is more than sufficient
    // for an in-process test. In production this would be bounded by the
    // TERMINATE_GRACE drain window.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // ── OBSERVE post-cancel state: cascade reached everything ─────────────────
    {
        let subleads = state.subleads.read().await;
        assert_eq!(
            subleads.len(),
            2,
            "sub-lead count should be unchanged after cancel"
        );
        for (sublead_id, sub) in subleads.iter() {
            assert!(
                sub.cancel.is_draining(),
                "cascade should have drained sub-tree {sublead_id}"
            );
            for (wid, tok) in sub.worker_cancels.read().await.iter() {
                assert!(
                    tok.is_draining(),
                    "cascade should have drained sub-tree worker {wid} under {sublead_id}"
                );
            }
        }
    }

    // Root itself must be draining — it is what triggered the cascade.
    assert!(
        state.root.cancel.is_draining(),
        "root cancel should be draining after operator cancel"
    );
}

#[tokio::test]
async fn dogfood_run_lease_contention() {
    use serde_json::json;

    // ── Scenario: two sub-leads compete for cross-tree resource (output.json)
    // S1 acquires first; S2 is blocked with S1 as holder. S1 releases; S2
    // acquires. Demonstrates the run_lease_* API for cross-tree coordination.

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // ── Spawn two sub-leads via MCP ───────────────────────────────────────────
    let mut root = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();

    let s1_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "sub-lead 1",
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
                "prompt": "sub-lead 2",
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

    // ── STEP 1: S1 acquires the lease ────────────────────────────────────────
    let mut s1_client = FakeMcpClient::connect_as(&socket, &s1_id, "sublead")
        .await
        .unwrap();
    let acq1 = s1_client
        .call_tool(
            "run_lease_acquire",
            json!({"key": "output.json", "ttl_secs": 60}),
        )
        .await;
    assert!(acq1.is_ok(), "S1 should acquire the lease");
    let acq1_resp = acq1.unwrap();
    assert_eq!(
        acq1_resp["acquired"], true,
        "S1 acquire should return acquired=true"
    );
    assert_eq!(
        acq1_resp["key"], "output.json",
        "S1 acquire should return the key"
    );
    assert_eq!(
        acq1_resp["holder"], s1_id,
        "S1 acquire should list S1 as holder"
    );

    // ── STEP 2: S2 tries to acquire the same lease — should be blocked ──────
    let mut s2_client = FakeMcpClient::connect_as(&socket, &s2_id, "sublead")
        .await
        .unwrap();
    let acq2 = s2_client
        .call_tool(
            "run_lease_acquire",
            json!({"key": "output.json", "ttl_secs": 60}),
        )
        .await;
    assert!(acq2.is_err(), "S2 should be blocked by S1's existing lease");
    let err = format!("{:?}", acq2.unwrap_err());
    assert!(
        err.contains(&s1_id),
        "error message should mention S1 as current holder; got: {err}"
    );

    // ── STEP 3: S1 releases the lease ────────────────────────────────────────
    let rel1 = s1_client
        .call_tool("run_lease_release", json!({"key": "output.json"}))
        .await;
    assert!(rel1.is_ok(), "S1 should release the lease successfully");
    let rel1_resp = rel1.unwrap();
    assert_eq!(
        rel1_resp["released"], true,
        "S1 release should return released=true"
    );

    // ── STEP 4: S2 retries and now acquires the lease ──────────────────────
    let acq3 = s2_client
        .call_tool(
            "run_lease_acquire",
            json!({"key": "output.json", "ttl_secs": 60}),
        )
        .await;
    assert!(
        acq3.is_ok(),
        "S2 should acquire the lease after S1 releases"
    );
    let acq3_resp = acq3.unwrap();
    assert_eq!(
        acq3_resp["acquired"], true,
        "S2 acquire should return acquired=true"
    );
    assert_eq!(
        acq3_resp["key"], "output.json",
        "S2 acquire should return the key"
    );
    assert_eq!(
        acq3_resp["holder"], s2_id,
        "S2 acquire should list S2 as holder (not S1)"
    );
}

#[tokio::test]
async fn dogfood_policy_auto_filter() {
    use pitboss_cli::mcp::policy::{ApprovalAction, ApprovalMatch, ApprovalRule, PolicyMatcher};
    use serde_json::json;

    // ── Scenario: Operator sets up policy rules to auto-approve routine
    // tool-use from S1 while blocking all plan-approvals, reducing operator
    // approval noise at depth=2 scale.
    //
    // Expected outcomes:
    // - S1's tool-use auto-approved silently (no queue entry)
    // - S2's same request enqueued (policy only matched S1)
    // - S1's plan approval queued (Rule 2 forces operator review)
    // - Only S2's tool-use and S1's plan blocked in queue (2 entries)

    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    // ── Spawn two sub-leads via MCP ──────────────────────────────────────
    let mut root = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();

    let s1_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "sub-lead 1 for routine work",
                "model": "claude-haiku-4-5",
                "budget_usd": 2.0,
                "max_workers": 2,
            }),
        )
        .await
        .unwrap();
    let s1_id = s1_resp["sublead_id"]
        .as_str()
        .expect("spawn_sublead must return sublead_id for S1")
        .to_string();

    let s2_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "sub-lead 2 for untrusted work",
                "model": "claude-haiku-4-5",
                "budget_usd": 2.0,
                "max_workers": 2,
            }),
        )
        .await
        .unwrap();
    let s2_id = s2_resp["sublead_id"]
        .as_str()
        .expect("spawn_sublead must return sublead_id for S2")
        .to_string();

    // ── Operator configures policy after spawn (so we have actual S1 id) ─
    // Rule 1: Auto-approve all tool-use from S1
    // Rule 2: Block all plan-category approvals (always require operator review)
    // (implicit Rule 3: everything else falls through to operator)
    let rules = vec![
        ApprovalRule {
            r#match: ApprovalMatch {
                actor: Some(format!("root→{}", s1_id)),
                category: Some(pitboss_cli::mcp::approval::ApprovalCategory::ToolUse),
                ..Default::default()
            },
            action: ApprovalAction::AutoApprove,
        },
        ApprovalRule {
            r#match: ApprovalMatch {
                category: Some(pitboss_cli::mcp::approval::ApprovalCategory::Plan),
                ..Default::default()
            },
            action: ApprovalAction::Block,
        },
    ];
    state
        .root
        .set_policy_matcher(PolicyMatcher::new(rules))
        .await;

    // ── ACT 1: S1 requests routine tool-use approval ──────────────────────
    // Expected: auto-approved by Rule 1, no queue entry
    let mut s1_client = FakeMcpClient::connect_as(&socket, &s1_id, "sublead")
        .await
        .unwrap();
    let s1_approval = s1_client
        .call_tool(
            "request_approval",
            json!({
                "summary": "S1: read config from shared storage",
                "timeout_secs": 2
            }),
        )
        .await
        .unwrap();

    assert_eq!(
        s1_approval["approved"], true,
        "S1 tool-use should be auto-approved by Rule 1"
    );
    assert_eq!(
        s1_approval["comment"], "auto-approved by policy",
        "S1 comment should indicate policy auto-approval"
    );

    // Verify no approval was queued for S1's tool-use.
    {
        let q = state.root.approval_queue.lock().await;
        assert_eq!(
            q.len(),
            0,
            "S1 auto-approved approval should not be enqueued; queue has {} items",
            q.len()
        );
    }

    // ── ACT 2: S2 requests same type of tool-use approval ───────────────
    // Expected: falls through to operator queue (no Rule 1 match for S2)
    // S2's request will block waiting for operator. Spawn it in a tokio task
    // so we can let it hang briefly while we verify the queue state.
    let socket_2 = socket.clone();
    let s2_id_clone = s2_id.clone();
    let s2_approval_handle = tokio::spawn(async move {
        let mut s2_client = FakeMcpClient::connect_as(&socket_2, &s2_id_clone, "sublead")
            .await
            .unwrap();
        s2_client
            .call_tool(
                "request_approval",
                json!({
                    "summary": "S2: read config from shared storage",
                    "timeout_secs": 60
                }),
            )
            .await
    });

    // Give S2's request time to land in the queue.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Verify S2's approval was queued (didn't match any auto-action rule).
    {
        let q = state.root.approval_queue.lock().await;
        assert_eq!(
            q.len(),
            1,
            "S2 tool-use should be enqueued (no Rule 1 match for S2); queue has {} items",
            q.len()
        );
    }

    // Clean up the hanging S2 request before moving to the next step.
    s2_approval_handle.abort();

    // ── ACT 3: S1 requests plan approval ─────────────────────────────────
    // Expected: blocked by Rule 2 (all Plan approvals require operator), queued
    let socket_3 = socket.clone();
    let s1_id_clone = s1_id.clone();
    let plan_req_handle = tokio::spawn(async move {
        let mut s1_client = FakeMcpClient::connect_as(&socket_3, &s1_id_clone, "sublead")
            .await
            .unwrap();
        s1_client
            .call_tool(
                "propose_plan",
                json!({
                    "plan": {
                        "summary": "S1: deploy phase 2 artifacts",
                        "rationale": "phase 1 complete, ready for next stage",
                        "resources": ["2 workers"],
                        "risks": ["data corruption if oversized writes"],
                        "rollback": "restore from backup"
                    },
                    "timeout_secs": 60
                }),
            )
            .await
    });

    // Give S1's plan approval time to land in the queue.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Verify S1's plan approval was queued (Rule 2 forces operator review).
    {
        let q = state.root.approval_queue.lock().await;
        assert_eq!(
            q.len(),
            2,
            "S1 plan approval should be enqueued + S2 tool-use already queued; queue has {} items",
            q.len()
        );
    }

    // Clean up the hanging plan approval.
    plan_req_handle.abort();
}

// ── Spotlight #06: Envelope cap rejection ──────────────────────────────────────
//
// Final fake-claude spotlight. Demonstrates manifest-level budget cap enforcement
// with clean rejection semantics. Root lead attempts to spawn a sub-lead with
// budget exceeding max_sublead_budget_usd; request is rejected with a clear error
// before any state mutation happens.
//
// Scenario:
// 1. Operator sets max_sublead_budget_usd = 3.0 as a safety rail
// 2. Root attempts to spawn sub-lead with budget_usd = 5.0 → rejected, no state change
// 3. Root retries with budget_usd = 2.0 → succeeds, sub-lead registered
//
// Expected outcomes:
// - Rejected spawn: no LayerState, no reservation, error message mentions cap
// - State after rejection: subleads.is_empty() && reserved_usd == 0.0
// - Successful retry: sub-lead registered, $2.0 reserved
#[tokio::test]
async fn dogfood_envelope_cap_rejection() {
    use serde_json::json;

    // Build state with max_sublead_budget_usd = 3.0 baked into the manifest.
    // We use mk_state_with_sublead_budget_cap from sublead_flows.rs helper.
    let (_dir, state) = {
        let dir = tempfile::TempDir::new().unwrap();
        let lead = ResolvedLead {
            id: "root".into(),
            directory: std::path::PathBuf::from("/tmp"),
            prompt: "root with cap enforcement".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: pitboss_cli::manifest::schema::Effort::High,
            tools: vec![],
            timeout_secs: 3600,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            allow_subleads: true,
            max_subleads: None,
            max_sublead_budget_usd: Some(3.0), // ← Cap: max $3 per sub-lead
            max_workers_across_tree: None,
            sublead_defaults: None,
        };
        let manifest = ResolvedManifest {
            max_parallel: 8,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: pitboss_cli::manifest::schema::WorktreeCleanup::OnSuccess,
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
            std::path::PathBuf::from("claude"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(pitboss_cli::shared_store::SharedStore::new()),
        ));
        (dir, state)
    };

    let socket = socket_path_for_run(state.root.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut root = FakeMcpClient::connect_as(&socket, "root", "root_lead")
        .await
        .unwrap();

    // ── Act 1: Root attempts to spawn sub-lead with budget_usd = 5.0 ────────
    // Expected: rejected with "exceeds per-sublead cap" error, no state change
    eprintln!("Act 1: Attempting spawn with budget_usd=5.0 (exceeds cap of 3.0)");
    let rejected_result = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "test sub-lead with excessive budget",
                "model": "claude-haiku-4-5",
                "budget_usd": 5.0,
                "max_workers": 2
            }),
        )
        .await;

    // Verify the call failed
    assert!(
        rejected_result.is_err(),
        "spawn_sublead with budget_usd=5.0 should fail when max_sublead_budget_usd=3.0"
    );
    let err_msg = format!("{:?}", rejected_result.unwrap_err());
    assert!(
        err_msg.contains("exceeds per-sublead cap"),
        "error message should mention the cap; got: {err_msg}"
    );

    // Verify no partial state was registered
    {
        let subleads = state.subleads.read().await;
        assert!(
            subleads.is_empty(),
            "after rejected spawn, subleads should be empty; got: {} entries",
            subleads.len()
        );
    }

    // Verify no budget reservation was made
    {
        let reserved = *state.root.reserved_usd.lock().await;
        assert_eq!(
            reserved, 0.0,
            "after rejected spawn, reserved_usd should be 0.0; got: {reserved}"
        );
    }

    // ── Act 2: Root retries with budget_usd = 2.0 ────────────────────────
    // Expected: succeeds, sub-lead registered, $2.0 reserved
    eprintln!("Act 2: Retrying spawn with budget_usd=2.0 (within cap of 3.0)");
    let success_resp = root
        .call_tool(
            "spawn_sublead",
            json!({
                "prompt": "test sub-lead with compliant budget",
                "model": "claude-haiku-4-5",
                "budget_usd": 2.0,
                "max_workers": 2
            }),
        )
        .await
        .expect("spawn_sublead with budget_usd=2.0 should succeed when max_sublead_budget_usd=3.0");

    // Verify the response includes sublead_id
    let sublead_id = success_resp["sublead_id"]
        .as_str()
        .expect("response should have sublead_id field")
        .to_string();
    assert!(
        sublead_id.starts_with("sublead-"),
        "sublead_id should start with 'sublead-'; got: {sublead_id}"
    );

    // Verify the sub-tree LayerState IS now registered
    {
        let subleads = state.subleads.read().await;
        assert!(
            subleads.contains_key(&sublead_id),
            "after successful spawn, subleads should contain the new sublead_id: {sublead_id}"
        );
    }

    // Verify budget IS reserved
    {
        let reserved = *state.root.reserved_usd.lock().await;
        assert!(
            (reserved - 2.0).abs() < 1e-9,
            "after successful spawn with budget_usd=2.0, reserved_usd should be 2.0; got: {reserved}"
        );
    }

    eprintln!("Spotlight #06 passed: cap enforcement clean rejection + successful retry");
}
