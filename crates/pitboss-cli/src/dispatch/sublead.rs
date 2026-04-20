//! Sub-lead spawn and teardown helpers. A sub-lead is structurally a
//! Claude subprocess (like a worker) that ALSO has its own LayerState
//! (workers map, shared_store, approval queue). It spawns into the root's
//! sub-leads map; its workers spawn into its own LayerState.

use std::collections::HashMap;
use std::sync::Arc;

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

/// Configuration for a sub-lead spawn, validated against root's caps.
#[derive(Debug, Clone)]
pub struct SubleadSpawnRequest {
    pub prompt: String,
    pub model: String,
    pub budget_usd: Option<f64>,
    pub max_workers: Option<u32>,
    pub lead_timeout_secs: Option<u64>,
    pub initial_ref: HashMap<String, Value>,
    pub read_down: bool,
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
/// sub-tree LayerState, seeds `/ref/*` from `req.initial_ref`, and registers
/// the sub-tree on `state.subleads`. Returns the new sublead_id.
///
/// The sub-lead's Claude session is wired in Task 2.3; for Task 2.2 the call
/// to `spawn_sublead_session` is an intentional no-op stub.
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

        // 9. Spawn the sub-lead's Claude session (stub — full wiring in Task 2.3).
        spawn_sublead_session(
            state.clone(),
            sub_layer.clone(),
            req.prompt,
            req.model,
            envelope,
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

/// Spawn the sub-lead's Claude subprocess as a worker-of-root so its
/// lifecycle is tracked for cancellation and wait purposes.
///
/// Task 2.2 stub — full session wiring (per-sub-tree MCP socket, restricted
/// toolset without `spawn_sublead`) lands in Task 2.3.
async fn spawn_sublead_session(
    _state: Arc<DispatchState>,
    _sub_layer: Arc<LayerState>,
    _prompt: String,
    _model: String,
    _envelope: ResolvedEnvelope,
) -> Result<()> {
    // TODO(Task 2.3): spawn the sub-lead's Claude subprocess via state.spawner
    // with the restricted toolset (no spawn_sublead) and per-sub-tree MCP socket
    // binding.
    Ok(())
}

/// Called when a sub-lead emits its terminal Event::Result (or when
/// it's cancelled / times out). Reconciles the sub-tree's actual spend
/// against the original reservation: spend is moved into root's
/// spent_usd, and any unspent reservation is released back to root's
/// reservable pool.
///
/// Idempotent: calling twice for the same sublead_id is a no-op the
/// second time (sub-tree LayerState is removed on first call).
///
/// The original reservation amount is read from `sub_layer.original_reservation_usd`,
/// which was set when the sub-layer was constructed by `spawn_sublead`.
///
/// TODO(Task 2.3/Task 3.x): Wire the call site in the dispatch event loop
/// (hierarchical.rs or runner.rs). When a sub-lead's Claude subprocess emits
/// its terminal Done event and flows through wait_actor, call this function
/// with the sub-lead's id. The original reservation is now read from the
/// sub-tree's LayerState.original_reservation_usd field. For Phase 2 (no real
/// sub-lead session yet), the test calls the helper directly; the integration
/// lands when full sub-lead session wiring (Task 2.3) is complete.
pub async fn reconcile_terminated_sublead(
    state: &Arc<DispatchState>,
    sublead_id: &str,
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
        "sub-lead budget reconciled"
    );

    // Emit SubleadTerminated lifecycle event to the control plane.
    // TODO(Task 2.3): thread the actual terminal outcome ("success" |
    // "cancel" | "timeout" | "error") from the sub-lead's Claude session
    // exit status. For now we default to "success" since reconcile is
    // only called on clean completion in the current stub.
    {
        let ev = EventEnvelope {
            actor_path: ActorPath::new(["root", sublead_id]),
            event: ControlEvent::SubleadTerminated {
                sublead_id: sublead_id.to_string(),
                spent_usd: actual_spend,
                unspent_usd: unspent,
                outcome: "success".to_string(),
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
        // TODO(Task 2.3): thread real outcome from sub-lead's Claude session
        // exit status. For now all reconciled sub-leads are "success".
        outcome: "success".to_string(),
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
