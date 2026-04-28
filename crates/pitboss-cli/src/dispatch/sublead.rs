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

/// Build the env for a sub-lead's claude subprocess.
///
/// Precedence (lowest → highest):
/// 1. `lead_env` — the root lead's resolved env (already contains
///    `[defaults.env]` merged with `[lead.env]`). Propagating this to
///    subleads means project-level path blocks (e.g. `WORK_DIR`,
///    `ARTIFACTS_DIR`) reach sub-leads automatically without the root
///    lead having to re-pass them in every `spawn_sublead` MCP call.
/// 2. `operator_env` — per-spawn env passed explicitly by the caller of
///    the `spawn_sublead` MCP tool. Wins over the lead-inherited env for
///    collisions (operator override wins).
/// 3. Pitboss defaults (e.g. `CLAUDE_CODE_ENTRYPOINT=sdk-ts`) —
///    `entry().or_insert()` semantics, so these only fill gaps left by the
///    two higher-priority sources.
pub fn compose_sublead_env(
    lead_env: &HashMap<String, String>,
    operator_env: &HashMap<String, String>,
    permission_routing: crate::manifest::schema::PermissionRouting,
) -> HashMap<String, String> {
    let mut out = lead_env.clone();
    out.extend(operator_env.clone());
    crate::dispatch::runner::apply_pitboss_env_defaults(&mut out, permission_routing);
    out
}

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
    /// When set, pass `--resume <id>` to the sub-lead's claude subprocess so
    /// it continues a prior session. Populated by the root lead after
    /// `pitboss resume` seeds `/resume/subleads` in the shared store with
    /// prior session IDs. `None` for fresh spawns.
    pub resume_session_id: Option<String>,
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
/// - `max_total_workers`: projected total workers must not exceed cap.
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

    // ── Cap: max_total_workers ─────────────────────────────────────────
    if let Some(cap) = lead.and_then(|l| l.max_total_workers) {
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
                "spawn_sublead: max_total_workers cap {} would be exceeded \
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
    // API health gate: refuse new sub-leads while a recent rate-limit or
    // auth failure persists. Mirrors the check in `handle_spawn_worker`
    // so the lead can't sidestep the gate by routing through a new
    // sub-tree. See `dispatch::failure_detection` for the rules.
    if let Err(gate) = state.api_health.check_can_spawn().await {
        use crate::dispatch::failure_detection::SpawnGateReason;
        match gate {
            SpawnGateReason::RateLimited { retry_after } => {
                bail!(
                    "spawn_sublead: api rate-limited: refusing new sub-leads until {} (retry_after)",
                    retry_after.to_rfc3339()
                );
            }
            SpawnGateReason::AuthFailed { clears_at } => {
                bail!(
                    "spawn_sublead: api auth failed recently; refusing new sub-leads until {} \
                     (clears_at). Rotate credentials or cancel the run",
                    clears_at.to_rfc3339()
                );
            }
        }
    }

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
            // Read spent first (snapshot acceptable — it only grows), then
            // hold reserved_usd across the check-and-add so concurrent
            // spawn_sublead calls can't both pass the guard simultaneously.
            // Fixes the TOCTOU described in #106: two independent lock
            // snapshots + an unlocked check allowed both callers to pass
            // the guard before either wrote, enabling budget over-commit.
            let spent = *state.root.spent_usd.lock().await;
            let mut reserved = state.root.reserved_usd.lock().await;
            if spent + *reserved + amount > cap {
                bail!(
                    "spawn_sublead: budget exceeded: ${:.2} spent + ${:.2} reserved + ${:.2} estimated > ${:.2} budget",
                    spent,
                    *reserved,
                    amount,
                    cap
                );
            }
            *reserved += amount;
        } else {
            *state.root.reserved_usd.lock().await += amount;
        }
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
        sub_store.start_lease_pruner();

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

        // 8. Register sub-tree LayerState on root DispatchState. Inserts into
        // `state.subleads`, installs the per-sublead cancel-cascade watcher,
        // AND eagerly propagates any in-flight root drain/terminate signal
        // to `sub_layer.cancel`.  Centralizing these three coupled steps in
        // `DispatchState::register_sublead` keeps the post-#99 contract from
        // drifting; pinned by `tests/cancel_cascade_flows.rs`.
        state
            .register_sublead(sublead_id.clone(), sub_layer.clone())
            .await;

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

        // 9. Spawn the sub-lead's Claude session (wired in Task 2.3).
        spawn_sublead_session(
            state.clone(),
            sub_layer.clone(),
            req.prompt,
            req.model,
            envelope,
            req.env,
            req.tools,
            req.resume_session_id,
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
    resume_session_id: Option<String>,
) -> Result<()> {
    use crate::dispatch::hierarchical::build_sublead_mcp_config;
    use crate::dispatch::runner::sublead_spawn_args;
    use crate::mcp::server::socket_path_for_run;
    use pitboss_core::process::SpawnCmd;
    use pitboss_core::store::TaskStatus;

    let sublead_id = sub_layer.lead_id.clone();

    // 1. Build the MCP socket path (shared run-level socket — all actors
    //    connect here; mcp-bridge stamps _meta.actor_role=sublead).
    let socket_path = socket_path_for_run(sub_layer.run_id, &sub_layer.manifest.run_dir);

    // 2. Build per-sub-lead mcp-config.json.
    //
    // Mint the sublead's auth token here — the bridge will inject it
    // into every tools/call's `_meta.token`, and the server uses the
    // bound identity for authz. Closes #145 for the sublead path.
    let sublead_token = state.mint_token(&sublead_id, "sublead").await;
    let mcp_config_path = build_sublead_mcp_config(
        &sublead_id,
        &socket_path,
        &state.root.run_subdir,
        Some(&sublead_token),
        &state.root.manifest.mcp_servers,
    )
    .await
    .context("build sublead mcp-config")?;

    // 3. Build the CLI args: sublead toolset (or operator override),
    //    model, prompt, and optional --resume when continuing a prior session.
    let tools_for_args: Option<&[String]> = if tools_override.is_empty() {
        None
    } else {
        Some(&tools_override)
    };
    let spawn_routing = state
        .root
        .manifest
        .lead
        .as_ref()
        .map(|l| l.permission_routing)
        .unwrap_or_default();
    let args = sublead_spawn_args(
        &sublead_id,
        &prompt,
        &model,
        &mcp_config_path,
        resume_session_id.as_deref(),
        tools_for_args,
        spawn_routing,
    );

    // 4. Task log directory (mirrors workers' layout for consistency).
    let task_dir = sub_layer.run_subdir.join("tasks").join(&sublead_id);
    tokio::fs::create_dir_all(&task_dir).await.ok();
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");

    // 5. Build the spawn command.
    //
    //    CWD: the root lead's manifest `directory` (not its worktree path,
    //    even when the lead uses one). Rationale:
    //    - Claude gates file I/O on paths outside cwd. Previous default of
    //      `run_subdir` (under ~/.local/share/pitboss/runs/…) put every
    //      operator artifact (project files, $WORK_DIR, $ARTIFACTS_DIR)
    //      outside the sublead's trust zone, triggering a permission
    //      prompt on every read. Under `-p` headless mode the prompt is
    //      unanswerable; subleads effectively couldn't read project files.
    //    - Using `lead.directory` (not `lead_cwd` = possibly-worktree-path)
    //      keeps subleads independent of the lead's worktree lifecycle:
    //      subleads won't lose cwd if the lead's worktree is cleaned up
    //      while subleads are still running, and they see the committed
    //      project state rather than the lead's in-flight edits.
    //    - Workers spawned by subleads resolve their own cwd via
    //      `args.directory` → `lead.directory` fallback → optional
    //      per-worker worktree, so worker cwd is unchanged by this.
    //    Falls back to `run_subdir` when the manifest has no `[lead]`
    //    (flat-only hierarchical run — not a path that reaches
    //    spawn_sublead in practice, but we prefer a well-defined cwd over
    //    a panic).
    //
    //    Env precedence (lowest → highest): root lead's resolved env
    //    (which already merges [defaults.env] + [lead.env]) →
    //    operator-supplied env from the `spawn_sublead` MCP call →
    //    pitboss defaults (`CLAUDE_CODE_ENTRYPOINT=sdk-ts`, only fills
    //    gaps). Inheriting the lead's env means project-level path
    //    blocks like `[defaults.env]` (WORK_DIR/ARTIFACTS_DIR/etc.)
    //    reach subleads automatically; the lead doesn't have to re-pass
    //    them in every spawn_sublead call.
    let lead_env = state
        .root
        .manifest
        .lead
        .as_ref()
        .map(|l| l.env.clone())
        .unwrap_or_default();
    let routing = state
        .root
        .manifest
        .lead
        .as_ref()
        .map(|l| l.permission_routing)
        .unwrap_or_default();
    let sublead_env = compose_sublead_env(&lead_env, &operator_env, routing);
    let sublead_cwd = state
        .root
        .manifest
        .lead
        .as_ref()
        .map(|l| l.directory.clone())
        .unwrap_or_else(|| sub_layer.run_subdir.clone());
    let initial_cmd = SpawnCmd {
        program: sub_layer.claude_binary.clone(),
        args,
        cwd: sublead_cwd.clone(),
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
    let (reprompt_tx, reprompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
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
        // Run the kill+resume loop via the shared helper. The closure
        // captures sub-lead-specific resume-args / env / cwd resolution
        // (root lead has its own resume builder; see hierarchical.rs).
        // Cost is applied once after the loop returns (sum across
        // iterations) — see the helper's module doc-comment for why
        // the timing change is benign.
        let kr_result = crate::dispatch::kill_resume::run_kill_resume_loop(
            sub_layer_bg.clone(),
            crate::dispatch::kill_resume::KillResumeArgs {
                actor_id: sublead_id_bg.clone(),
                initial_cmd,
                timeout: Duration::from_secs(timeout_secs),
                log_path: log_path.clone(),
                stderr_path: stderr_path.clone(),
            },
            reprompt_rx,
            |sid, new_prompt| {
                let tools_for_resume: Option<&[String]> = if tools_override_bg.is_empty() {
                    None
                } else {
                    Some(&tools_override_bg)
                };
                let resume_routing = state_bg
                    .root
                    .manifest
                    .lead
                    .as_ref()
                    .map(|l| l.permission_routing)
                    .unwrap_or_default();
                let resume_args = sublead_spawn_args(
                    &sublead_id_bg,
                    new_prompt,
                    &model_bg,
                    &mcp_config_path_bg,
                    Some(sid),
                    tools_for_resume,
                    resume_routing,
                );
                // Env precedence: lead → operator → pitboss defaults
                // (see compose_sublead_env). Same cwd rationale as the
                // initial spawn: lead.directory (not lead_cwd) — see
                // the long comment in finalize_sublead_spawn.
                let lead_env_resume = state_bg
                    .root
                    .manifest
                    .lead
                    .as_ref()
                    .map(|l| l.env.clone())
                    .unwrap_or_default();
                let resume_env =
                    compose_sublead_env(&lead_env_resume, &operator_env_bg, resume_routing);
                let resume_cwd = state_bg
                    .root
                    .manifest
                    .lead
                    .as_ref()
                    .map(|l| l.directory.clone())
                    .unwrap_or_else(|| sub_layer_bg.run_subdir.clone());
                SpawnCmd {
                    program: sub_layer_bg.claude_binary.clone(),
                    args: resume_args,
                    cwd: resume_cwd,
                    env: resume_env,
                }
            },
        )
        .await;

        let final_outcome = kr_result.final_outcome;
        let overall_started_at = kr_result.overall_started_at;
        let total_token_usage = kr_result.total_token_usage;
        let reprompt_count = kr_result.reprompt_count;
        let last_session_id = kr_result.last_session_id;

        // Apply accumulated cost once. Per-iteration accumulation moved
        // here from inside the loop; benign (no worker spawn into the
        // sub-tree can occur between iterations because the sub-lead's
        // MCP session is closed while its subprocess is dead).
        if let Some(cost) = pitboss_core::prices::cost_usd(&model_bg, &total_token_usage) {
            *sub_layer_bg.spent_usd.lock().await += cost;
        }

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
        // sub-lead that called propose_plan, got auto_reject or a TTL
        // fallback, and exited would otherwise show as Success. See
        // run_worker for the same pattern.
        if matches!(sublead_outcome, SubleadOutcome::Success) {
            match state_bg.approval_driven_termination(&sublead_id_bg).await {
                Some(crate::dispatch::state::ApprovalTerminationKind::Rejected) => {
                    sublead_outcome = SubleadOutcome::ApprovalRejected;
                }
                Some(crate::dispatch::state::ApprovalTerminationKind::TimedOut) => {
                    sublead_outcome = SubleadOutcome::ApprovalTimedOut;
                }
                None => {}
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
            log_path: log_path.clone(),
            token_usage: total_token_usage,
            claude_session_id: final_outcome.claude_session_id,
            final_message_preview: final_outcome.final_message_preview,
            final_message: final_outcome.final_message,
            parent_task_id: Some(state_bg.root.lead_id.clone()),
            pause_count: 0,
            reprompt_count,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: Some(model_bg),
            failure_reason: crate::dispatch::failure_detection::detect_failure_reason(
                final_outcome.exit_code,
                Some(&log_path),
                Some(&stderr_path),
            ),
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

        // Persist the session_id mapping so `pitboss resume` can seed the
        // shared store with prior sub-lead session IDs on the next run.
        if let Some(ref sid) = last_session_id {
            let entry = serde_json::json!({
                "sublead_id": sublead_id_bg,
                "session_id": sid,
            });
            let subleads_jsonl = sub_layer_bg.run_subdir.join("subleads.jsonl");
            if let Ok(mut line) = serde_json::to_string(&entry) {
                line.push('\n');
                // Best-effort: failure to persist is not fatal. No explicit
                // flush — a process crash between write_all returning Ok and
                // the file being dropped can lose kernel-buffered bytes, but
                // that's accepted (resume is idempotent across re-runs). The
                // warn branch below catches I/O errors from write_all itself,
                // not a crash-after-write (#111). Use tokio::fs instead of
                // std::fs here because this closure runs on the tokio runtime;
                // a sync open+write would block a runtime worker thread (#98).
                let res: std::io::Result<()> = async {
                    use tokio::io::AsyncWriteExt;
                    let mut f = tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&subleads_jsonl)
                        .await?;
                    f.write_all(line.as_bytes()).await?;
                    Ok(())
                }
                .await;
                if let Err(e) = res {
                    tracing::warn!(
                        sublead_id = %sublead_id_bg,
                        error = %e,
                        "failed to append sublead session to subleads.jsonl"
                    );
                }
            }
        }

        // Broadcast classified failure from the sub-lead so the root TUI
        // sees why this branch of the tree died, and update api_health
        // so further spawns across the tree see the gate.
        if let Some(reason) = rec.failure_reason.clone() {
            state_bg.api_health.record(&reason).await;
            crate::dispatch::failure_detection::broadcast_worker_failed(
                &state_bg.root,
                sublead_id_bg.clone(),
                Some("root".into()),
                reason,
                &["root", sublead_id_bg.as_str()],
            )
            .await;
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

#[cfg(test)]
mod env_composition_tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn lead_env_propagates_to_sublead() {
        // Regression: operator reported [defaults.env] reaching the lead
        // but not the subleads. Lead-env must be the base layer.
        let lead = env(&[
            ("WORK_DIR", "/run/work"),
            ("ARTIFACTS_DIR", "/run/artifacts"),
            ("ADVENTURE_OUT", "/run/out"),
        ]);
        let out = compose_sublead_env(&lead, &HashMap::new(), Default::default());
        assert_eq!(out.get("WORK_DIR").map(String::as_str), Some("/run/work"));
        assert_eq!(
            out.get("ARTIFACTS_DIR").map(String::as_str),
            Some("/run/artifacts")
        );
        assert_eq!(
            out.get("ADVENTURE_OUT").map(String::as_str),
            Some("/run/out")
        );
    }

    #[test]
    fn operator_env_wins_over_lead_env() {
        // Spawn-site override beats inherited lead env. Lets a lead steer a
        // specific sublead to a different WORK_DIR without editing the manifest.
        let lead = env(&[("WORK_DIR", "/lead/path")]);
        let operator = env(&[("WORK_DIR", "/operator/path")]);
        let out = compose_sublead_env(&lead, &operator, Default::default());
        assert_eq!(
            out.get("WORK_DIR").map(String::as_str),
            Some("/operator/path")
        );
    }

    #[test]
    fn pitboss_defaults_fill_gaps_only() {
        // CLAUDE_CODE_ENTRYPOINT is only set when neither lead nor operator
        // supplied it. If either did, that value wins.
        let empty = HashMap::new();
        let out = compose_sublead_env(&empty, &empty, Default::default());
        assert_eq!(
            out.get("CLAUDE_CODE_ENTRYPOINT").map(String::as_str),
            Some("sdk-ts")
        );

        let lead = env(&[("CLAUDE_CODE_ENTRYPOINT", "cli")]);
        let out = compose_sublead_env(&lead, &HashMap::new(), Default::default());
        assert_eq!(
            out.get("CLAUDE_CODE_ENTRYPOINT").map(String::as_str),
            Some("cli"),
            "lead's explicit CLAUDE_CODE_ENTRYPOINT must not be clobbered by defaults"
        );
    }

    #[test]
    fn empty_lead_env_still_applies_pitboss_defaults() {
        // No [defaults.env] and no operator env → sublead still gets
        // CLAUDE_CODE_ENTRYPOINT so the MCP permission bypass engages.
        let out = compose_sublead_env(&HashMap::new(), &HashMap::new(), Default::default());
        assert!(out.contains_key("CLAUDE_CODE_ENTRYPOINT"));
    }
}
