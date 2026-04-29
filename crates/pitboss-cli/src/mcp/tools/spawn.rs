//! Spawn-side MCP handlers and helpers: `handle_spawn_worker`,
//! `spawn_resume_worker` (also used by lifecycle continue/reprompt and
//! by `control/server.rs`), the per-spawn budget reservation helpers,
//! the `worker_spawn_args` argv builder, and `initial_estimate_for`.

use std::sync::Arc;

use anyhow::{bail, Result};
use pitboss_core::store::TaskRecord;
use uuid::Uuid;

use super::{layer_for_worker, SpawnWorkerArgs, SpawnWorkerResult};
use crate::dispatch::layer::LayerState;
use crate::dispatch::state::{DispatchState, WorkerState};

/// Resolve the `LayerState` into which a new worker should be registered,
/// based on the caller's role from `_meta`.
///
/// - `Lead` / `root_lead` alias (or absent `_meta`): root layer — unchanged v0.5 behavior.
/// - `Sublead`: the caller's own sub-tree layer.
/// - `Worker`: REJECTED — workers cannot spawn workers (depth-2 cap).
///
/// NOTE: Unlike `resolve_layer_for_caller` in `mcp/server.rs` (which routes
/// workers to their registered layer), this function explicitly rejects Worker
/// callers — spawning workers-from-workers would exceed the depth-2 cap.
async fn resolve_target_layer(
    state: &Arc<DispatchState>,
    caller_id: &str,
    caller_role: crate::shared_store::ActorRole,
) -> anyhow::Result<Arc<LayerState>> {
    use crate::shared_store::ActorRole;
    match caller_role {
        ActorRole::Lead => Ok(Arc::clone(&state.root)),
        ActorRole::Sublead => {
            let subleads = state.subleads.read().await;
            subleads
                .get(caller_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown sublead_id: {caller_id}"))
        }
        ActorRole::Worker => anyhow::bail!(
            "spawn_worker is not available to workers (depth-2 cap); \
             only leads and sub-leads may spawn workers"
        ),
    }
}

pub async fn handle_spawn_worker(
    state: &Arc<DispatchState>,
    args: SpawnWorkerArgs,
) -> Result<SpawnWorkerResult> {
    use crate::shared_store::ActorRole;

    // Resolve caller identity from _meta (v0.6+) or fall back to root-lead
    // identity for backward-compat with v0.5 callers that omit _meta.
    let (caller_id, caller_role): (String, ActorRole) = match &args.meta {
        Some(m) => (m.actor_id.clone(), m.actor_role),
        None => (state.root.lead_id.clone(), ActorRole::Lead),
    };

    // Resolve the target layer: sub-lead callers land in their own sub-tree;
    // root-lead callers land in root; worker callers are rejected.
    let target_layer = resolve_target_layer(state, &caller_id, caller_role).await?;

    // Guard 1: draining (root cancel gate — always check root even for sublead workers)
    if state.root.cancel.is_draining() || state.root.cancel.is_terminated() {
        bail!("run is draining: no new workers accepted");
    }

    // Guard 1a: API health. If a recent worker classified as rate-limited
    // or auth-failed, refuse new spawns until the condition clears. The
    // lead's Claude session sees a structured error and can plan around
    // the outage (wait for reset, report to operator) rather than firing
    // another doomed subprocess. See `dispatch::failure_detection` for
    // the gate rules.
    if let Err(gate) = state.api_health.check_can_spawn().await {
        use crate::dispatch::failure_detection::SpawnGateReason;
        match gate {
            SpawnGateReason::RateLimited { retry_after } => bail!(
                "api rate-limited: refusing to spawn new workers until {} (retry_after); a \
                 prior worker hit the limit",
                retry_after.to_rfc3339()
            ),
            SpawnGateReason::AuthFailed { clears_at } => bail!(
                "api auth failed on a recent worker; refusing to spawn new workers until {} \
                 (clears_at). Rotate credentials or cancel the run",
                clears_at.to_rfc3339()
            ),
        }
    }

    // Guard 1b: plan approval. When the manifest opts in with
    // `[run].require_plan_approval = true`, each lead must call
    // `propose_plan` and get operator approval before any worker
    // dispatches in its own layer. The gate is read from the TARGET layer
    // (not `state.root`), so a sub-lead's approval only unblocks its own
    // sub-tree — the root still needs its own plan approval before root-
    // layer worker spawns succeed.
    if state.root.manifest.require_plan_approval
        && !target_layer
            .plan_approved
            .load(std::sync::atomic::Ordering::Acquire)
    {
        bail!(
            "plan approval required: call `propose_plan` and wait for \
             operator approval before spawning workers"
        );
    }

    // Guard 2: worker cap (checked against the target layer's own cap)
    if let Some(cap) = target_layer.manifest.max_workers {
        let active = target_layer.active_worker_count().await;
        if active >= cap as usize {
            bail!("worker cap reached: {} active (max {})", active, cap);
        }
    }

    // Resolve the worker's model up-front so the budget guard can price it.
    let lead = target_layer.manifest.lead.as_ref();
    let worker_model = args
        .model
        .clone()
        .or_else(|| lead.map(|l| l.model.clone()))
        .unwrap_or_else(|| "claude-haiku-4-5".to_string());

    let task_id = format!("worker-{}", Uuid::now_v7());

    // Guard 3: budget (reservation-aware + model-aware). Budget accounting
    // runs against the target layer's envelope (sub-lead's own budget for
    // sublead callers, root budget for root-lead callers).
    //
    // Special case: if the sub-lead is in shared-pool mode (budget_usd = None
    // on the target layer but root has a budget), the reservation falls back
    // to root's pool.
    // TODO(sub-task 3): fully exercise and validate shared-pool reservation
    // semantics; for now treat None budget_usd on the target layer as
    // uncapped (no reservation placed, same as root-layer uncapped behavior).
    if let Some(budget) = target_layer.manifest.budget_usd {
        // Estimate this worker's cost using its intended model, as the median
        // of prior workers priced at their actual models (or a model-specific
        // fallback if no worker has completed yet).
        let estimate = estimate_new_worker_cost_for_layer(&target_layer, &worker_model).await;

        // Hold `reserved_usd` across the whole compute/compare/add step so
        // two concurrent spawn_workers can't both pass the budget check
        // before either increments the reservation. Spent_usd is captured
        // inside the same critical section to keep the arithmetic on a
        // single consistent snapshot.
        let mut reserved_guard = target_layer.reserved_usd.lock().await;
        let spent = *target_layer.spent_usd.lock().await;
        let reserved = *reserved_guard;
        if spent + reserved + estimate > budget {
            drop(reserved_guard);
            if let Some(router) = target_layer.notification_router.clone() {
                let envelope = crate::notify::NotificationEnvelope::new(
                    &state.root.run_id.to_string(),
                    crate::notify::Severity::Error,
                    crate::notify::PitbossEvent::BudgetExceeded {
                        run_id: state.root.run_id.to_string(),
                        spent_usd: spent,
                        budget_usd: budget,
                    },
                    chrono::Utc::now(),
                );
                let _ = router.dispatch(envelope).await;
            }
            bail!(
                "budget exceeded: ${:.2} spent + ${:.2} reserved + ${:.2} estimated > ${:.2} budget",
                spent, reserved, estimate, budget
            );
        }
        // Reserve against the target layer.
        *reserved_guard += estimate;
        drop(reserved_guard);
        target_layer
            .worker_reservations
            .write()
            .await
            .insert(task_id.clone(), estimate);
    }

    {
        let mut workers = target_layer.workers.write().await;
        workers.insert(task_id.clone(), WorkerState::Pending);
    }

    // Register in the worker_layer_index so KV routing can look up this
    // worker's layer in O(1).
    //   - Root-lead callers: None = root layer (unchanged v0.5 behavior)
    //   - Sub-lead callers: Some(caller_id) = the sub-lead's layer
    let layer_index_value: Option<String> = if matches!(caller_role, ActorRole::Sublead) {
        Some(caller_id.clone())
    } else {
        None
    };
    state
        .worker_layer_index
        .write()
        .await
        .insert(task_id.clone(), layer_index_value);

    // Register the worker's CancelToken on the target layer. This both
    // inserts into `worker_cancels` and eagerly propagates any in-flight
    // drain/terminate state from `target_layer.cancel` to the new token —
    // the post-#99 cascade gap fix lives inside `register_worker_cancel`
    // so the watcher / registration paths share one cascade rule
    // (terminate dominates drain). Pinned by `tests/cancel_cascade_flows.rs`.
    let worker_cancel = pitboss_core::session::CancelToken::new();
    target_layer
        .register_worker_cancel(task_id.clone(), worker_cancel)
        .await;

    // Record the prompt preview before spawning the background task.
    let prompt_preview: String = args.prompt.chars().take(80).collect();
    target_layer
        .worker_prompts
        .write()
        .await
        .insert(task_id.clone(), prompt_preview);

    // Track the worker's resolved model so cost estimation can price
    // completed workers at the correct rate.
    target_layer
        .worker_models
        .write()
        .await
        .insert(task_id.clone(), worker_model.clone());

    // Resolve the worker's directory and worktree-use, falling back through:
    //   1. `args.directory` / per-args overrides — operator's spawn_worker call
    //   2. target_layer's lead — set for root-lead callers, NOT for sub-leads
    //      (derive_sublead_manifest clears `lead` on the sub-manifest)
    //   3. ROOT lead — always present in hierarchical runs and pinned to the
    //      operator's project directory
    //   4. /tmp — last-ditch fallback (git worktree creation will fail loudly,
    //      which is the right outcome for a malformed run)
    //
    // Pre-fix, sub-lead-spawned workers without an explicit `directory` arg
    // landed at /tmp because step 2 returned None and we skipped to step 4
    // immediately. Worker spawn then SpawnFailed with "not inside a git
    // work-tree: /tmp", visible in summary.jsonl but not in the sublead's
    // wait_for_worker reply (which sees the SpawnFailed task record but
    // doesn't surface the cwd that caused it). Smoke-test artifact:
    // sublead-1's only worker SpawnFailed; sublead-2's first worker
    // SpawnFailed and its second only succeeded because haiku retried with
    // an explicit directory after seeing the failure.
    let root_lead = state.root.manifest.lead.as_ref();
    let worker_dir: std::path::PathBuf = args
        .directory
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            target_layer
                .manifest
                .lead
                .as_ref()
                .map(|l| l.directory.clone())
        })
        .or_else(|| root_lead.map(|l| l.directory.clone()))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));

    // Resolve tools, timeout: per-args override -> lead defaults -> fallback.
    // (worker_model was resolved above for the budget guard.)
    let worker_tools = args
        .tools
        .clone()
        .or_else(|| lead.map(|l| l.tools.clone()))
        .or_else(|| root_lead.map(|l| l.tools.clone()))
        .unwrap_or_default();
    let worker_timeout_secs = args
        .timeout_secs
        .or_else(|| lead.map(|l| l.timeout_secs))
        .or_else(|| root_lead.map(|l| l.timeout_secs))
        .unwrap_or(3600);
    let worker_branch = args.branch.clone();
    // Mirror the directory cascade: target lead → root lead → default(true).
    // Without the root-lead step, sub-lead workers always made a worktree
    // (lead.is_none_or → true) regardless of root's `use_worktree` setting,
    // surprising operators who set `use_worktree = false` at the root level.
    let worker_use_worktree = lead
        .map(|l| l.use_worktree)
        .or_else(|| root_lead.map(|l| l.use_worktree))
        .unwrap_or(true);

    // Retrieve the per-worker cancel token we inserted above.
    let worker_cancel_bg = target_layer
        .worker_cancels
        .read()
        .await
        .get(&task_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("internal: worker_cancel missing after insert"))?;

    let state_bg = Arc::clone(state);
    let target_layer_bg = Arc::clone(&target_layer);
    let task_id_bg = task_id.clone();
    let lead_id_bg = target_layer.lead_id.clone();
    let prompt_bg = args.prompt.clone();

    tokio::spawn(async move {
        run_worker(
            state_bg,
            target_layer_bg,
            task_id_bg,
            lead_id_bg,
            prompt_bg,
            worker_dir,
            worker_branch,
            worker_model,
            worker_tools,
            worker_timeout_secs,
            worker_use_worktree,
            worker_cancel_bg,
        )
        .await;
    });

    Ok(SpawnWorkerResult {
        task_id,
        // worktree_path is set later inside Done(rec); callers needing it
        // should go through worker_status / wait_for_worker.
        worktree_path: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    state: Arc<DispatchState>,
    layer: Arc<LayerState>,
    task_id: String,
    lead_id: String,
    prompt: String,
    directory: std::path::PathBuf,
    branch: Option<String>,
    model: String,
    tools: Vec<String>,
    timeout_secs: u64,
    use_worktree: bool,
    cancel: pitboss_core::session::CancelToken,
) {
    use chrono::Utc;
    use pitboss_core::process::SpawnCmd;
    use pitboss_core::session::SessionHandle;
    use pitboss_core::store::TaskStatus;
    use std::time::Duration;

    let task_dir = layer.run_subdir.join("tasks").join(&task_id);
    let _ = tokio::fs::create_dir_all(&task_dir).await;
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");

    // Optional worktree prep.
    let mut worktree_handle: Option<pitboss_core::worktree::Worktree> = None;
    let cwd = if use_worktree {
        let name = format!("pitboss-worker-{}-{}", task_id, layer.run_id);
        match layer.wt_mgr.prepare(&directory, &name, branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                // Persist the worktree path so the TUI's Detail view can run
                // `git diff --shortstat` against it mid-flight, not just after
                // the TaskRecord lands. TaskRecord.worktree_path is only set
                // on settle; writing this sidecar file closes the gap.
                let _ = tokio::fs::write(
                    task_dir.join("worktree.path"),
                    p.to_string_lossy().as_bytes(),
                )
                .await;
                worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                // Release the spawn-time reservation (SpawnFailed path).
                release_reservation_for_layer(&layer, &task_id).await;
                // Record a SpawnFailed TaskRecord and broadcast done.
                let now = Utc::now();
                let rec = TaskRecord {
                    task_id: task_id.clone(),
                    status: TaskStatus::SpawnFailed,
                    exit_code: None,
                    started_at: now,
                    ended_at: now,
                    duration_ms: 0,
                    worktree_path: None,
                    log_path: log_path.clone(),
                    token_usage: Default::default(),
                    claude_session_id: None,
                    final_message_preview: Some(format!("worktree error: {e}")),
                    final_message: None,
                    parent_task_id: Some(lead_id),
                    pause_count: 0,
                    reprompt_count: 0,
                    approvals_requested: 0,
                    approvals_approved: 0,
                    approvals_rejected: 0,
                    model: Some(model.clone()),
                    failure_reason: None,
                };
                let _ = layer.store.append_record(layer.run_id, &rec).await;
                layer
                    .workers
                    .write()
                    .await
                    .insert(task_id.clone(), WorkerState::Done(rec));
                // Fan out to root so cross-layer wait_actor subscribers wake
                // even on early-exit paths like SpawnFailed. Same rationale
                // as the normal-exit fan-out below.
                let _ = layer.done_tx.send(task_id.clone());
                if !std::sync::Arc::ptr_eq(&layer, &state.root) {
                    let _ = state.root.done_tx.send(task_id);
                }
                return;
            }
        }
    } else {
        directory.clone()
    };

    // Transition Pending → Running.
    layer.workers.write().await.insert(
        task_id.clone(),
        WorkerState::Running {
            started_at: Utc::now(),
            session_id: None,
        },
    );

    // Generate worker-scoped mcp-config.json so the worker can reach
    // the shared store via the bridge-injected identity.
    //
    // Mint the worker's auth token here so it gets embedded in the
    // mcp-config.json args. The server validates the token on every
    // tools/call and binds the connection's canonical identity from
    // it — defending against same-UID processes that connect directly
    // to the socket and forge `_meta.actor_role`. Issue #145.
    let worker_token = state.mint_token(&task_id, "worker").await;
    let worker_task_dir = layer.run_subdir.join("tasks").join(&task_id);
    tokio::fs::create_dir_all(&worker_task_dir).await.ok();
    let worker_mcp_config = worker_task_dir.join("mcp-config.json");
    let socket_path =
        crate::mcp::server::socket_path_for_run(layer.run_id, &layer.manifest.run_dir);
    let mcp_config_arg = match crate::dispatch::hierarchical::write_worker_mcp_config(
        &worker_mcp_config,
        &socket_path,
        &task_id,
        Some(&worker_token),
        &layer.manifest.mcp_servers,
    )
    .await
    {
        Ok(()) => Some(worker_mcp_config),
        Err(e) => {
            tracing::warn!("write worker mcp-config for {task_id}: {e}; proceeding without");
            None
        }
    };

    // Worker env: inherit from the parent layer's resolved lead env (which
    // already merges `[defaults.env]` + `[lead.env]`), then apply pitboss
    // defaults to fill gaps like `CLAUDE_CODE_ENTRYPOINT=sdk-ts`. Matches
    // the precedence used at sublead spawn time.
    //
    // Previous behavior was `env: Default::default()` (empty env). A
    // manifest setting `[defaults.env.WORK_DIR] = "/project/out"` would
    // reach the lead and sublead subprocesses but NOT workers — a
    // sublead's bash call to `echo ... >> "$WORK_DIR/file"` would get an
    // empty `WORK_DIR` and drop output to `/file`. Same bug class as the
    // sublead-env regression fixed earlier; this closes the worker hole.
    //
    // `SpawnWorkerArgs` has no `env` field today, so the operator-env
    // layer is an empty HashMap. If we ever add one, pass it here.
    //
    // Same target_layer.lead = None pitfall as the worker_dir cascade
    // above: sub-leads have `lead` cleared on their derived manifest, so
    // a sub-lead-spawned worker would otherwise get an empty env even
    // though the operator set `[defaults.env]` at the manifest level.
    // Fall back through the root lead (always populated, carries the
    // merged [defaults.env] + [lead.env]) before defaulting to empty.
    let lead_env_for_worker = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.env.clone())
        .or_else(|| state.root.manifest.lead.as_ref().map(|l| l.env.clone()))
        .unwrap_or_default();
    let worker_routing = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.permission_routing)
        .or_else(|| {
            state
                .root
                .manifest
                .lead
                .as_ref()
                .map(|l| l.permission_routing)
        })
        .unwrap_or_default();
    let worker_env = crate::dispatch::sublead::compose_sublead_env(
        &lead_env_for_worker,
        &std::collections::HashMap::new(),
        worker_routing,
    );
    let cmd = SpawnCmd {
        program: layer.claude_binary.clone(),
        args: worker_spawn_args(
            &prompt,
            &model,
            &tools,
            mcp_config_arg.as_deref(),
            worker_routing,
        ),
        cwd: cwd.clone(),
        env: worker_env,
    };

    let outcome = {
        let (session_id_tx, mut session_id_rx) = tokio::sync::mpsc::channel::<String>(1);
        let session_layer = Arc::clone(&layer);
        let task_id_for_rx = task_id.clone();
        let promote_task = tokio::spawn(async move {
            if let Some(sid) = session_id_rx.recv().await {
                let mut workers = session_layer.workers.write().await;
                if let Some(WorkerState::Running { started_at, .. }) =
                    workers.get(&task_id_for_rx).cloned()
                {
                    workers.insert(
                        task_id_for_rx,
                        WorkerState::Running {
                            started_at,
                            session_id: Some(sid),
                        },
                    );
                }
            }
        });
        // Register a pid slot so the SIGSTOP freeze-pause path can
        // signal this worker directly. Populated inside
        // `run_to_completion` right after the spawn succeeds.
        let pid_slot = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        layer
            .worker_pids
            .write()
            .await
            .insert(task_id.clone(), pid_slot.clone());
        let outcome = SessionHandle::new(task_id.clone(), Arc::clone(&layer.spawner), cmd)
            .with_log_path(log_path.clone())
            .with_stderr_log_path(stderr_path.clone())
            .with_session_id_tx(session_id_tx)
            .with_pid_slot(pid_slot)
            .run_to_completion(cancel, Duration::from_secs(timeout_secs))
            .await;
        promote_task.abort();
        // Clean up the pid slot — the worker is done, the pid is stale.
        layer.worker_pids.write().await.remove(&task_id);
        outcome
    };

    let mut status = match outcome.final_state {
        pitboss_core::session::SessionState::Completed => TaskStatus::Success,
        pitboss_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
        pitboss_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
        pitboss_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
        pitboss_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
        _ => TaskStatus::Failed,
    };

    // Reclassify silent exits driven by a recent rejected approval. When a
    // worker calls request_approval / propose_plan, gets {approved: false}
    // (operator action or [[approval_policy]] auto_reject), and exits
    // shortly after, the claude subprocess exits 0 and we'd otherwise
    // mark Success. Now distinguished as ApprovalRejected (operator) or
    // ApprovalTimedOut (TTL-fired fallback) so headless operators can
    // tell the difference.
    if matches!(status, TaskStatus::Success) {
        match state.approval_driven_termination(&task_id).await {
            Some(crate::dispatch::state::ApprovalTerminationKind::Rejected) => {
                status = TaskStatus::ApprovalRejected;
            }
            Some(crate::dispatch::state::ApprovalTerminationKind::TimedOut) => {
                status = TaskStatus::ApprovalTimedOut;
            }
            None => {}
        }
    }

    // Cleanup worktree per policy.
    if let Some(wt) = worktree_handle {
        let succeeded = matches!(status, TaskStatus::Success);
        let _ = layer.wt_mgr.cleanup(wt, layer.cleanup_policy, succeeded);
    }

    let worktree_path = if use_worktree { Some(cwd) } else { None };
    let counters = layer
        .worker_counters
        .read()
        .await
        .get(&task_id)
        .cloned()
        .unwrap_or_default();
    // Classify non-zero exits by scanning the tail of stdout/stderr for known
    // markers (rate-limit, network, auth, etc.). `None` on exit_code == 0.
    let failure_reason = crate::dispatch::failure_detection::detect_failure_reason(
        outcome.exit_code,
        Some(&log_path),
        Some(&stderr_path),
    );
    let rec = TaskRecord {
        task_id: task_id.clone(),
        status,
        exit_code: outcome.exit_code,
        started_at: outcome.started_at,
        ended_at: outcome.ended_at,
        duration_ms: outcome.duration_ms(),
        worktree_path,
        log_path,
        token_usage: outcome.token_usage,
        claude_session_id: outcome.claude_session_id,
        final_message_preview: outcome.final_message_preview,
        final_message: outcome.final_message,
        parent_task_id: Some(lead_id.clone()),
        pause_count: counters.pause_count,
        reprompt_count: counters.reprompt_count,
        approvals_requested: counters.approvals_requested,
        approvals_approved: counters.approvals_approved,
        approvals_rejected: counters.approvals_rejected,
        model: Some(model.clone()),
        failure_reason,
    };

    // Persist record.
    let _ = layer.store.append_record(layer.run_id, &rec).await;

    // Broadcast structured failure to any connected TUI so operators see
    // *why* the worker failed without opening logs, and so a parent lead
    // can react (back off on RateLimit, fail fast on AuthFailure).
    // Also record into `api_health` so the next spawn call short-circuits
    // while the condition persists — one dead worker is enough; a loop
    // of them burns budget faster than any operator can intervene.
    if let Some(reason) = rec.failure_reason.clone() {
        state.api_health.record(&reason).await;
        crate::dispatch::failure_detection::broadcast_worker_failed(
            &state.root,
            task_id.clone(),
            Some(lead_id.clone()),
            reason,
            &["root", &lead_id, &task_id],
        )
        .await;
    }

    // Release the spawn-time reservation before accumulating actual cost.
    release_reservation_for_layer(&layer, &task_id).await;

    // Accumulate cost into the layer's spent_usd.
    if let Some(cost) = pitboss_core::prices::cost_usd(&model, &rec.token_usage) {
        *layer.spent_usd.lock().await += cost;
    }

    // Transition to Done + broadcast on the layer's done channel.
    layer
        .workers
        .write()
        .await
        .insert(task_id.clone(), WorkerState::Done(rec));
    // Clean up the worker_layer_index entry (on DispatchState, not LayerState).
    state.worker_layer_index.write().await.remove(&task_id);
    // Release any run-global leases the worker was holding.
    let released_count = state.run_leases.release_all_held_by(&task_id).await;
    if released_count > 0 {
        tracing::info!(worker_id = %task_id, count = released_count, "auto-released run-global leases on worker termination");
    }
    // Broadcast termination on the worker's own layer AND on root.
    // wait_for_actor_internal (the engine for wait_actor / wait_for_worker)
    // always subscribes via state.root.done_tx — regardless of which layer
    // the caller is in — so a sublead-spawned worker that only fired its
    // own layer's done_tx would be invisible to its parent sub-lead's
    // wait_actor (which subscribes to root). Fan out to root so every
    // wait_actor caller, anywhere in the tree, wakes on every worker
    // completion. Cheap; broadcast.send is O(subscribers).
    let _ = layer.done_tx.send(task_id.clone());
    if !std::sync::Arc::ptr_eq(&layer, &state.root) {
        let _ = state.root.done_tx.send(task_id);
    }
}

/// Remove `task_id`'s spawn-time reservation from `reserved_usd` on the
/// given `LayerState`. Safe to call even if no reservation was placed
/// (returns 0 from the map). Clamped at 0.0 to avoid f64 drift going negative.
async fn release_reservation_for_layer(layer: &Arc<LayerState>, task_id: &str) {
    let reserved_amount = layer
        .worker_reservations
        .write()
        .await
        .remove(task_id)
        .unwrap_or(0.0);
    if reserved_amount > 0.0 {
        let mut r = layer.reserved_usd.lock().await;
        *r = (*r - reserved_amount).max(0.0);
    }
}

/// Estimate the cost (USD) of a new worker against the given `LayerState`'s
/// completed-worker history. Takes a `LayerState` reference so it works for
/// both root and sub-tree layers.
async fn estimate_new_worker_cost_for_layer(layer: &Arc<LayerState>, intended_model: &str) -> f64 {
    use pitboss_core::prices::cost_usd;
    let workers = layer.workers.read().await;
    let models = layer.worker_models.read().await;
    let mut costs: Vec<f64> = Vec::new();
    for (id, w) in workers.iter() {
        if let WorkerState::Done(rec) = w {
            let m = models.get(id).map(String::as_str).unwrap_or(intended_model);
            if let Some(c) = cost_usd(m, &rec.token_usage) {
                costs.push(c);
            }
        }
    }
    if costs.is_empty() {
        return initial_estimate_for(intended_model);
    }
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    costs[costs.len() / 2]
}

/// MCP tool names workers need permission to call. Narrower than the lead's
/// `PITBOSS_MCP_TOOLS` — workers only get the shared-store surface, never
/// the orchestration tools (spawn_worker / cancel_worker / request_approval
/// / etc.). Pre-approved via `--allowedTools` so claude doesn't stall at
/// the interactive permission prompt.
pub const PITBOSS_WORKER_MCP_TOOLS: &[&str] = &[
    "mcp__pitboss__kv_get",
    "mcp__pitboss__kv_set",
    "mcp__pitboss__kv_cas",
    "mcp__pitboss__kv_list",
    "mcp__pitboss__kv_wait",
    "mcp__pitboss__lease_acquire",
    "mcp__pitboss__lease_release",
];

pub(super) fn worker_spawn_args(
    prompt: &str,
    model: &str,
    tools: &[String],
    mcp_config: Option<&std::path::Path>,
    permission_routing: crate::manifest::schema::PermissionRouting,
) -> Vec<String> {
    use crate::manifest::schema::PermissionRouting;
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if permission_routing == PermissionRouting::PathA {
        args.push("--dangerously-skip-permissions".into());
    }
    // Plugin/skill isolation (see runner::lead_spawn_args doc).
    args.push("--strict-mcp-config".into());
    args.push("--disable-slash-commands".into());
    // Workers always get the shared-store MCP tools in their allowlist when
    // an mcp-config is supplied, alongside their user-declared tools. Without
    // this, kv_set / lease_acquire / etc. hit the permission prompt which
    // can't be answered in non-interactive mode.
    let mut allowed: Vec<String> = tools.to_vec();
    if mcp_config.is_some() {
        for t in PITBOSS_WORKER_MCP_TOOLS {
            allowed.push((*t).to_string());
        }
        // Path B: pre-allow permission_prompt so workers can route checks.
        if permission_routing == PermissionRouting::PathB {
            allowed.push("mcp__pitboss__permission_prompt".into());
        }
    }
    if !allowed.is_empty() {
        args.push("--allowedTools".into());
        args.push(allowed.join(","));
    }
    args.push("--model".into());
    args.push(model.to_string());
    if let Some(path) = mcp_config {
        args.push("--mcp-config".into());
        args.push(path.display().to_string());
    }
    args.push("-p".into());
    args.push(prompt.to_string());
    args
}

/// Spawn a resume-subprocess for `task_id`, replacing the worker's current
/// SessionHandle. Used by `pause_worker` → `continue_worker` and by
/// `reprompt_worker`. Returns immediately after setting state to Running; the
/// background task drives `run_to_completion` and the terminal TaskRecord.
pub async fn spawn_resume_worker(
    state: &Arc<DispatchState>,
    task_id: String,
    prompt: String,
    session_id: String,
) -> anyhow::Result<()> {
    use chrono::Utc;
    // Resolve the owning layer (root OR sub-lead). Sub-tree workers'
    // models, prompts, pid slots, and worker map all live in the
    // sub-lead's `LayerState`; reading/writing root would split state
    // and silently corrupt sub-tree resumes (issue #146).
    let layer = layer_for_worker(state, &task_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let model = layer
        .worker_models
        .read()
        .await
        .get(&task_id)
        .cloned()
        .unwrap_or_else(|| "claude-haiku-4-5".to_string());
    let tools: Vec<String> = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.tools.clone())
        .or_else(|| state.root.manifest.lead.as_ref().map(|l| l.tools.clone()))
        .unwrap_or_default();
    let timeout_secs = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.timeout_secs)
        .or_else(|| state.root.manifest.lead.as_ref().map(|l| l.timeout_secs))
        .unwrap_or(3600);
    let cwd = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.directory.clone())
        .or_else(|| {
            state
                .root
                .manifest
                .lead
                .as_ref()
                .map(|l| l.directory.clone())
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let worker_cancel = pitboss_core::session::CancelToken::new();
    // Same post-register cascade plug as `handle_spawn_worker` (#99): if the
    // owning layer's cancel has already fired, the fire-once watcher won't
    // re-issue to this token — propagate synchronously before we register a
    // Running worker against it.
    if layer.cancel.is_terminated() {
        worker_cancel.terminate();
    } else if layer.cancel.is_draining() {
        worker_cancel.drain();
    }
    layer
        .worker_cancels
        .write()
        .await
        .insert(task_id.clone(), worker_cancel.clone());
    layer.workers.write().await.insert(
        task_id.clone(),
        WorkerState::Running {
            started_at: Utc::now(),
            session_id: Some(session_id.clone()),
        },
    );
    let state_bg = Arc::clone(state);
    let layer_bg = layer.clone();
    let task_id_bg = task_id.clone();
    let lead_id_bg = layer.lead_id.clone();

    // Generate (or reuse) worker-scoped mcp-config.json for the resumed
    // subprocess. write_worker_mcp_config is idempotent so calling it
    // again on an existing file is safe.
    //
    // Mint a fresh auth token for the resumed bridge — the original
    // token from the initial spawn is still valid (we never revoke), but
    // re-minting keeps the per-spawn token lifetime simple. Issue #145.
    let worker_token = state.mint_token(&task_id, "worker").await;
    let worker_task_dir = layer.run_subdir.join("tasks").join(&task_id);
    tokio::fs::create_dir_all(&worker_task_dir).await.ok();
    let worker_mcp_config_path = worker_task_dir.join("mcp-config.json");
    let socket_path =
        crate::mcp::server::socket_path_for_run(layer.run_id, &layer.manifest.run_dir);
    let mcp_config_arg = match crate::dispatch::hierarchical::write_worker_mcp_config(
        &worker_mcp_config_path,
        &socket_path,
        &task_id,
        Some(&worker_token),
        &layer.manifest.mcp_servers,
    )
    .await
    {
        Ok(()) => Some(worker_mcp_config_path),
        Err(e) => {
            tracing::warn!(
                "write worker mcp-config for {task_id} (resume): {e}; proceeding without"
            );
            None
        }
    };

    let resume_routing = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.permission_routing)
        .or_else(|| {
            state
                .root
                .manifest
                .lead
                .as_ref()
                .map(|l| l.permission_routing)
        })
        .unwrap_or_default();
    // Build spawn args with --resume.
    let mut spawn_args_v = worker_spawn_args(
        &prompt,
        &model,
        &tools,
        mcp_config_arg.as_deref(),
        resume_routing,
    );
    spawn_args_v.insert(0, "--resume".into());
    spawn_args_v.insert(1, session_id);

    // Resume path mirrors the initial spawn: inherit the parent lead's
    // resolved env so `[defaults.env]` and `[lead.env]` survive a
    // pause/continue or reprompt cycle.
    let lead_env_for_resume = layer
        .manifest
        .lead
        .as_ref()
        .map(|l| l.env.clone())
        .or_else(|| state.root.manifest.lead.as_ref().map(|l| l.env.clone()))
        .unwrap_or_default();
    let resume_env = crate::dispatch::sublead::compose_sublead_env(
        &lead_env_for_resume,
        &std::collections::HashMap::new(),
        resume_routing,
    );
    let cmd = pitboss_core::process::SpawnCmd {
        program: layer.claude_binary.clone(),
        args: spawn_args_v,
        cwd,
        env: resume_env,
    };
    let task_dir = layer.run_subdir.join("tasks").join(&task_id);
    let _ = tokio::fs::create_dir_all(&task_dir).await;
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");
    let resume_model = model.clone();
    // Register a pid slot for the resumed subprocess too, so
    // freeze-pause works across continue_worker boundaries.
    let resume_pid_slot = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    layer
        .worker_pids
        .write()
        .await
        .insert(task_id.clone(), resume_pid_slot.clone());

    tokio::spawn(async move {
        use pitboss_core::store::{TaskRecord, TaskStatus};
        let outcome = pitboss_core::session::SessionHandle::new(
            task_id_bg.clone(),
            Arc::clone(&layer_bg.spawner),
            cmd,
        )
        .with_log_path(log_path.clone())
        .with_stderr_log_path(stderr_path.clone())
        .with_pid_slot(resume_pid_slot)
        .run_to_completion(worker_cancel, std::time::Duration::from_secs(timeout_secs))
        .await;
        // Clean up the pid slot when the resumed subprocess exits.
        layer_bg.worker_pids.write().await.remove(&task_id_bg);
        let mut status = match outcome.final_state {
            pitboss_core::session::SessionState::Completed => TaskStatus::Success,
            pitboss_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
            pitboss_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
            pitboss_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
            pitboss_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
            _ => TaskStatus::Failed,
        };
        // Reclassify silent exits driven by a recent rejected approval (see
        // run_worker for the same pattern + rationale).
        if matches!(status, TaskStatus::Success) {
            match state_bg.approval_driven_termination(&task_id_bg).await {
                Some(crate::dispatch::state::ApprovalTerminationKind::Rejected) => {
                    status = TaskStatus::ApprovalRejected;
                }
                Some(crate::dispatch::state::ApprovalTerminationKind::TimedOut) => {
                    status = TaskStatus::ApprovalTimedOut;
                }
                None => {}
            }
        }
        let counters = layer_bg
            .worker_counters
            .read()
            .await
            .get(&task_id_bg)
            .cloned()
            .unwrap_or_default();
        let rec = TaskRecord {
            task_id: task_id_bg.clone(),
            status,
            exit_code: outcome.exit_code,
            started_at: outcome.started_at,
            ended_at: outcome.ended_at,
            duration_ms: outcome.duration_ms(),
            worktree_path: None,
            log_path: log_path.clone(),
            token_usage: outcome.token_usage,
            claude_session_id: outcome.claude_session_id,
            final_message_preview: outcome.final_message_preview,
            final_message: outcome.final_message,
            parent_task_id: Some(lead_id_bg.clone()),
            pause_count: counters.pause_count,
            reprompt_count: counters.reprompt_count,
            approvals_requested: counters.approvals_requested,
            approvals_approved: counters.approvals_approved,
            approvals_rejected: counters.approvals_rejected,
            model: Some(resume_model),
            failure_reason: crate::dispatch::failure_detection::detect_failure_reason(
                outcome.exit_code,
                Some(&log_path),
                Some(&stderr_path),
            ),
        };
        let _ = layer_bg.store.append_record(layer_bg.run_id, &rec).await;
        if let Some(reason) = rec.failure_reason.clone() {
            state_bg.api_health.record(&reason).await;
            crate::dispatch::failure_detection::broadcast_worker_failed(
                &layer_bg,
                task_id_bg.clone(),
                Some(lead_id_bg.clone()),
                reason,
                &["root", &lead_id_bg, &task_id_bg],
            )
            .await;
        }
        layer_bg
            .workers
            .write()
            .await
            .insert(task_id_bg.clone(), WorkerState::Done(rec));
        let _ = layer_bg.done_tx.send(task_id_bg);
    });

    Ok(())
}

/// Initial per-worker cost estimate before any worker has completed. Used as
/// the fallback inside `estimate_new_worker_cost_for_layer` and as a
/// model-aware replacement for the old `INITIAL_WORKER_COST_EST = 0.10`
/// constant which undercounted Sonnet (~5x) and Opus (~20x) workers.
///
/// Normalizes dated model suffixes (e.g. `claude-haiku-4-5-20251001`) the
/// same way `pitboss_core::prices::rates_for` does.
pub(crate) fn initial_estimate_for(model: &str) -> f64 {
    let base = model.split('-').take(4).collect::<Vec<_>>().join("-");
    match base.as_str() {
        "claude-opus-4-7" => 2.00,
        "claude-sonnet-4-6" => 0.50,
        _ => 0.10, // haiku or unknown
    }
}
