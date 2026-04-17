//! Integration tests for v0.3 hierarchical orchestration. These drive the
//! shire MCP server as if we were a lead claude subprocess, using fake-mcp-client.

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use fake_mcp_client::FakeMcpClient;
use mosaic_core::session::CancelToken;
use mosaic_core::store::{JsonFileStore, SessionStore};
use shire_cli::dispatch::state::DispatchState;
use shire_cli::manifest::resolve::ResolvedManifest;
use shire_cli::manifest::schema::WorktreeCleanup;
use shire_cli::mcp::{socket_path_for_run, McpServer};
use uuid::Uuid;

fn mk_state() -> (TempDir, Arc<DispatchState>) {
    let dir = TempDir::new().unwrap();
    let manifest = ResolvedManifest {
        max_parallel: 4,
        halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![],
        lead: None,
        max_workers: Some(4),
        budget_usd: Some(5.0),
        lead_timeout_secs: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let run_id = Uuid::now_v7();
    let state = Arc::new(DispatchState::new(
        run_id,
        manifest,
        store,
        CancelToken::new(),
        "lead".into(),
    ));
    (dir, state)
}

#[tokio::test]
async fn mcp_spawn_and_list_round_trip() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone())
        .await
        .unwrap();

    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let spawn_result = client
        .call_tool(
            "spawn_worker",
            json!({
                "prompt": "investigate issue #1"
            }),
        )
        .await
        .unwrap();
    let task_id = spawn_result["task_id"].as_str().unwrap().to_string();
    assert!(task_id.starts_with("worker-"));

    let list_result = client.call_tool("list_workers", json!({})).await.unwrap();
    let list = list_result.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["task_id"].as_str().unwrap(), task_id);

    client.close().await.unwrap();
}
