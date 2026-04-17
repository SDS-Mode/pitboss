//! Hierarchical dispatch path — one lead subprocess plus dynamically-spawned
//! workers. Reuses most of the flat dispatch plumbing from runner.rs and
//! adds the MCP server lifecycle + lead spawn wiring on top.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use mosaic_core::process::{ProcessSpawner, TokioSpawner};
use mosaic_core::session::CancelToken;
use mosaic_core::store::{JsonFileStore, RunMeta, RunSummary, SessionStore};
use uuid::Uuid;

use crate::dispatch::state::DispatchState;
use crate::manifest::resolve::ResolvedManifest;
use crate::mcp::{socket_path_for_run, McpServer};

#[allow(clippy::too_many_arguments)]
pub async fn run_hierarchical(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32> {
    let run_id = Uuid::now_v7();
    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();

    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), &manifest_text).await?;
    if let Ok(b) = serde_json::to_vec_pretty(&resolved) {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }

    let lead = resolved
        .lead
        .as_ref()
        .context("hierarchical mode requires a [[lead]]")?;

    if dry_run {
        println!("DRY-RUN lead: {}", lead.id);
        println!(
            "DRY-RUN command: {} --verbose (mcp socket TBD)",
            claude_binary.display()
        );
        return Ok(0);
    }

    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.clone()));
    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.clone(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at: Utc::now(),
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    let cancel = CancelToken::new();
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());

    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());

    // 1. Start the MCP server.
    let socket = socket_path_for_run(run_id, &run_dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        resolved.clone(),
        store.clone(),
        cancel.clone(),
        lead.id.clone(),
    ));
    let _mcp = McpServer::start(socket.clone(), state.clone()).await?;

    // 2. Build the --mcp-config file for the lead.
    let mcp_config_path = run_subdir.join("lead-mcp-config.json");
    write_mcp_config(&mcp_config_path, &socket).await?;

    // 3. Spawn the lead.
    //    (Actual spawn wiring in Task 20; for now we log and return Ok(0).)
    let _ = (spawner, lead);
    tracing::info!(run_id = %run_id, "hierarchical run scaffolded; lead spawn wired in Task 20");

    // 4. Finalize.
    let started_at = meta.started_at;
    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path,
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at,
        ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total: 0,
        tasks_failed: 0,
        was_interrupted: false,
        tasks: vec![],
    };
    store.finalize_run(&summary).await?;
    Ok(0)
}

async fn write_mcp_config(path: &std::path::Path, socket: &std::path::Path) -> Result<()> {
    let cfg = serde_json::json!({
        "mcpServers": {
            "shire": {
                "command": "shire-mcp-stub",
                "args": [],
                "transport": { "type": "unix", "path": socket.to_string_lossy() }
            }
        }
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}
