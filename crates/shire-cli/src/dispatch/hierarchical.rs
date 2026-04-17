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

    // 3. Prepare lead worktree + spawn.
    let wt_mgr = Arc::new(mosaic_core::worktree::WorktreeManager::new());
    let mut lead_worktree_handle: Option<mosaic_core::worktree::Worktree> = None;
    let lead_cwd = if lead.use_worktree {
        let name = format!("shire-lead-{}-{}", lead.id, run_id);
        match wt_mgr.prepare(&lead.directory, &name, lead.branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                lead_worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                anyhow::bail!("lead worktree prepare failed: {e}");
            }
        }
    } else {
        lead.directory.clone()
    };

    let lead_task_dir = run_subdir.join("tasks").join(&lead.id);
    tokio::fs::create_dir_all(&lead_task_dir).await.ok();
    let lead_log_path = lead_task_dir.join("stdout.log");
    let lead_stderr_path = lead_task_dir.join("stderr.log");

    let spawn_cmd = mosaic_core::process::SpawnCmd {
        program: claude_binary.clone(),
        args: crate::dispatch::runner::lead_spawn_args(lead, &mcp_config_path),
        cwd: lead_cwd.clone(),
        env: lead.env.clone(),
    };

    state.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Running {
            started_at: Utc::now(),
        },
    );

    let outcome = mosaic_core::session::SessionHandle::new(lead.id.clone(), spawner, spawn_cmd)
        .with_log_path(lead_log_path.clone())
        .with_stderr_log_path(lead_stderr_path)
        .run_to_completion(
            cancel.clone(),
            std::time::Duration::from_secs(lead.timeout_secs),
        )
        .await;

    // Build lead TaskRecord
    let lead_record = mosaic_core::store::TaskRecord {
        task_id: lead.id.clone(),
        status: match outcome.final_state {
            mosaic_core::session::SessionState::Completed => {
                mosaic_core::store::TaskStatus::Success
            }
            mosaic_core::session::SessionState::Failed { .. } => {
                mosaic_core::store::TaskStatus::Failed
            }
            mosaic_core::session::SessionState::TimedOut => {
                mosaic_core::store::TaskStatus::TimedOut
            }
            mosaic_core::session::SessionState::Cancelled => {
                mosaic_core::store::TaskStatus::Cancelled
            }
            mosaic_core::session::SessionState::SpawnFailed { .. } => {
                mosaic_core::store::TaskStatus::SpawnFailed
            }
            _ => mosaic_core::store::TaskStatus::Failed,
        },
        exit_code: outcome.exit_code,
        started_at: outcome.started_at,
        ended_at: outcome.ended_at,
        duration_ms: outcome.duration_ms(),
        worktree_path: if lead.use_worktree {
            Some(lead_cwd)
        } else {
            None
        },
        log_path: lead_log_path,
        token_usage: outcome.token_usage,
        claude_session_id: outcome.claude_session_id,
        final_message_preview: outcome.final_message_preview,
        parent_task_id: None, // lead has no parent
    };

    // Cleanup worktree per policy
    if let Some(wt) = lead_worktree_handle {
        let succeeded = matches!(lead_record.status, mosaic_core::store::TaskStatus::Success);
        let cleanup = match resolved.worktree_cleanup {
            crate::manifest::schema::WorktreeCleanup::Always => {
                mosaic_core::worktree::CleanupPolicy::Always
            }
            crate::manifest::schema::WorktreeCleanup::OnSuccess => {
                mosaic_core::worktree::CleanupPolicy::OnSuccess
            }
            crate::manifest::schema::WorktreeCleanup::Never => {
                mosaic_core::worktree::CleanupPolicy::Never
            }
        };
        let _ = wt_mgr.cleanup(wt, cleanup, succeeded);
    }

    // Persist lead record
    store.append_record(run_id, &lead_record).await?;
    state.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Done(lead_record.clone()),
    );
    let _ = state.done_tx.send(lead.id.clone());

    // 4. Finalize.
    let lead_failed = !matches!(lead_record.status, mosaic_core::store::TaskStatus::Success);
    let started_at = meta.started_at;
    let ended_at = Utc::now();
    let tasks = vec![lead_record];
    let tasks_failed = tasks
        .iter()
        .filter(|r| !matches!(r.status, mosaic_core::store::TaskStatus::Success))
        .count();
    let summary = RunSummary {
        run_id,
        manifest_path,
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at,
        ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total: tasks.len(),
        tasks_failed,
        was_interrupted: cancel.is_draining() || cancel.is_terminated(),
        tasks,
    };
    store.finalize_run(&summary).await?;

    let rc = if cancel.is_terminated() {
        130
    } else if lead_failed {
        1
    } else {
        0
    };
    Ok(rc)
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
