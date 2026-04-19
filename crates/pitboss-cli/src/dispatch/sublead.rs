//! Sub-lead spawn and teardown helpers. A sub-lead is structurally a
//! Claude subprocess (like a worker) that ALSO has its own LayerState
//! (workers map, shared_store, approval queue). It spawns into the root's
//! sub-leads map; its workers spawn into its own LayerState.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

use crate::dispatch::actor::ActorId;
use crate::dispatch::layer::LayerState;
use crate::dispatch::state::DispatchState;
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
/// TODO(Task 5.1): wire up `manifest.lead.sublead_defaults` for default budget_usd,
/// max_workers, and lead_timeout_secs once that field is added to `ResolvedLead`.
///
/// TODO(Task 5.1): enforce `manifest.lead.max_sublead_budget_usd` and
/// `manifest.lead.max_workers_across_tree` caps once those fields are added.
pub fn resolve_envelope(
    // _manifest: underscore prefix is intentional — per-sublead cap enforcement
    // (max_sublead_budget_usd, max_workers_across_tree) is deferred to Task 5.1.
    // Do NOT remove the underscore or the parameter; the signature must remain
    // stable for when Task 5.1 wires up the cap checks.
    _manifest: &ResolvedManifest,
    req: &SubleadSpawnRequest,
) -> Result<ResolvedEnvelope> {
    // Shared-pool mode: read_down=true with no explicit resource allocation.
    if req.read_down && req.budget_usd.is_none() && req.max_workers.is_none() {
        return Ok(ResolvedEnvelope::SharedPool);
    }

    // Otherwise every resource field must be explicitly provided.
    // TODO(Task 5.1): fall back to sublead_defaults from [lead] block.
    let budget_usd = req
        .budget_usd
        .ok_or_else(|| anyhow!("budget_usd required when read_down=false"))?;

    let max_workers = req
        .max_workers
        .ok_or_else(|| anyhow!("max_workers required when read_down=false"))?;

    let lead_timeout_secs = req.lead_timeout_secs.unwrap_or(3600);

    // TODO(Task 5.1): enforce manifest.lead.max_sublead_budget_usd cap.
    // TODO(Task 5.1): enforce manifest.lead.max_workers_across_tree cap.

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
    let envelope = resolve_envelope(&state.root.manifest, &req)?;

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
        ));

        // 8. Register sub-tree LayerState on root DispatchState.
        state
            .subleads
            .write()
            .await
            .insert(sublead_id.clone(), sub_layer.clone());

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
