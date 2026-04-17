//! Hierarchical dispatch path — one lead subprocess plus dynamically-spawned
//! workers. Reuses most of the flat dispatch plumbing from runner.rs and
//! adds the MCP server lifecycle + lead spawn wiring on top.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, RunMeta, RunSummary, SessionStore};
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
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at: Utc::now(),
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    let cancel = CancelToken::new();
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());

    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());

    // Moved up from later: WorktreeManager and cleanup policy are now needed
    // by DispatchState so the MCP spawn_worker handler can prepare worktrees.
    let wt_mgr = Arc::new(pitboss_core::worktree::WorktreeManager::new());
    let cleanup_policy = match resolved.worktree_cleanup {
        crate::manifest::schema::WorktreeCleanup::Always => {
            pitboss_core::worktree::CleanupPolicy::Always
        }
        crate::manifest::schema::WorktreeCleanup::OnSuccess => {
            pitboss_core::worktree::CleanupPolicy::OnSuccess
        }
        crate::manifest::schema::WorktreeCleanup::Never => {
            pitboss_core::worktree::CleanupPolicy::Never
        }
    };

    // 1. Start the MCP server.
    let socket = socket_path_for_run(run_id, &run_dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        resolved.clone(),
        store.clone(),
        cancel.clone(),
        lead.id.clone(),
        spawner.clone(),
        claude_binary.clone(),
        wt_mgr.clone(),
        cleanup_policy,
        run_subdir.clone(),
    ));
    let _mcp = McpServer::start(socket.clone(), state.clone()).await?;

    // 2. Build the --mcp-config file for the lead.
    let mcp_config_path = run_subdir.join("lead-mcp-config.json");
    write_mcp_config(&mcp_config_path, &socket).await?;

    // 3. Prepare lead worktree + spawn.
    let mut lead_worktree_handle: Option<pitboss_core::worktree::Worktree> = None;
    let lead_cwd = if lead.use_worktree {
        let name = format!("pitboss-lead-{}-{}", lead.id, run_id);
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

    let spawn_cmd = pitboss_core::process::SpawnCmd {
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

    let outcome = pitboss_core::session::SessionHandle::new(lead.id.clone(), spawner, spawn_cmd)
        .with_log_path(lead_log_path.clone())
        .with_stderr_log_path(lead_stderr_path)
        .run_to_completion(
            cancel.clone(),
            std::time::Duration::from_secs(lead.timeout_secs),
        )
        .await;

    // Build lead TaskRecord
    let lead_record = pitboss_core::store::TaskRecord {
        task_id: lead.id.clone(),
        status: match outcome.final_state {
            pitboss_core::session::SessionState::Completed => {
                pitboss_core::store::TaskStatus::Success
            }
            pitboss_core::session::SessionState::Failed { .. } => {
                pitboss_core::store::TaskStatus::Failed
            }
            pitboss_core::session::SessionState::TimedOut => {
                pitboss_core::store::TaskStatus::TimedOut
            }
            pitboss_core::session::SessionState::Cancelled => {
                pitboss_core::store::TaskStatus::Cancelled
            }
            pitboss_core::session::SessionState::SpawnFailed { .. } => {
                pitboss_core::store::TaskStatus::SpawnFailed
            }
            _ => pitboss_core::store::TaskStatus::Failed,
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
        let succeeded = matches!(lead_record.status, pitboss_core::store::TaskStatus::Success);
        let _ = wt_mgr.cleanup(wt, cleanup_policy, succeeded);
    }

    // Persist lead record
    store.append_record(run_id, &lead_record).await?;
    state.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Done(lead_record.clone()),
    );
    let _ = state.done_tx.send(lead.id.clone());

    // 4. Finalize.
    // Capture the ORIGINAL cancel state BEFORE we call terminate() below.
    // This preserves the distinction between user Ctrl-C (real interruption)
    // and our internal cleanup termination signal to drain workers.
    let was_interrupted = cancel.is_draining() || cancel.is_terminated();

    // Any in-flight workers get cancelled, their TaskRecord synthesized.
    // `cancel.terminate()` signals all in-flight SessionHandles.
    cancel.terminate();
    // Give them up to TERMINATE_GRACE to drain.
    tokio::time::sleep(pitboss_core::session::TERMINATE_GRACE).await;

    let worker_records: Vec<pitboss_core::store::TaskRecord> = {
        let workers = state.workers.read().await;
        workers
            .iter()
            .filter(|(id, _)| *id != &lead.id) // don't double-count the lead
            .map(|(id, w)| match w {
                crate::dispatch::state::WorkerState::Done(rec) => rec.clone(),
                crate::dispatch::state::WorkerState::Pending
                | crate::dispatch::state::WorkerState::Running { .. } => {
                    let now = Utc::now();
                    pitboss_core::store::TaskRecord {
                        task_id: id.clone(),
                        status: pitboss_core::store::TaskStatus::Cancelled,
                        exit_code: None,
                        started_at: now,
                        ended_at: now,
                        duration_ms: 0,
                        worktree_path: None,
                        log_path: run_subdir.join("tasks").join(id).join("stdout.log"),
                        token_usage: Default::default(),
                        claude_session_id: None,
                        final_message_preview: Some("cancelled when lead exited".into()),
                        parent_task_id: Some(lead.id.clone()),
                    }
                }
            })
            .collect()
    };

    for rec in &worker_records {
        store.append_record(run_id, rec).await?;
    }

    // Assemble final summary with lead + workers.
    let mut all_records = vec![lead_record.clone()];
    all_records.extend(worker_records);

    let tasks_failed = all_records
        .iter()
        .filter(|r| !matches!(r.status, pitboss_core::store::TaskStatus::Success))
        .count();

    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path,
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at: meta.started_at,
        ended_at,
        total_duration_ms: (ended_at - meta.started_at).num_milliseconds(),
        tasks_total: all_records.len(),
        tasks_failed,
        was_interrupted,
        tasks: all_records,
    };
    store.finalize_run(&summary).await?;

    // Exit code same as flat dispatch
    let rc = if was_interrupted {
        130
    } else if tasks_failed > 0 {
        1
    } else {
        0
    };
    Ok(rc)
}

/// Emit a `--mcp-config` file that tells claude to launch our own pitboss
/// binary as a stdio MCP server, passing the socket path as an argument.
/// `pitboss mcp-bridge <socket>` then proxies bytes between claude's stdio
/// pair and the pitboss MCP server's unix socket.
///
/// This avoids relying on a non-standard `transport: { type: "unix", ... }`
/// field that claude's MCP client may not honor. The generated config uses
/// only the documented `command` + `args` (stdio transport) shape.
async fn write_mcp_config(path: &std::path::Path, socket: &std::path::Path) -> Result<()> {
    // Find the pitboss binary path (the one running us now) so the lead can
    // re-exec the same build for the bridge subcommand.
    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;

    let cfg = serde_json::json!({
        "mcpServers": {
            "pitboss": {
                "command": pitboss_exe.to_string_lossy(),
                "args": ["mcp-bridge", socket.to_string_lossy()],
            }
        }
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}
