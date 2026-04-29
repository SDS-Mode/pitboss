//! Worker lifecycle handlers: list / status / cancel / pause / continue / reprompt.
//!
//! Mutating handlers (cancel/pause/continue/reprompt) all route through
//! `super::layer_for_worker` so sub-tree workers land in their owning
//! sub-lead's `LayerState` rather than always reading/writing root
//! (issue #146).

use std::sync::Arc;

use anyhow::Result;

use super::spawn::spawn_resume_worker;
use super::{
    find_worker_across_layers, layer_for_worker, CancelResult, ContinueWorkerArgs, PauseMode,
    RepromptWorkerArgs, WorkerStatus, WorkerSummary,
};
use crate::dispatch::state::{DispatchState, WorkerState};

pub async fn handle_list_workers(state: &Arc<DispatchState>) -> Vec<WorkerSummary> {
    // Collect from every layer (root + each active sub-lead). Previously
    // this read only `state.root.workers` (= root via Deref), so sub-leads
    // calling `list_workers` got an empty list even when they had their
    // own workers active — the workers were registered in the sub-lead
    // layer's own `workers` map by `handle_spawn_worker`'s
    // `target_layer.workers.write()`.
    //
    // Lead id filtering: excludes the root lead id. Sub-lead ids aren't
    // registered as workers so they don't need filtering here.
    let mut summaries: Vec<WorkerSummary> = Vec::new();
    let prompts = state.root.worker_prompts.read().await;
    let render = |id: &String, w: &WorkerState| -> WorkerSummary {
        let (state_str, started_at) = match w {
            WorkerState::Pending => ("Pending".to_string(), None),
            WorkerState::Running { started_at, .. } => {
                ("Running".to_string(), Some(started_at.to_rfc3339()))
            }
            WorkerState::Paused { paused_at, .. } => {
                ("Paused".to_string(), Some(paused_at.to_rfc3339()))
            }
            WorkerState::Frozen { started_at, .. } => {
                ("Frozen".to_string(), Some(started_at.to_rfc3339()))
            }
            WorkerState::Done(rec) => (
                match rec.status {
                    pitboss_core::store::TaskStatus::Success => "Completed",
                    pitboss_core::store::TaskStatus::Failed => "Failed",
                    pitboss_core::store::TaskStatus::TimedOut => "TimedOut",
                    pitboss_core::store::TaskStatus::Cancelled => "Cancelled",
                    pitboss_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
                    pitboss_core::store::TaskStatus::ApprovalRejected => "ApprovalRejected",
                    pitboss_core::store::TaskStatus::ApprovalTimedOut => "ApprovalTimedOut",
                }
                .to_string(),
                Some(rec.started_at.to_rfc3339()),
            ),
        };
        WorkerSummary {
            task_id: id.clone(),
            state: state_str,
            prompt_preview: prompts.get(id).cloned().unwrap_or_default(),
            started_at,
        }
    };
    for (id, w) in state.root.workers.read().await.iter() {
        if id != &state.root.lead_id {
            summaries.push(render(id, w));
        }
    }
    let subleads = state.subleads.read().await;
    for layer in subleads.values() {
        for (id, w) in layer.workers.read().await.iter() {
            // Sub-lead layers hold the sub-lead itself as a "worker" entry
            // (the claude subprocess registered via workers.write() in
            // finalize_sublead_spawn). Filter by layer.lead_id so the
            // sub-lead doesn't show up as one of its own workers.
            if id != &layer.lead_id {
                summaries.push(render(id, w));
            }
        }
    }
    summaries
}

pub async fn handle_worker_status(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<WorkerStatus> {
    // Scan all layers — same rationale as find_worker_across_layers:
    // a sub-lead's own workers are registered in the sub-lead's layer.
    let w = find_worker_across_layers(state, task_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let (state_str, started_at, partial_usage, last_text_preview) = match &w {
        WorkerState::Pending => (
            "Pending".to_string(),
            None,
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Running { started_at, .. } => (
            "Running".to_string(),
            Some(started_at.to_rfc3339()),
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Paused {
            paused_at,
            prior_token_usage,
            ..
        } => (
            "Paused".to_string(),
            Some(paused_at.to_rfc3339()),
            *prior_token_usage,
            None,
        ),
        WorkerState::Frozen { started_at, .. } => (
            "Frozen".to_string(),
            Some(started_at.to_rfc3339()),
            // The child is still alive and its counters haven't been
            // snapshotted at freeze time (partial_usage is populated by
            // Done records). Report zeros rather than inventing a value.
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Done(rec) => (
            match rec.status {
                pitboss_core::store::TaskStatus::Success => "Completed",
                pitboss_core::store::TaskStatus::Failed => "Failed",
                pitboss_core::store::TaskStatus::TimedOut => "TimedOut",
                pitboss_core::store::TaskStatus::Cancelled => "Cancelled",
                pitboss_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
                pitboss_core::store::TaskStatus::ApprovalRejected => "ApprovalRejected",
                pitboss_core::store::TaskStatus::ApprovalTimedOut => "ApprovalTimedOut",
            }
            .to_string(),
            Some(rec.started_at.to_rfc3339()),
            rec.token_usage,
            rec.final_message_preview.clone(),
        ),
    };
    // #151 L2: read prompt_preview from the *owning* layer, not just
    // root. Sub-tree workers' prompts live in their layer's
    // `worker_prompts`; pre-fix, `handle_worker_status` only checked
    // root and so returned an empty prompt_preview for every
    // sub-lead-spawned worker. Falls back to root for compat in case
    // an older record landed there.
    let prompt_preview = if let Some(layer) = layer_for_worker(state, task_id).await {
        layer
            .worker_prompts
            .read()
            .await
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    } else {
        state
            .root
            .worker_prompts
            .read()
            .await
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    };
    Ok(WorkerStatus {
        state: state_str,
        started_at,
        partial_usage,
        last_text_preview,
        prompt_preview,
    })
}

pub async fn handle_cancel_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<CancelResult> {
    // Resolve the owning layer (root OR sub-lead) — a sub-lead-owned
    // worker is registered in its sub-tree's `LayerState`, not root.
    // See `layer_for_worker` (issue #146).
    let layer = layer_for_worker(state, task_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let cancels = layer.worker_cancels.read().await;
    let Some(token) = cancels.get(task_id) else {
        anyhow::bail!("unknown task_id: {task_id}");
    };
    token.terminate();
    Ok(CancelResult { ok: true })
}

pub async fn handle_pause_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
    mode: PauseMode,
) -> Result<CancelResult> {
    // Resolve the owning layer up front — sub-tree workers live in
    // their sub-lead's `LayerState`, not root (issue #146). Reading the
    // wrong layer here silently no-op'd pause for sub-tree workers.
    let layer = layer_for_worker(state, task_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let mut workers = layer.workers.write().await;
    let Some(entry) = workers.get(task_id).cloned() else {
        anyhow::bail!("unknown task_id: {task_id}");
    };
    match entry {
        WorkerState::Running {
            started_at,
            session_id: Some(sid),
        } => match mode {
            PauseMode::Cancel => {
                let cancels = layer.worker_cancels.read().await;
                if let Some(tok) = cancels.get(task_id) {
                    tok.terminate();
                }
                workers.insert(
                    task_id.to_string(),
                    WorkerState::Paused {
                        session_id: sid,
                        paused_at: chrono::Utc::now(),
                        prior_token_usage: Default::default(),
                    },
                );
                Ok(CancelResult { ok: true })
            }
            PauseMode::Freeze => {
                // Read the pid slot. If 0 (subprocess hasn't spawned
                // yet), fail — freeze is meaningless without a pid.
                let pid = layer
                    .worker_pids
                    .read()
                    .await
                    .get(task_id)
                    .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
                    .unwrap_or(0);
                if pid == 0 {
                    anyhow::bail!("cannot freeze {task_id}: worker pid unknown (race with spawn?)");
                }
                crate::dispatch::signals::freeze(pid)?;
                workers.insert(
                    task_id.to_string(),
                    WorkerState::Frozen {
                        session_id: sid,
                        frozen_at: chrono::Utc::now(),
                        started_at,
                    },
                );
                Ok(CancelResult { ok: true })
            }
        },
        WorkerState::Running {
            session_id: None, ..
        } => anyhow::bail!("worker not yet initialized (no session_id)"),
        WorkerState::Paused { .. } => anyhow::bail!("worker already paused"),
        WorkerState::Frozen { .. } => anyhow::bail!("worker already frozen"),
        _ => anyhow::bail!("worker not in a pausable state"),
    }
}

pub async fn handle_continue_worker(
    state: &Arc<DispatchState>,
    args: ContinueWorkerArgs,
) -> Result<CancelResult> {
    // Resolve the owning layer (sub-tree workers live in their sub-lead's
    // LayerState — see `layer_for_worker`, issue #146).
    let layer = layer_for_worker(state, &args.task_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {}", args.task_id))?;
    let current = layer.workers.read().await.get(&args.task_id).cloned();
    match current {
        Some(WorkerState::Paused { session_id, .. }) => {
            let prompt = args.prompt.unwrap_or_else(|| "continue".into());
            spawn_resume_worker(state, args.task_id, prompt, session_id).await?;
            Ok(CancelResult { ok: true })
        }
        Some(WorkerState::Frozen {
            session_id,
            started_at,
            ..
        }) => {
            // SIGCONT the process in place — no respawn, no session
            // replay. The subprocess picks up exactly where it left
            // off. `prompt` is silently ignored in freeze mode (it's
            // a resume-only concept); clients that want to inject a
            // new prompt should thaw + reprompt as two steps.
            let pid = layer
                .worker_pids
                .read()
                .await
                .get(&args.task_id)
                .map(|slot| slot.load(std::sync::atomic::Ordering::Acquire))
                .unwrap_or(0);
            if pid == 0 {
                anyhow::bail!(
                    "cannot thaw {}: pid slot empty (race with exit?)",
                    args.task_id
                );
            }
            crate::dispatch::signals::resume_stopped(pid)?;
            // Transition back to Running, preserving the ORIGINAL
            // started_at so wall-clock duration stays accurate.
            layer.workers.write().await.insert(
                args.task_id.clone(),
                WorkerState::Running {
                    started_at,
                    session_id: Some(session_id),
                },
            );
            Ok(CancelResult { ok: true })
        }
        Some(_) => anyhow::bail!("worker not paused"),
        None => anyhow::bail!("unknown task_id: {}", args.task_id),
    }
}

pub async fn handle_reprompt_worker(
    state: &Arc<DispatchState>,
    args: RepromptWorkerArgs,
) -> Result<CancelResult> {
    // Resolve the owning layer (issue #146: sub-tree workers were
    // unreachable to reprompt because all reads/writes were hard-coded
    // to root).
    let layer = layer_for_worker(state, &args.task_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {}", args.task_id))?;
    let current = layer.workers.read().await.get(&args.task_id).cloned();
    let session_id = match current {
        Some(WorkerState::Running {
            session_id: Some(sid),
            ..
        }) => {
            let cancels = layer.worker_cancels.read().await;
            if let Some(tok) = cancels.get(&args.task_id) {
                tok.terminate();
            }
            // Brief grace so the prior subprocess exits before spawn_resume
            // starts the new one. Matches the control-socket op.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            sid
        }
        Some(WorkerState::Paused { session_id, .. }) => session_id,
        Some(WorkerState::Frozen { .. }) => {
            anyhow::bail!("worker is frozen; continue_worker (SIGCONT) it first before reprompting")
        }
        Some(WorkerState::Running {
            session_id: None, ..
        }) => anyhow::bail!("worker not yet initialized (no session_id)"),
        Some(WorkerState::Pending) => anyhow::bail!("worker is still pending"),
        Some(WorkerState::Done(_)) => anyhow::bail!("worker already completed"),
        None => anyhow::bail!("unknown task_id: {}", args.task_id),
    };

    // Unconditionally record the reprompt attempt — audit trail even if
    // the subsequent spawn fails. The events directory is owned by the
    // layer that registered the worker (sub-tree workers' events live
    // under the sub-lead's run_subdir).
    let _ = crate::dispatch::events::append_event(
        &layer.run_subdir,
        &args.task_id,
        &crate::dispatch::events::TaskEvent::Reprompt {
            at: chrono::Utc::now(),
            prompt_preview: args.prompt.chars().take(80).collect(),
            prior_session_id: session_id.clone(),
        },
    )
    .await;

    spawn_resume_worker(state, args.task_id.clone(), args.prompt, session_id).await?;

    // Counter bump is conditional on spawn success so a failed spawn
    // doesn't falsely inflate the reprompt count. Bump the counter on
    // the OWNING layer, not root.
    layer
        .worker_counters
        .write()
        .await
        .entry(args.task_id)
        .or_default()
        .reprompt_count += 1;

    Ok(CancelResult { ok: true })
}
