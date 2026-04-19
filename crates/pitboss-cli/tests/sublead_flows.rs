//! Integration tests for v0.6 depth-2 sub-leads. Driven through the
//! pitboss MCP server using fake-mcp-client, mirroring the
//! hierarchical_flows.rs pattern.

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;

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

/// Same shape as hierarchical_flows::mk_state but with allow_subleads
/// enabled on the lead. Used by every test in this file.
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

#[tokio::test]
async fn spawn_sublead_tool_is_exposed_to_root() {
    let (_dir, state) = mk_state_with_subleads();
    let socket = socket_path_for_run(state.run_id, &state.root.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert!(
        tools.iter().any(|t| t.name == "spawn_sublead"),
        "spawn_sublead should be in the root lead's MCP toolset"
    );
}
