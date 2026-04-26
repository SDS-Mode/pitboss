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

use crate::control::{control_socket_path, server::start_control_server};
use crate::dispatch::state::DispatchState;
use crate::manifest::resolve::ResolvedManifest;
use crate::mcp::{socket_path_for_run, McpServer};

// `sublead_sessions`: prior sub-lead session IDs from `subleads.jsonl`,
// read by `build_resume_hierarchical`. Empty for fresh dispatches. When
// non-empty, seeded into the root shared store at `/resume/subleads` so the
// root lead can pass `resume_session_id` to `spawn_sublead`.
#[allow(clippy::too_many_arguments)]
pub async fn run_hierarchical(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
    sublead_sessions: std::collections::HashMap<String, String>,
    pre_minted_run_id: Option<Uuid>,
) -> Result<i32> {
    // Surface headless approval-gate mis-configurations early, before any
    // claude subprocess launches. Runs silently if stdout is a TTY (the
    // operator has the TUI to approve things).
    crate::dispatch::runner::print_headless_warnings_if_applicable(&resolved);

    // Snapshot the inherited PITBOSS_RUN_ID (the parent orchestrator's run id,
    // when one exists) BEFORE we overwrite it with our own. See `notify::parent`
    // for the env-var contract introduced for issue #133.
    let parent_run_id = crate::notify::parent::parent_run_id();
    // Honor a pre-minted id from `--background` (issue #133-C); otherwise
    // mint fresh.
    let run_id = pre_minted_run_id.unwrap_or_else(Uuid::now_v7);
    crate::notify::parent::set_run_id_env(&run_id.to_string());

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

    // Build notification router from manifest [[notification]] sections AND
    // the optional `PITBOSS_PARENT_NOTIFY_URL` env var. See notify::parent
    // for the parent-orchestrator hook contract (issue #133).
    let http = std::sync::Arc::new(reqwest::Client::new());
    let notification_router = crate::notify::parent::build_router(&resolved.notifications, &http)?;

    // Fire RunDispatched up front so a parent orchestrator can register the
    // run before any tokens spend.
    if let Some(router) = &notification_router {
        let env = crate::notify::NotificationEnvelope::new(
            &run_id.to_string(),
            crate::notify::Severity::Info,
            crate::notify::PitbossEvent::RunDispatched {
                run_id: run_id.to_string(),
                parent_run_id: parent_run_id.clone(),
                manifest_path: manifest_path.display().to_string(),
                mode: "hierarchical".to_string(),
                survive_parent: resolved
                    .lifecycle
                    .as_ref()
                    .is_some_and(|l| l.survive_parent),
            },
            Utc::now(),
        );
        let _ = router.dispatch(env).await;
    }

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
        resolved.default_approval_policy.unwrap_or_default(),
        notification_router.clone(),
        {
            let s = std::sync::Arc::new(crate::shared_store::SharedStore::new());
            s.start_lease_pruner();
            s
        },
    ));
    let _mcp = McpServer::start(socket.clone(), state.clone()).await?;

    // Seed /resume/subleads with prior sub-lead session IDs so the root lead
    // can read them on a resume run and pass resume_session_id to spawn_sublead.
    // Skipped on fresh dispatches (sublead_sessions is empty).
    if !sublead_sessions.is_empty() {
        let value = serde_json::to_vec(&sublead_sessions).unwrap_or_default();
        if let Err(e) = state
            .root
            .shared_store
            .set("/resume/subleads", value, "pitboss")
            .await
        {
            tracing::warn!(error = %e, "failed to seed /resume/subleads in shared store");
        }
    }

    // Load declarative approval policy from the manifest into the root layer.
    // Empty approval_rules → no PolicyMatcher is installed (legacy path unchanged).
    if !resolved.approval_rules.is_empty() {
        let matcher = crate::mcp::policy::PolicyMatcher::new(resolved.approval_rules.clone());
        state.root.set_policy_matcher(matcher).await;
    }

    // Install cascade cancel watcher: when root drains, all registered
    // sub-trees and their workers are drained automatically.
    crate::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    // Install approval TTL watcher: scan the approval queue every 250ms
    // and apply fallback actions (auto_reject, auto_approve) to expired entries.
    crate::dispatch::runner::install_approval_ttl_watcher(state.clone());

    // Bind the control socket for TUI ↔ dispatcher ops.
    let control_sock = control_socket_path(run_id, &run_dir);
    let _control = start_control_server(
        control_sock,
        env!("CARGO_PKG_VERSION").to_string(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .context("start control server")?;

    // 2. Build the --mcp-config file for the lead.
    //
    // Mint a per-actor token for the root lead and embed it in the
    // bridge args. The server uses the token to bind connection identity
    // — defending against direct (non-bridge) socket connections that
    // try to forge `_meta.actor_role: root_lead`. Issue #145.
    let lead_token = state.mint_token(&lead.id, "lead").await;
    let mcp_config_path = run_subdir.join("lead-mcp-config.json");
    write_mcp_config(
        &mcp_config_path,
        &socket,
        &lead.id,
        "lead",
        Some(&lead_token),
        &resolved.mcp_servers,
    )
    .await?;

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

    // Persist the lead's worktree path so the TUI can run mid-flight
    // git-diff against it (same pattern as workers — see mcp/tools.rs).
    if lead.use_worktree {
        let _ = tokio::fs::write(
            lead_task_dir.join("worktree.path"),
            lead_cwd.to_string_lossy().as_bytes(),
        )
        .await;
    }

    // 3b. Wire the reprompt delivery channel for the root lead BEFORE spawning.
    //     `send_synthetic_reprompt` will find this channel when a worker is
    //     killed with reason and the root layer is the target. The receiving
    //     end is consumed by the kill+resume loop below.
    let (reprompt_tx, mut reprompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    state.root.set_reprompt_tx(reprompt_tx).await;

    // 3c. Kill+resume loop — identical in structure to spawn_sublead_session.
    //
    //     When no reprompts are queued the loop runs exactly once, producing
    //     identical behaviour to the v0.5 single-shot. The extra state
    //     (last_session_id, overall_started_at, total_token_usage) adds no
    //     overhead on the common path.
    let mut lead_env = lead.env.clone();
    crate::dispatch::runner::apply_pitboss_env_defaults(&mut lead_env, lead.permission_routing);
    let initial_cmd = pitboss_core::process::SpawnCmd {
        program: claude_binary.clone(),
        args: crate::dispatch::runner::lead_spawn_args(lead, &mcp_config_path),
        cwd: lead_cwd.clone(),
        env: lead_env,
    };

    state.root.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Running {
            started_at: Utc::now(),
            session_id: None,
        },
    );

    let overall_started_at = Utc::now();
    let mut last_session_id: Option<String> = None;
    let mut total_token_usage = pitboss_core::parser::TokenUsage::default();
    let mut reprompt_count: u32 = 0;
    let mut current_cmd = initial_cmd;

    let final_outcome = loop {
        // Per-iteration session_id capture channel.
        let (session_id_tx, mut session_id_rx) = tokio::sync::mpsc::channel::<String>(1);

        // Per-iteration cancel token: forwards termination from the run-level
        // cancel token. This lets operator Ctrl-C still reach the subprocess
        // while still allowing the reprompt path to kill+restart without
        // terminating the whole run.
        let proc_cancel = pitboss_core::session::CancelToken::new();
        {
            let run_cancel = cancel.clone();
            let proc = proc_cancel.clone();
            tokio::spawn(async move {
                run_cancel.await_terminate().await;
                proc.terminate();
            });
        }

        let outcome = pitboss_core::session::SessionHandle::new(
            lead.id.clone(),
            spawner.clone(),
            current_cmd.clone(),
        )
        .with_log_path(lead_log_path.clone())
        .with_stderr_log_path(lead_stderr_path.clone())
        .with_session_id_tx(session_id_tx)
        .run_to_completion(
            proc_cancel,
            std::time::Duration::from_secs(lead.timeout_secs),
        )
        .await;

        // Capture session_id from either the mid-run channel or the final result.
        if let Ok(sid) = session_id_rx.try_recv() {
            state.root.workers.write().await.insert(
                lead.id.clone(),
                crate::dispatch::state::WorkerState::Running {
                    started_at: overall_started_at,
                    session_id: Some(sid.clone()),
                },
            );
            last_session_id = Some(sid);
        } else if let Some(ref sid) = outcome.claude_session_id {
            last_session_id = Some(sid.clone());
        }

        // Accumulate token usage across iterations.
        total_token_usage.add(&outcome.token_usage);

        // Check for a pending reprompt.
        let pending_reprompt = reprompt_rx.try_recv().ok();
        if let Some(new_prompt) = pending_reprompt {
            if let Some(ref sid) = last_session_id {
                tracing::info!(
                    lead_id = %lead.id,
                    session_id = %sid,
                    "root-lead kill+resume: new synthetic reprompt received"
                );
                reprompt_count += 1;
                let resume_args = crate::dispatch::runner::lead_resume_spawn_args(
                    lead,
                    &mcp_config_path,
                    sid,
                    &new_prompt,
                );
                let mut resume_env = lead.env.clone();
                crate::dispatch::runner::apply_pitboss_env_defaults(
                    &mut resume_env,
                    lead.permission_routing,
                );
                current_cmd = pitboss_core::process::SpawnCmd {
                    program: claude_binary.clone(),
                    args: resume_args,
                    cwd: lead_cwd.clone(),
                    env: resume_env,
                };
                // Reset the workers map entry to Running (session_id TBD).
                state.root.workers.write().await.insert(
                    lead.id.clone(),
                    crate::dispatch::state::WorkerState::Running {
                        started_at: overall_started_at,
                        session_id: None,
                    },
                );
                continue;
            } else {
                tracing::warn!(
                    lead_id = %lead.id,
                    "root-lead kill+resume: reprompt arrived but no session_id captured; \
                     treating as normal termination"
                );
                break outcome;
            }
        }

        // No pending reprompt — subprocess reached terminal state normally.
        break outcome;
    };

    // Close the reprompt channel so further sends fail fast.
    state.root.clear_reprompt_tx().await;

    // Build lead TaskRecord using the accumulated data from all iterations.
    let lead_counters = state
        .root
        .worker_counters
        .read()
        .await
        .get(&state.root.lead_id)
        .cloned()
        .unwrap_or_default();
    // Merge the loop's own reprompt_count into the counter-based one.
    let total_reprompt_count = lead_counters.reprompt_count + reprompt_count;
    // Compute initial status from session state, then reclassify if the
    // root lead exited shortly after a rejected approval (lessons-learned:
    // require_plan_approval = true + auto_reject by policy → lead exits 0
    // → would otherwise be Success). See `mcp::tools::run_worker` for the
    // same pattern.
    let mut lead_status = match final_outcome.final_state {
        pitboss_core::session::SessionState::Completed => pitboss_core::store::TaskStatus::Success,
        pitboss_core::session::SessionState::Failed { .. } => {
            pitboss_core::store::TaskStatus::Failed
        }
        pitboss_core::session::SessionState::TimedOut => pitboss_core::store::TaskStatus::TimedOut,
        pitboss_core::session::SessionState::Cancelled => {
            pitboss_core::store::TaskStatus::Cancelled
        }
        pitboss_core::session::SessionState::SpawnFailed { .. } => {
            pitboss_core::store::TaskStatus::SpawnFailed
        }
        _ => pitboss_core::store::TaskStatus::Failed,
    };
    if matches!(lead_status, pitboss_core::store::TaskStatus::Success) {
        if let Some(crate::dispatch::state::ApprovalTerminationKind::Rejected) =
            state.approval_driven_termination(&lead.id).await
        {
            lead_status = pitboss_core::store::TaskStatus::ApprovalRejected;
        }
    }
    let lead_record = pitboss_core::store::TaskRecord {
        task_id: lead.id.clone(),
        status: lead_status,
        exit_code: final_outcome.exit_code,
        started_at: overall_started_at,
        ended_at: final_outcome.ended_at,
        duration_ms: (final_outcome.ended_at - overall_started_at)
            .num_milliseconds()
            .max(0),
        worktree_path: if lead.use_worktree {
            Some(lead_cwd)
        } else {
            None
        },
        log_path: lead_log_path.clone(),
        token_usage: total_token_usage,
        claude_session_id: final_outcome.claude_session_id,
        final_message_preview: final_outcome.final_message_preview,
        final_message: final_outcome.final_message,
        parent_task_id: None, // lead has no parent
        pause_count: lead_counters.pause_count,
        reprompt_count: total_reprompt_count,
        approvals_requested: lead_counters.approvals_requested,
        approvals_approved: lead_counters.approvals_approved,
        approvals_rejected: lead_counters.approvals_rejected,
        model: Some(lead.model.clone()),
        failure_reason: crate::dispatch::failure_detection::detect_failure_reason(
            final_outcome.exit_code,
            Some(&lead_log_path),
            Some(&lead_stderr_path),
        ),
    };

    // Cleanup worktree per policy
    if let Some(wt) = lead_worktree_handle {
        let succeeded = matches!(lead_record.status, pitboss_core::store::TaskStatus::Success);
        let _ = wt_mgr.cleanup(wt, cleanup_policy, succeeded);
    }

    // Persist lead record
    store.append_record(run_id, &lead_record).await?;
    // Broadcast classified failure so the TUI and any attached client see
    // why the root lead died (rate-limit / network / auth / ...).
    // Root-lead dying generally ends the run, but still record into
    // api_health for completeness — any subleads spawned after this point
    // (rare) will see the gate.
    if let Some(reason) = lead_record.failure_reason.clone() {
        state.api_health.record(&reason).await;
        crate::dispatch::failure_detection::broadcast_worker_failed(
            &state.root,
            lead.id.clone(),
            None,
            reason,
            &["root", lead.id.as_str()],
        )
        .await;
    }
    state.root.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Done(lead_record.clone()),
    );
    let _ = state.root.done_tx.send(lead.id.clone());

    // 4. Finalize.
    // Capture the ORIGINAL cancel state BEFORE we call terminate() below.
    // This preserves the distinction between user Ctrl-C (real interruption)
    // and our internal cleanup termination signal to drain workers.
    let was_interrupted = cancel.is_draining() || cancel.is_terminated();

    // Any in-flight workers get cancelled. `cancel` is the RUN-level token
    // (observed only by the lead's SessionHandle), so terminating it alone
    // leaves workers orphaned — their sessions use per-task tokens in
    // `state.root.worker_cancels`. Cascade to every live worker so their
    // SessionHandles SIGTERM the child claude processes too. Without this
    // cascade, `ps` showed live claude workers after the run "finished"
    // and the summary marked them Cancelled with no actual termination.
    {
        let cancels = state.root.worker_cancels.read().await;
        for tok in cancels.values() {
            tok.terminate();
        }
    }
    cancel.terminate();
    // Give them up to TERMINATE_GRACE to drain.
    tokio::time::sleep(pitboss_core::session::TERMINATE_GRACE).await;

    let worker_records: Vec<pitboss_core::store::TaskRecord> = {
        let workers = state.root.workers.read().await;
        let worker_models = state.root.worker_models.read().await;
        workers
            .iter()
            .filter(|(id, _)| *id != &lead.id) // don't double-count the lead
            .map(|(id, w)| match w {
                crate::dispatch::state::WorkerState::Done(rec) => rec.clone(),
                crate::dispatch::state::WorkerState::Pending
                | crate::dispatch::state::WorkerState::Running { .. }
                | crate::dispatch::state::WorkerState::Paused { .. }
                | crate::dispatch::state::WorkerState::Frozen { .. } => {
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
                        final_message: None,
                        parent_task_id: Some(lead.id.clone()),
                        pause_count: 0,
                        reprompt_count: 0,
                        approvals_requested: 0,
                        approvals_approved: 0,
                        approvals_rejected: 0,
                        model: worker_models.get(id).cloned(),
                        failure_reason: None,
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

    // Workers cancelled because the lead finished cleanly are not failures.
    let lead_succeeded = matches!(lead_record.status, pitboss_core::store::TaskStatus::Success);
    let tasks_failed = all_records
        .iter()
        .filter(|r| {
            if lead_succeeded && matches!(r.status, pitboss_core::store::TaskStatus::Cancelled) {
                false
            } else {
                !matches!(r.status, pitboss_core::store::TaskStatus::Success)
            }
        })
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

    // Optional post-mortem dump of shared-store contents.
    if resolved.dump_shared_store {
        let dump_path = run_subdir.join("shared-store.json");
        if let Err(e) = state.root.shared_store.dump_to_path(&dump_path).await {
            tracing::warn!(?e, "shared-store dump failed");
        }
    }

    // Emit RunFinished. Hierarchical mode used to build the notification
    // router and never fire this event — consumers only saw the run via
    // RunDispatched (now) and the per-tool approval traffic. Cost intent
    // (spent_usd) is left at 0.0 here; the lead-spend accounting work
    // (separate roadmap item) will populate it.
    if let Some(router) = &notification_router {
        let env = crate::notify::NotificationEnvelope::new(
            &run_id.to_string(),
            crate::notify::Severity::Info,
            crate::notify::PitbossEvent::RunFinished {
                run_id: run_id.to_string(),
                tasks_total: summary.tasks_total,
                tasks_failed,
                duration_ms: u64::try_from(summary.total_duration_ms).unwrap_or(0),
                spent_usd: 0.0,
            },
            Utc::now(),
        );
        let _ = router.dispatch(env).await;
    }

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
/// `pitboss mcp-bridge --actor-id <id> --actor-role <role> <socket>` proxies
/// bytes between claude's stdio pair and the pitboss MCP server's unix socket,
/// stamping every inbound tool call with the caller's identity.
///
/// This avoids relying on a non-standard `transport: { type: "unix", ... }`
/// field that claude's MCP client may not honor. The generated config uses
/// only the documented `command` + `args` (stdio transport) shape.
/// Build the `mcpServers` JSON object with the pitboss bridge entry plus any
/// operator-declared `[[mcp_server]]` entries from the manifest.
///
/// `token` is the actor's per-connection auth token (minted by
/// `DispatchState::mint_token`). When `Some`, it is appended as
/// `--token <hex>` to the bridge args; the server then validates it and
/// binds the connection identity. Closes #145.
fn build_mcp_servers_json(
    pitboss_exe: &std::path::Path,
    socket: &std::path::Path,
    actor_id: &str,
    actor_role: &str,
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> serde_json::Map<String, serde_json::Value> {
    let mut servers = serde_json::Map::new();
    let mut args: Vec<serde_json::Value> = vec![
        serde_json::Value::String("mcp-bridge".into()),
        serde_json::Value::String("--actor-id".into()),
        serde_json::Value::String(actor_id.to_string()),
        serde_json::Value::String("--actor-role".into()),
        serde_json::Value::String(actor_role.to_string()),
    ];
    if let Some(t) = token {
        args.push(serde_json::Value::String("--token".into()));
        args.push(serde_json::Value::String(t.to_string()));
    }
    args.push(serde_json::Value::String(socket.to_string_lossy().into()));
    servers.insert(
        "pitboss".into(),
        serde_json::json!({
            "command": pitboss_exe.to_string_lossy(),
            "args": args,
        }),
    );
    for s in extra_servers {
        let mut entry = serde_json::Map::new();
        entry.insert("command".into(), s.command.clone().into());
        entry.insert(
            "args".into(),
            serde_json::Value::Array(
                s.args
                    .iter()
                    .map(|a| serde_json::Value::String(a.clone()))
                    .collect(),
            ),
        );
        if !s.env.is_empty() {
            entry.insert("env".into(), serde_json::json!(s.env));
        }
        servers.insert(s.id.clone(), serde_json::Value::Object(entry));
    }
    servers
}

async fn write_mcp_config(
    path: &std::path::Path,
    socket: &std::path::Path,
    actor_id: &str,
    actor_role: &str, // "lead" or "worker"
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> Result<()> {
    // Find the pitboss binary path (the one running us now) so the lead can
    // re-exec the same build for the bridge subcommand.
    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;
    let mcp_servers = build_mcp_servers_json(
        &pitboss_exe,
        socket,
        actor_id,
        actor_role,
        token,
        extra_servers,
    );
    let cfg = serde_json::json!({ "mcpServers": mcp_servers });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    // mcp-config.json carries the actor's auth token (#145). Restrict to the
    // running user so a same-UID-but-different-process attacker can't read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(())
}

/// Emit a worker-scoped `--mcp-config` file. Lists only the 7 shared-store
/// tools — NOT spawn_worker / cancel_worker / wait_for_worker / etc.
/// The bridge command includes the worker's actor_id + actor_role=worker
/// so the dispatcher can identify the caller and enforce namespace authz.
///
/// `token` is the worker's auth token (issue #145). When `Some`, it is
/// embedded so the bridge can prove the connection's identity to the
/// server; when `None`, no token is written (server falls back to
/// rejecting calls without bound identity).
pub async fn write_worker_mcp_config(
    path: &std::path::Path,
    socket: &std::path::Path,
    worker_id: &str,
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> Result<()> {
    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;
    let mcp_servers = build_mcp_servers_json(
        &pitboss_exe,
        socket,
        worker_id,
        "worker",
        token,
        extra_servers,
    );
    let cfg = serde_json::json!({
        "mcpServers": mcp_servers,
        "allowedTools": [
            "mcp__pitboss__kv_get",
            "mcp__pitboss__kv_set",
            "mcp__pitboss__kv_cas",
            "mcp__pitboss__kv_list",
            "mcp__pitboss__kv_wait",
            "mcp__pitboss__lease_acquire",
            "mcp__pitboss__lease_release"
        ]
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    // Same 0o600 hardening as write_mcp_config: keeps the embedded token
    // unreadable to other local users (issue #145).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(())
}

/// Emit a sublead-scoped `--mcp-config` file. Lists only the SUBLEAD_MCP_TOOLS
/// (no spawn_sublead, no wait_for_sublead — depth-2 cap enforced).
/// The bridge command includes the sublead's actor_id + actor_role=sublead
/// so the dispatcher can identify the caller and enforce namespace authz.
///
/// `token` is the sublead's auth token (issue #145). When `Some`, it is
/// embedded into the bridge args.
pub async fn build_sublead_mcp_config(
    sublead_id: &str,
    socket: &std::path::Path,
    run_subdir: &std::path::Path,
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> Result<PathBuf> {
    use crate::dispatch::runner::SUBLEAD_MCP_TOOLS;

    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;
    let mcp_servers = build_mcp_servers_json(
        &pitboss_exe,
        socket,
        sublead_id,
        "sublead",
        token,
        extra_servers,
    );
    let cfg = serde_json::json!({
        "mcpServers": mcp_servers,
        "allowedTools": SUBLEAD_MCP_TOOLS.iter().collect::<Vec<_>>()
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;

    // Ensure run_subdir exists. In production runner.rs creates it before any
    // sublead spawns, but test harnesses build DispatchState without creating
    // the directory, so this create_dir_all is a defensive no-op in production
    // and a required step in tests.
    tokio::fs::create_dir_all(run_subdir).await?;
    let mcp_config_path = run_subdir.join(format!("sublead-{sublead_id}-mcp-config.json"));
    tokio::fs::write(&mcp_config_path, bytes).await?;
    // Same 0o600 hardening as the lead/worker config writers (#145).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            tokio::fs::set_permissions(&mcp_config_path, std::fs::Permissions::from_mode(0o600))
                .await;
    }
    Ok(mcp_config_path)
}
