//! Wait-side MCP handlers: `wait_for_worker`, `wait_for_actor`,
//! `wait_for_any`. All three share a single internal engine
//! `wait_for_actor_internal` that subscribes to `state.root.done_tx`
//! before any fast-path check (preventing the cross-layer
//! subscribe-after-completion race).

use std::sync::Arc;

use anyhow::{bail, Result};
use pitboss_core::store::TaskRecord;
use tokio::time::Duration;

use super::find_worker_across_layers;
use crate::dispatch::state::{ActorTerminalRecord, DispatchState, WorkerState};

async fn wait_for_actor_internal(
    state: &Arc<DispatchState>,
    actor_id: &str,
    timeout_secs: Option<u64>,
) -> Result<ActorTerminalRecord> {
    // ── Subscribe FIRST so a completion that races the fast-path checks
    //    is captured by the broadcast subscription. The original code
    //    subscribed AFTER the fast-path + existence checks, which works
    //    fine for root-lead callers (the worker can't complete in the
    //    microseconds between the existence check and the subscribe in
    //    practice) but exposed a real race once cross-layer worker
    //    visibility (commit d134289) let sub-leads call wait_actor on
    //    their own workers. Sub-lead callers' workers run very short
    //    (smoke-test workers finished in 12s) and their wait_actor calls
    //    are issued immediately after spawn_worker — giving the worker
    //    time to complete before the sublead's wait subscribes. The
    //    broadcast is missed and the wait blocks until either the
    //    timeout fires or the sublead itself is killed by lead_timeout.
    //    Subscribing first guarantees no completion is lost.
    let mut rx = state.root.done_tx.subscribe();

    // ── Fast path: already Done ────────────────────────────────────────────────
    // 1. Worker already Done? (scan all layers — the worker may be in a
    //    sub-lead's layer, not root.)
    if let Some(WorkerState::Done(rec)) = find_worker_across_layers(state, actor_id).await {
        return Ok(ActorTerminalRecord::Worker(rec));
    }
    // 2. Sub-lead already terminated?
    {
        let results = state.sublead_results.read().await;
        if let Some(rec) = results.get(actor_id) {
            return Ok(ActorTerminalRecord::Sublead(rec.clone()));
        }
    }

    // 3. Is actor_id known at all (worker in any layer OR active sub-lead)?
    {
        let subleads = state.subleads.read().await;
        let is_sublead = subleads.contains_key(actor_id);
        // Drop the subleads lock before scanning layers to avoid deadlock
        // (find_worker_across_layers re-acquires it in read mode — RwLock
        // permits multiple readers, but keep the surface tight).
        drop(subleads);
        let is_worker = find_worker_across_layers(state, actor_id).await.is_some();
        if !is_worker && !is_sublead {
            bail!("unknown actor_id: {actor_id}");
        }
    }

    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_actor timed out for {actor_id}"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                if completed_id == actor_id {
                    // Check workers first (all layers — sub-lead-owned
                    // workers aren't in root's workers map).
                    if let Some(WorkerState::Done(rec)) =
                        find_worker_across_layers(state, actor_id).await
                    {
                        return Ok(ActorTerminalRecord::Worker(rec));
                    }
                    // Then check sublead_results.
                    {
                        let results = state.sublead_results.read().await;
                        if let Some(rec) = results.get(actor_id) {
                            return Ok(ActorTerminalRecord::Sublead(rec.clone()));
                        }
                    }
                    bail!("internal: actor_id marked done but record not present");
                }
                // Defensive: our target may actually be Done now; re-check
                // across all layers.
                if let Some(WorkerState::Done(rec)) =
                    find_worker_across_layers(state, actor_id).await
                {
                    return Ok(ActorTerminalRecord::Worker(rec));
                }
                {
                    let results = state.sublead_results.read().await;
                    if let Some(rec) = results.get(actor_id) {
                        return Ok(ActorTerminalRecord::Sublead(rec.clone()));
                    }
                }
                // Not our actor and target not yet done — keep waiting.
            }
        }
    }
}

pub async fn handle_wait_for_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
    timeout_secs: Option<u64>,
) -> Result<TaskRecord> {
    match wait_for_actor_internal(state, task_id, timeout_secs).await? {
        ActorTerminalRecord::Worker(rec) => Ok(rec),
        ActorTerminalRecord::Sublead(_) => {
            bail!("internal: wait_for_worker called with a sub-lead id; use wait_actor instead")
        }
    }
}

pub async fn handle_wait_for_actor(
    state: &Arc<DispatchState>,
    actor_id: &str,
    timeout_secs: Option<u64>,
) -> Result<ActorTerminalRecord> {
    wait_for_actor_internal(state, actor_id, timeout_secs).await
}

pub async fn handle_wait_for_any(
    state: &Arc<DispatchState>,
    task_ids: &[String],
    timeout_secs: Option<u64>,
) -> Result<(String, TaskRecord)> {
    if task_ids.is_empty() {
        bail!("wait_for_any: task_ids is empty");
    }

    // Subscribe FIRST so a completion that races the fast-path check is
    // captured by the broadcast subscription. Same race fix pattern as
    // wait_for_actor_internal (commit 70e2bd8). Without this, a worker
    // that terminated between our existence check and our subscribe is
    // lost and we block until timeout.
    let mut rx = state.root.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    // Fast path: any already Done? Scan ALL layers — sub-lead-spawned
    // workers are registered in the sub-lead layer's workers map, not
    // root's. Pre-fix, this only checked `state.root.workers.read()` (= root
    // via Deref) and so stayed blocked indefinitely on sub-lead-owned
    // workers. Same bug class as commit d134289 (which fixed wait_actor
    // + list_workers + worker_status) but this handler had its own
    // lookup code that was missed by that fix.
    let mut any_known = false;
    for id in task_ids {
        match find_worker_across_layers(state, id).await {
            Some(WorkerState::Done(rec)) => return Ok((id.clone(), rec)),
            Some(_) => any_known = true,
            None => {}
        }
    }
    // #151 M1: if every task_id is unknown (typo, evicted, never spawned),
    // there is no possible source of a `done_tx` event matching them, so
    // the loop below would block for the full `timeout_secs` (default
    // 3600s) waking only on unrelated broadcasts. Fail fast with a
    // diagnostic naming the unknown ids.
    if !any_known {
        bail!(
            "wait_for_any: none of the requested task_ids are known to the dispatcher \
             (typo, evicted, or never spawned): {}",
            task_ids.join(", ")
        );
    }

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_any timed out"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                // Primary path: our target completed.
                if task_ids.iter().any(|id| id == &completed_id) {
                    if let Some(WorkerState::Done(rec)) =
                        find_worker_across_layers(state, &completed_id).await
                    {
                        return Ok((completed_id, rec));
                    }
                }
                // Defensive re-scan: a prior broadcast we missed, or a
                // write-ordering race, might mean one of our targets is
                // actually Done now even though the recv'd id isn't in
                // our set. Cheap to check; returns only if found.
                for id in task_ids {
                    if let Some(WorkerState::Done(rec)) = find_worker_across_layers(state, id).await
                    {
                        return Ok((id.clone(), rec));
                    }
                }
            }
        }
    }
}
