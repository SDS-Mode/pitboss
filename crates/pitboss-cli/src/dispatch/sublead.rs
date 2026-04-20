//! Sub-lead spawn and teardown helpers. A sub-lead is structurally a
//! Claude subprocess (like a worker) that ALSO has its own LayerState
//! (workers map, shared_store, approval queue). It spawns into the root's
//! sub-leads map; its workers spawn into its own LayerState.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

use crate::control::protocol::{ControlEvent, EventEnvelope};
use crate::dispatch::actor::{ActorId, ActorPath};
use crate::dispatch::layer::LayerState;
use crate::dispatch::state::{DispatchState, SubleadTerminalRecord};
use crate::manifest::resolve::ResolvedManifest;
use crate::shared_store::SharedStore;
use pitboss_core::session::CancelToken;
use pitboss_core::worktree::CleanupPolicy;

/// Outcome classification for a terminated sub-lead session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubleadOutcome {
    Success,
    Cancel,
    Timeout,
    Error(String),
    /// Sub-lead exited after receiving `{approved: false}` from an
    /// operator action or an `[[approval_policy]]` auto-reject rule.
    /// Distinguished from `Success` because the sub-lead didn't do
    /// meaningful work before exiting — it asked and was denied.
    ApprovalRejected,
    /// Sub-lead exited after its pending approval timed out (TTL
    /// expired, fallback fired). Distinguished from `ApprovalRejected`
    /// because no operator attention reached the approval.
    ApprovalTimedOut,
}

impl SubleadOutcome {
    /// Convert to the canonical string stored in `SubleadTerminalRecord.outcome`.
    pub fn as_str(&self) -> &str {
        match self {
            SubleadOutcome::Success => "success",
            SubleadOutcome::Cancel => "cancel",
            SubleadOutcome::Timeout => "timeout",
            SubleadOutcome::Error(_) => "error",
            SubleadOutcome::ApprovalRejected => "approval_rejected",
            SubleadOutcome::ApprovalTimedOut => "approval_timed_out",
        }
    }
}

/// Configuration for a sub-lead spawn, validated against root's caps.
#[derive(Debug, Clone, Default)]
pub struct SubleadSpawnRequest {
    pub prompt: String,
    pub model: String,
    pub budget_usd: Option<f64>,
    pub max_workers: Option<u32>,
    pub lead_timeout_secs: Option<u64>,
    pub initial_ref: HashMap<String, Value>,
    pub read_down: bool,
    /// Operator-supplied env vars passed to the sub-lead's claude
    /// subprocess. Layered over pitboss's own defaults
    /// (`CLAUDE_CODE_ENTRYPOINT=sdk-ts`); operator-set keys win when the
    /// names collide. See `apply_pitboss_env_defaults` in dispatch/runner.rs
    /// for the resolution order.
    pub env: HashMap<String, String>,
    /// Operator-supplied tool list override for `--allowedTools`. Empty
    /// means "use the standard sublead toolset" (preserves v0.6 behavior).
    /// Non-empty replaces the user-tool portion; the pitboss MCP tools
    /// (`mcp__pitboss__*`) are always included regardless so the sub-lead
    /// can still orchestrate workers.
    pub tools: Vec<String>,
}

/// Validated, defaults-applied resource envelope for a sub-lead spawn.
#[derive(Debug, Clone)]
pub enum ResolvedEnvelope {
    Owned {
        budget_usd: f64,
        max_workers: u32,
        lead_timeout_secs: u64,
    },
    /// read_down=true with no explicit budget/workers — sub-tree shares root's pool.
    SharedPool,
}

/// Validate the request against root's manifest caps and apply defaults.
/// Returns `Err` if required fields are missing or caps would be exceeded.
///
/// Cap enforcement (Task 5.1):
/// - `max_sublead_budget_usd`: per-call `budget_usd` must not exceed cap.
/// - `max_subleads`: current sub-lead count + 1 must not exceed cap.
/// - `max_workers_across_tree`: projected total workers must not exceed cap.
///
/// `sublead_defaults` fallback (Task 5.1): when `spawn_sublead` omits
/// `budget_usd`, `max_workers`, or `lead_timeout_secs`, the values are
/// filled from `manifest.lead.sublead_defaults` when present.
pub async fn resolve_envelope(
    state: &Arc<DispatchState>,
    req: &SubleadSpawnRequest,
) -> Result<ResolvedEnvelope> {
    let manifest = &state.root.manifest;
    let lead = manifest.lead.as_ref();

    // ── Apply sublead_defaults for omitted fields ────────────────────────────
    // When the caller omits budget_usd / max_workers / lead_timeout_secs /
    // read_down, fall through to [lead.sublead_defaults] from the manifest.
    let defaults = lead.and_then(|l| l.sublead_defaults.as_ref());

    let effective_budget_usd = req
        .budget_usd
        .or_else(|| defaults.and_then(|d| d.budget_usd));
    let effective_max_workers = req
        .max_workers
        .or_else(|| defaults.and_then(|d| d.max_workers));
    let effective_lead_timeout_secs = req
        .lead_timeout_secs
        .or_else(|| defaults.and_then(|d| d.lead_timeout_secs));
    let effective_read_down = req.read_down || defaults.is_some_and(|d| d.read_down);

    // ── Shared-pool mode ─────────────────────────────────────────────────────
    // read_down=true with no explicit resource allocation → share root's pool.
    if effective_read_down && effective_budget_usd.is_none() && effective_max_workers.is_none() {
        return Ok(ResolvedEnvelope::SharedPool);
    }

    // ── Resolve required fields ──────────────────────────────────────────────
    let budget_usd =
        effective_budget_usd.ok_or_else(|| anyhow!("budget_usd required when read_down=false"))?;

    let max_workers = effective_max_workers
        .ok_or_else(|| anyhow!("max_workers required when read_down=false"))?;

    let lead_timeout_secs = effective_lead_timeout_secs.unwrap_or(3600);

    // ── Cap: max_sublead_budget_usd ──────────────────────────────────────────
    if let Some(cap) = lead.and_then(|l| l.max_sublead_budget_usd) {
        if budget_usd > cap {
            return Err(anyhow!(
                "spawn_sublead: budget_usd ${:.2} exceeds per-sublead cap ${:.2}",
                budget_usd,
                cap
            ));
        }
    }

    // ── Cap: max_subleads ────────────────────────────────────────────────────
    if let Some(cap) = lead.and_then(|l| l.max_subleads) {
        let current = state.subleads.read().await.len() as u32;
        if current + 1 > cap {
            return Err(anyhow!(
                "spawn_sublead: max_subleads cap {} reached (current: {})",
                cap,
                current
            ));
        }
    }

    // ── Cap: max_workers_across_tree ─────────────────────────────────────────
    if let Some(cap) = lead.and_then(|l| l.max_workers_across_tree) {
        let root_workers = state.root.workers.read().await.len() as u32;
        let subleads_guard = state.subleads.read().await;
        let sub_workers: u32 = {
            let mut total = 0u32;
            for sub in subleads_guard.values() {
                total += sub.workers.read().await.len() as u32;
            }
            total
        };
        let projected = root_workers + sub_workers + max_workers;
        if projected > cap {
            return Err(anyhow!(
                "spawn_sublead: max_workers_across_tree cap {} would be exceeded \
                 (current {}, requested {})",
                cap,
                root_workers + sub_workers,
                max_workers
            ));
        }
    }

    Ok(ResolvedEnvelope::Owned {
        budget_usd,
        max_workers,
        lead_timeout_secs,
    })
}

/// Spawn a sub-lead under `state.root`. Reserves budget at root, creates the
/// sub-tree LayerState, seeds `/ref/*` from `req.initial_ref`, registers
/// the sub-tree on `state.subleads`, and launches the sub-lead's Claude
/// subprocess in a background task. Returns the new sublead_id.
pub async fn spawn_sublead(
    state: &Arc<DispatchState>,
    req: SubleadSpawnRequest,
) -> Result<ActorId> {
    // 1. Resolve envelope (apply defaults, check caps).
    let envelope = resolve_envelope(state, &req).await?;

    // 2. Reserve budget at root for Owned envelopes.
    //
    // I-1: Check root budget before reserving, mirroring the spawn_worker guard
    // in mcp/tools.rs. Hold the lock across the check-and-add so concurrent
    // spawn_sublead calls can't both pass the guard simultaneously.
    //
    // Extract the amount to reserve so we can roll it back on failure (I-2).
    let reserved_amount: Option<f64> = if let ResolvedEnvelope::Owned { budget_usd, .. } = &envelope
    {
        let amount = *budget_usd;
        if let Some(cap) = state.root.manifest.budget_usd {
            // Snapshot both accumulators, then check before reserving.
            // Mirroring the pattern in spawn_worker (mcp/tools.rs:329-356):
            // read spent and reserved, check, then re-acquire reserved to add.
            let spent = *state.root.spent_usd.lock().await;
            let reserved = *state.root.reserved_usd.lock().await;
            if spent + reserved + amount > cap {
                bail!(
                    "spawn_sublead: budget exceeded: ${:.2} spent + ${:.2} reserved + ${:.2} estimated > ${:.2} budget",
                    spent,
                    reserved,
                    amount,
                    cap
                );
            }
        }
        *state.root.reserved_usd.lock().await += amount;
        Some(amount)
    } else {
        None
    };

    // 3. Mint a fresh sublead_id.
    let sublead_id: ActorId = format!("sublead-{}", Uuid::now_v7());

    // I-2: Wrap all post-reservation steps so that any failure releases the
    // budget reservation before propagating. Explicit cleanup keeps the error
    // paths readable without a RAII guard type that would need to hold an
    // async Mutex across an await point.
    let result: Result<ActorId> = async {
        // 4. Build sub-tree manifest (inherits root fields; clears lead + tasks).
        let sub_manifest = derive_sublead_manifest(&state.root.manifest, &envelope);

        // 5. Construct a fresh SharedStore for this sub-tree.
        let sub_store = Arc::new(SharedStore::new());

        // 6. Snapshot-seed /ref/* from initial_ref.
        for (k, v) in &req.initial_ref {
            let path = format!("/ref/{}", k);
            let bytes = serde_json::to_vec(v)
                .with_context(|| format!("serializing initial_ref key {k}"))?;
            sub_store
                .set(&path, bytes, &sublead_id)
                .await
                .with_context(|| format!("seeding {path} in sub-tree store"))?;
        }

        // 7. Construct the sub-tree LayerState.
        let sub_layer = Arc::new(LayerState::new(
            state.root.run_id,
            sub_manifest,
            state.root.store.clone(),
            CancelToken::new(),
            sublead_id.clone(),
            state.root.spawner.clone(),
            state.root.claude_binary.clone(),
            state.root.wt_mgr.clone(),
            CleanupPolicy::Never, // sub-tree shares run cleanup behaviour; finalized by root
            state.root.run_subdir.clone(),
            // Inherit root approval policy; sub-leads can escalate via request_approval.
            state.root.approval_policy,
            // No notification router for sub-trees — root router handles run events.
            None,
            sub_store,
            reserved_amount,
        ));

        // 8. Register sub-tree LayerState on root DispatchState.
        state
            .subleads
            .write()
            .await
            .insert(sublead_id.clone(), sub_layer.clone());

        // Emit SubleadSpawned lifecycle event to the control plane.
        {
            let (budget_usd_val, max_workers_val) = match &envelope {
                ResolvedEnvelope::Owned {
                    budget_usd,
                    max_workers,
                    ..
                } => (Some(*budget_usd), Some(*max_workers)),
                ResolvedEnvelope::SharedPool => (None, None),
            };
            let ev = EventEnvelope {
                actor_path: ActorPath::new(["root", sublead_id.as_str()]),
                event: ControlEvent::SubleadSpawned {
                    sublead_id: sublead_id.clone(),
                    budget_usd: budget_usd_val,
                    max_workers: max_workers_val,
                    read_down: req.read_down,
                },
            };
            state.root.broadcast_control_event(ev).await;
        }

        // I-1: If root has entered cascade-drain phase AFTER this read-lock snapshot,
        // immediately drain the new sub-tree's cancel token before spawning the session.
        // This guarantees any sub-lead spawned post-drain inherits the cancellation
        // synchronously, avoiding the race where the cascade watcher's snapshot would
        // miss this sub-lead entirely.
        if state.root.cancel.is_draining() {
            sub_layer.cancel.drain();
            tracing::info!(sublead_id = %sublead_id, "spawned during cascade drain; immediate cascade applied");
        }

        // 9. Spawn the sub-lead's Claude session (wired in Task 2.3).
        spawn_sublead_session(
            state.clone(),
            sub_layer.clone(),
            req.prompt,
            req.model,
            envelope,
            req.env,
            req.tools,
        )
        .await
        .context("sub-lead claude session spawn failed")?;

        Ok(sublead_id)
    }
    .await;

    // I-2: On failure, release the reservation to avoid leaking reserved budget.
    if result.is_err() {
        if let Some(amount) = reserved_amount {
            let mut reserved = state.root.reserved_usd.lock().await;
            *reserved = (*reserved - amount).max(0.0);
        }
    }

    result
}

/// Build a `ResolvedManifest` for the sub-tree by inheriting root fields and
/// overriding with envelope-specific resource caps.
///
/// Field-by-field rationale for inherited vs. overridden vs. cleared:
///
/// - `lead = None`: the sub-tree has no further `[lead]` TOML block; the
///   sub-lead itself is the layer's lead at runtime.
/// - `tasks = vec![]`: no pre-declared tasks; sub-lead spawns workers dynamically.
/// - `budget_usd`, `max_workers`, `lead_timeout_secs`: come from the envelope.
/// - `notifications = vec![]`: cleared — the sub-tree LayerState already sets
///   `notification_router: None`; keeping the config would be dead/misleading.
/// - `dump_shared_store = false`: sub-tree post-mortem dumps are out of scope for
///   v0.6; revisit when sub-tree post-mortem inspection becomes a feature request.
/// - `require_plan_approval`: inherited — sub-leads spawning workers may
///   legitimately need plan approval too.
/// - `approval_policy`: inherited (also passed directly to LayerState::new).
fn derive_sublead_manifest(
    root: &ResolvedManifest,
    envelope: &ResolvedEnvelope,
) -> ResolvedManifest {
    let mut sub = root.clone();
    sub.lead = None;
    sub.tasks = vec![];
    // See field rationale in the doc-comment above.
    sub.notifications = vec![];
    sub.dump_shared_store = false;
    match envelope {
        ResolvedEnvelope::Owned {
            budget_usd,
            max_workers,
            lead_timeout_secs,
        } => {
            sub.budget_usd = Some(*budget_usd);
            sub.max_workers = Some(*max_workers);
            sub.lead_timeout_secs = Some(*lead_timeout_secs);
        }
        ResolvedEnvelope::SharedPool => {
            // Shared-pool mode: sub-tree's caps are root's caps (no override).
        }
    }
    sub
}

/// Spawn the sub-lead's Claude subprocess and monitor it to completion.
///
/// Uses `SessionHandle` (the same machinery as `run_worker`) to drive
/// the sub-lead's Claude process end-to-end:
///
/// 1. Builds the per-sub-lead MCP config pointing at the run-level socket.
/// 2. Spawns via `state.root.spawner` (sub-leads are spawned by root).
/// 3. Background-monitors stdout for stream-json events and accumulates cost.
/// 4. On termination: builds a `TaskRecord`, persists it, classifies the
///    outcome, and calls `reconcile_terminated_sublead`.
///
/// Cancel handling: the sub-lead's `sub_layer.cancel` token is propagated
/// by the cascade watcher from root. `SessionHandle::run_to_completion`
/// observes it via `cancel.await_terminate()`, sends SIGTERM, then SIGKILL
/// after TERMINATE_GRACE.
#[allow(clippy::too_many_arguments)]
async fn spawn_sublead_session(
    state: Arc<DispatchState>,
    sub_layer: Arc<LayerState>,
    prompt: String,
    model: String,
    envelope: ResolvedEnvelope,
    operator_env: std::collections::HashMap<String, String>,
    tools_override: Vec<String>,
) -> Result<()> {
    use crate::dispatch::hierarchical::build_sublead_mcp_config;
    use crate::dispatch::runner::sublead_spawn_args;
    use crate::mcp::server::socket_path_for_run;
    use pitboss_core::process::SpawnCmd;
    use pitboss_core::session::SessionHandle;
    use pitboss_core::store::TaskStatus;

    let sublead_id = sub_layer.lead_id.clone();

    // 1. Build the MCP socket path (shared run-level socket — all actors
    //    connect here; mcp-bridge stamps _meta.actor_role=sublead).
    let socket_path = socket_path_for_run(sub_layer.run_id, &sub_layer.manifest.run_dir);

    // 2. Build per-sub-lead mcp-config.json.
    let mcp_config_path = build_sublead_mcp_config(&sublead_id, &socket_path)
        .await
        .context("build sublead mcp-config")?;

    // 3. Build the CLI args: sublead toolset (or operator override),
    //    model, prompt, no --resume on first spawn.
    let tools_for_args: Option<&[String]> = if tools_override.is_empty() {
        None
    } else {
        Some(&tools_override)
    };
    let args = sublead_spawn_args(
        &sublead_id,
        &prompt,
        &model,
        &mcp_config_path,
        None,
        tools_for_args,
    );

    // 4. Task log directory (mirrors workers' layout for consistency).
    let task_dir = sub_layer.run_subdir.join("tasks").join(&sublead_id);
    tokio::fs::create_dir_all(&task_dir).await.ok();
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");

    // 5. Build the spawn command. CWD is root's run_subdir (sub-leads don't
    //    get separate worktrees in v0.6 — revisit in future).
    //    Env precedence (lowest → highest): pitboss defaults
    //    (`CLAUDE_CODE_ENTRYPOINT=sdk-ts`) → operator-supplied env from the
    //    `spawn_sublead` MCP call. Operator wins for collisions.
    let mut sublead_env: std::collections::HashMap<String, String> = operator_env.clone();
    crate::dispatch::runner::apply_pitboss_env_defaults(&mut sublead_env);
    let initial_cmd = SpawnCmd {
        program: sub_layer.claude_binary.clone(),
        args,
        cwd: sub_layer.run_subdir.clone(),
        env: sublead_env,
    };

    // 6. Determine the lead timeout — fall back to 1 hour if the manifest
    //    didn't set one (should always be set for Owned envelopes).
    let timeout_secs = match &envelope {
        ResolvedEnvelope::Owned {
            lead_timeout_secs, ..
        } => *lead_timeout_secs,
        ResolvedEnvelope::SharedPool => sub_layer.manifest.lead_timeout_secs.unwrap_or(3600),
    };

    // 7. Set up the reprompt delivery channel.
    //    `send_synthetic_reprompt` sends messages here; the background loop
    //    handles them by kill+resume-ing the sub-lead's current subprocess.
    let (reprompt_tx, mut reprompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    *sub_layer.reprompt_tx.lock().await = Some(reprompt_tx);

    // Background task: run the sub-lead's subprocess to completion, handling
    // any synthetic reprompts by kill+resuming. We detach with tokio::spawn so
    // `spawn_sublead_session` returns immediately.
    let state_bg = state.clone();
    let sub_layer_bg = sub_layer.clone();
    let sublead_id_bg = sublead_id.clone();
    let model_bg = model.clone();
    let mcp_config_path_bg = mcp_config_path.clone();
    let operator_env_bg = operator_env;
    let tools_override_bg = tools_override;

    tokio::spawn(async move {
        let mut current_cmd = initial_cmd;
        // The last session_id emitted by the subprocess. Needed for --resume when
        // a synthetic reprompt arrives.
        let mut last_session_id: Option<String> = None;
        // Accumulate cost and reprompt count across all subprocess iterations.
        let mut total_token_usage = pitboss_core::parser::TokenUsage::default();
        let mut reprompt_count: u32 = 0;
        // Record the very first started_at for the compound TaskRecord.
        let overall_started_at = chrono::Utc::now();

        // Mark the lead as Running (no session_id yet) so worker_status et al.
        // can report it.
        sub_layer_bg.workers.write().await.insert(
            sublead_id_bg.clone(),
            crate::dispatch::state::WorkerState::Running {
                started_at: overall_started_at,
                session_id: None,
            },
        );

        // Subprocess loop: runs until the subprocess exits without a pending reprompt.
        let final_outcome = loop {
            // Build a per-iteration session_id channel so we can capture the
            // new session id and update the workers map entry.
            let (session_id_tx, mut session_id_rx) = tokio::sync::mpsc::channel::<String>(1);

            // Per-subprocess cancel token: receives termination from either the
            // sub-tree cancel (cascade/operator kill) or from the reprompt path
            // (which kills the current process to restart it with --resume).
            let proc_cancel = CancelToken::new();

            // Bridge: forward terminate from sub-tree cancel → subprocess cancel.
            // This ensures operator kills and cascade drains still reach the process.
            {
                let tree_cancel = sub_layer_bg.cancel.clone();
                let proc = proc_cancel.clone();
                tokio::spawn(async move {
                    tree_cancel.await_terminate().await;
                    proc.terminate();
                });
            }

            // Launch the subprocess.
            let outcome = SessionHandle::new(
                sublead_id_bg.clone(),
                Arc::clone(&sub_layer_bg.spawner),
                current_cmd.clone(),
            )
            .with_log_path(log_path.clone())
            .with_stderr_log_path(stderr_path.clone())
            .with_session_id_tx(session_id_tx)
            .run_to_completion(proc_cancel.clone(), Duration::from_secs(timeout_secs))
            .await;

            // Update last_session_id and the workers map if the subprocess
            // emitted a session_id before it exited.
            if let Ok(sid) = session_id_rx.try_recv() {
                sub_layer_bg.workers.write().await.insert(
                    sublead_id_bg.clone(),
                    crate::dispatch::state::WorkerState::Running {
                        started_at: overall_started_at,
                        session_id: Some(sid.clone()),
                    },
                );
                last_session_id = Some(sid);
            } else if let Some(sid) = outcome.claude_session_id.clone() {
                // Fallback: session_id from the final result event (not yet in workers map
                // since the subprocess already finished, but saves it for future reprompts).
                last_session_id = Some(sid);
            }

            // Accumulate cost.
            if let Some(cost) = pitboss_core::prices::cost_usd(&model_bg, &outcome.token_usage) {
                *sub_layer_bg.spent_usd.lock().await += cost;
            }
            total_token_usage.add(&outcome.token_usage);

            // Check if the process ended due to cancellation/termination AND
            // there is a pending reprompt in the channel (meaning we should
            // kill+resume rather than treating this as a final exit).
            let pending_reprompt = reprompt_rx.try_recv().ok();

            if let Some(new_prompt) = pending_reprompt {
                // A synthetic reprompt arrived. If we have a session_id, resume.
                if let Some(ref sid) = last_session_id {
                    tracing::info!(
                        sublead_id = %sublead_id_bg,
                        session_id = %sid,
                        "synthetic reprompt: killing current subprocess and resuming with new prompt"
                    );
                    reprompt_count += 1;

                    // Re-build args with --resume and the new prompt.
                    // Carry forward the operator's tool override for resume too.
                    let tools_for_resume: Option<&[String]> = if tools_override_bg.is_empty() {
                        None
                    } else {
                        Some(&tools_override_bg)
                    };
                    let resume_args = sublead_spawn_args(
                        &sublead_id_bg,
                        &new_prompt,
                        &model_bg,
                        &mcp_config_path_bg,
                        Some(sid.as_str()),
                        tools_for_resume,
                    );
                    // Same env precedence as the initial spawn: operator first,
                    // pitboss defaults fill gaps.
                    let mut resume_env: std::collections::HashMap<String, String> =
                        operator_env_bg.clone();
                    crate::dispatch::runner::apply_pitboss_env_defaults(&mut resume_env);
                    current_cmd = SpawnCmd {
                        program: sub_layer_bg.claude_binary.clone(),
                        args: resume_args,
                        cwd: sub_layer_bg.run_subdir.clone(),
                        env: resume_env,
                    };

                    // Reset workers entry to Running with no session_id (will be
                    // updated once the resumed subprocess emits its init event).
                    sub_layer_bg.workers.write().await.insert(
                        sublead_id_bg.clone(),
                        crate::dispatch::state::WorkerState::Running {
                            started_at: overall_started_at,
                            session_id: None,
                        },
                    );

                    // Continue the loop: spawn the resumed subprocess.
                    continue;
                } else {
                    // No session_id — cannot resume. Treat as normal termination.
                    tracing::warn!(
                        sublead_id = %sublead_id_bg,
                        "synthetic reprompt arrived but no session_id available; \
                         treating as normal termination"
                    );
                    break outcome;
                }
            }

            // No pending reprompt — subprocess reached a terminal state normally.
            break outcome;
        };

        // Close the reprompt channel so further sends return errors.
        *sub_layer_bg.reprompt_tx.lock().await = None;

        // 7. Classify the outcome for the terminal record.
        let mut sublead_outcome = match final_outcome.final_state {
            pitboss_core::session::SessionState::Completed => SubleadOutcome::Success,
            pitboss_core::session::SessionState::Cancelled => SubleadOutcome::Cancel,
            pitboss_core::session::SessionState::TimedOut => SubleadOutcome::Timeout,
            pitboss_core::session::SessionState::SpawnFailed { ref message } => {
                SubleadOutcome::Error(message.clone())
            }
            pitboss_core::session::SessionState::Failed { ref message } => {
                SubleadOutcome::Error(message.clone())
            }
            _ => SubleadOutcome::Error("unknown terminal state".into()),
        };

        // Reclassify silent exits driven by a recent rejected approval. A
        // sub-lead that called propose_plan, got auto_reject, and exited
        // would otherwise show as Success. See run_worker for the same
        // pattern.
        if matches!(sublead_outcome, SubleadOutcome::Success) {
            if let Some(crate::dispatch::state::ApprovalTerminationKind::Rejected) =
                state_bg.approval_driven_termination(&sublead_id_bg).await
            {
                sublead_outcome = SubleadOutcome::ApprovalRejected;
            }
        }

        // 8. Map to TaskStatus for the TaskRecord.
        let status = match &sublead_outcome {
            SubleadOutcome::Success => TaskStatus::Success,
            SubleadOutcome::Cancel => TaskStatus::Cancelled,
            SubleadOutcome::Timeout => TaskStatus::TimedOut,
            SubleadOutcome::Error(_) => TaskStatus::Failed,
            SubleadOutcome::ApprovalRejected => TaskStatus::ApprovalRejected,
            SubleadOutcome::ApprovalTimedOut => TaskStatus::ApprovalTimedOut,
        };

        // 9. Build and persist a TaskRecord for the sub-lead's compound session.
        //    Uses total_token_usage across all subprocess iterations.
        let rec = pitboss_core::store::TaskRecord {
            task_id: sublead_id_bg.clone(),
            status,
            exit_code: final_outcome.exit_code,
            started_at: overall_started_at,
            ended_at: final_outcome.ended_at,
            duration_ms: (final_outcome.ended_at - overall_started_at)
                .num_milliseconds()
                .max(0),
            worktree_path: None,
            log_path,
            token_usage: total_token_usage,
            claude_session_id: final_outcome.claude_session_id,
            final_message_preview: final_outcome.final_message_preview,
            parent_task_id: Some("root".into()),
            pause_count: 0,
            reprompt_count,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: Some(model_bg),
        };
        if let Err(e) = sub_layer_bg
            .store
            .append_record(sub_layer_bg.run_id, &rec)
            .await
        {
            tracing::warn!(
                sublead_id = %sublead_id_bg,
                error = %e,
                "failed to persist sub-lead TaskRecord"
            );
        }

        // 10. Reconcile: release reservation, update root's spent_usd,
        //     populate sublead_results, and wake wait_actor subscribers.
        if let Err(e) =
            reconcile_terminated_sublead(&state_bg, &sublead_id_bg, sublead_outcome).await
        {
            tracing::warn!(
                sublead_id = %sublead_id_bg,
                error = %e,
                "reconcile_terminated_sublead failed"
            );
        }
    });

    Ok(())
}

/// Called when a sub-lead terminates (success, cancel, timeout, or error).
/// Reconciles the sub-tree's actual spend against the original reservation:
/// spend is moved into root's `spent_usd`, and any unspent reservation is
/// released back to root's reservable pool.
///
/// Idempotent: calling twice for the same `sublead_id` is a no-op the
/// second time (sub-tree LayerState is removed on first call).
///
/// The original reservation amount is read from
/// `sub_layer.original_reservation_usd`, which was set when the sub-layer
/// was constructed by `spawn_sublead`.
///
/// `outcome` classifies the terminal state for the `SubleadTerminalRecord`
/// and the `SubleadTerminated` control-plane event. Existing callers (tests
/// that reconcile manually) should pass `SubleadOutcome::Success`.
pub async fn reconcile_terminated_sublead(
    state: &Arc<DispatchState>,
    sublead_id: &str,
    outcome: SubleadOutcome,
) -> Result<()> {
    let sub_layer_opt = state.subleads.write().await.remove(sublead_id);
    let Some(sub_layer) = sub_layer_opt else {
        // Already reconciled or never existed.
        return Ok(());
    };

    let actual_spend = *sub_layer.spent_usd.lock().await;
    let original_reservation_usd = sub_layer.original_reservation_usd.unwrap_or(0.0);
    let unspent = (original_reservation_usd - actual_spend).max(0.0);

    // Release the original reservation in full
    {
        let mut reserved = state.root.reserved_usd.lock().await;
        *reserved = (*reserved - original_reservation_usd).max(0.0);
    }
    // Then record the actual spend
    {
        let mut spent = state.root.spent_usd.lock().await;
        *spent += actual_spend;
    }

    tracing::info!(
        sublead_id = %sublead_id,
        reserved = original_reservation_usd,
        spent = actual_spend,
        returned = unspent,
        outcome = outcome.as_str(),
        "sub-lead budget reconciled"
    );

    // Emit SubleadTerminated lifecycle event to the control plane.
    {
        let ev = EventEnvelope {
            actor_path: ActorPath::new(["root", sublead_id]),
            event: ControlEvent::SubleadTerminated {
                sublead_id: sublead_id.to_string(),
                spent_usd: actual_spend,
                unspent_usd: unspent,
                outcome: outcome.as_str().to_string(),
            },
        };
        state.root.broadcast_control_event(ev).await;
    }

    // Release any run-global leases the sub-lead was holding
    let released_count = state.run_leases.release_all_held_by(sublead_id).await;
    if released_count > 0 {
        tracing::info!(sublead_id = %sublead_id, count = released_count, "auto-released run-global leases on sublead termination");
    }

    // Persist terminal record so wait_actor(sublead_id) can return it.
    let record = SubleadTerminalRecord {
        sublead_id: sublead_id.to_string(),
        outcome: outcome.as_str().to_string(),
        spent_usd: actual_spend,
        unspent_usd: unspent,
        terminated_at: chrono::Utc::now(),
    };
    state
        .sublead_results
        .write()
        .await
        .insert(sublead_id.to_string(), record);

    // Wake any wait_actor subscribers blocked on this sublead_id.
    let _ = state.root.done_tx.send(sublead_id.to_string());

    Ok(())
}
