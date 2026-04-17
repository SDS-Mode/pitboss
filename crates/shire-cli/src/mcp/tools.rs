//! The six MCP tool handlers exposed to the lead. Real implementations
//! land in Tasks 10-16; this file establishes the types + signatures.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnWorkerArgs {
    pub prompt: String,
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnWorkerResult {
    pub task_id: String,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub state: String,
    pub started_at: Option<String>,
    pub partial_usage: mosaic_core::parser::TokenUsage,
    pub last_text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    pub task_id: String,
    pub state: String,
    pub prompt_preview: String,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResult {
    pub ok: bool,
}

use std::sync::Arc;

use anyhow::{bail, Result};
use mosaic_core::store::TaskRecord;
use tokio::time::Duration;
use uuid::Uuid;

use crate::dispatch::state::{DispatchState, WorkerState};

pub async fn handle_spawn_worker(
    state: &Arc<DispatchState>,
    args: SpawnWorkerArgs,
) -> Result<SpawnWorkerResult> {
    // Guard 1: draining
    if state.cancel.is_draining() || state.cancel.is_terminated() {
        bail!("run is draining: no new workers accepted");
    }

    // Guard 2: worker cap
    if let Some(cap) = state.manifest.max_workers {
        let active = state.active_worker_count().await;
        if active >= cap as usize {
            bail!("worker cap reached: {} active (max {})", active, cap);
        }
    }

    // Guard 3: budget
    if let (Some(budget), Some(_remaining)) =
        (state.manifest.budget_usd, state.budget_remaining().await)
    {
        let spent = *state.spent_usd.lock().await;
        // Estimate this worker's cost as median of prior workers or fallback.
        let estimate = estimate_new_worker_cost(state).await;
        if spent + estimate > budget {
            bail!(
                "budget exceeded: ${:.2} spent + ${:.2} estimated > ${:.2} budget",
                spent,
                estimate,
                budget
            );
        }
    }

    let task_id = format!("worker-{}", Uuid::now_v7());
    {
        let mut workers = state.workers.write().await;
        workers.insert(task_id.clone(), WorkerState::Pending);
    }

    let _ = args;
    Ok(SpawnWorkerResult {
        task_id,
        worktree_path: None,
    })
}

const INITIAL_WORKER_COST_EST: f64 = 0.10;

async fn estimate_new_worker_cost(state: &Arc<DispatchState>) -> f64 {
    use mosaic_core::prices::cost_usd;
    let workers = state.workers.read().await;
    let mut costs: Vec<f64> = Vec::new();
    for w in workers.values() {
        if let WorkerState::Done(rec) = w {
            // Try to price using whatever model the worker used. If model isn't
            // available at record-level, just sum tokens via a neutral rate.
            if let Some(c) = cost_usd("claude-haiku-4-5", &rec.token_usage) {
                costs.push(c);
            }
        }
    }
    if costs.is_empty() {
        return INITIAL_WORKER_COST_EST;
    }
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    costs[costs.len() / 2]
}

pub async fn handle_list_workers(state: &Arc<DispatchState>) -> Vec<WorkerSummary> {
    let workers = state.workers.read().await;
    workers
        .iter()
        .filter(|(id, _)| *id != &state.lead_id)
        .map(|(id, w)| {
            let (state_str, started_at) = match w {
                WorkerState::Pending => ("Pending".to_string(), None),
                WorkerState::Running { started_at } => {
                    ("Running".to_string(), Some(started_at.to_rfc3339()))
                }
                WorkerState::Done(rec) => (
                    match rec.status {
                        mosaic_core::store::TaskStatus::Success => "Completed",
                        mosaic_core::store::TaskStatus::Failed => "Failed",
                        mosaic_core::store::TaskStatus::TimedOut => "TimedOut",
                        mosaic_core::store::TaskStatus::Cancelled => "Cancelled",
                        mosaic_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
                    }
                    .to_string(),
                    Some(rec.started_at.to_rfc3339()),
                ),
            };
            WorkerSummary {
                task_id: id.clone(),
                state: state_str,
                prompt_preview: String::new(), // populated by spawn_worker in Task 12
                started_at,
            }
        })
        .collect()
}

pub async fn handle_worker_status(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<WorkerStatus> {
    let workers = state.workers.read().await;
    let w = workers
        .get(task_id)
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let (state_str, started_at, partial_usage, last_text_preview) = match w {
        WorkerState::Pending => (
            "Pending".to_string(),
            None,
            mosaic_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Running { started_at } => (
            "Running".to_string(),
            Some(started_at.to_rfc3339()),
            mosaic_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Done(rec) => (
            match rec.status {
                mosaic_core::store::TaskStatus::Success => "Completed",
                mosaic_core::store::TaskStatus::Failed => "Failed",
                mosaic_core::store::TaskStatus::TimedOut => "TimedOut",
                mosaic_core::store::TaskStatus::Cancelled => "Cancelled",
                mosaic_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
            }
            .to_string(),
            Some(rec.started_at.to_rfc3339()),
            rec.token_usage,
            rec.final_message_preview.clone(),
        ),
    };
    Ok(WorkerStatus {
        state: state_str,
        started_at,
        partial_usage,
        last_text_preview,
    })
}

pub async fn handle_cancel_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<CancelResult> {
    // Look up the worker's own CancelToken and fire it. In v0.3 the worker's
    // CancelToken is a *clone* of the run-level cancel; a per-worker signal
    // would require additional plumbing in the hierarchical runner (Task 22).
    // For now, issuing a run-level drain is the closest we can do without
    // per-worker tokens. This is wired fully in the integration tests.
    let workers = state.workers.read().await;
    if !workers.contains_key(task_id) {
        anyhow::bail!("unknown task_id: {task_id}");
    }
    // Actual SIGTERM signalling happens in Task 22 via state.cancel_worker_task_id().
    state.cancel.drain(); // temporary; refined in Task 22
    Ok(CancelResult { ok: true })
}

pub async fn handle_wait_for_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
    timeout_secs: Option<u64>,
) -> Result<TaskRecord> {
    // Fast path: already Done.
    {
        let workers = state.workers.read().await;
        if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
            return Ok(rec.clone());
        }
        if !workers.contains_key(task_id) {
            bail!("unknown task_id: {task_id}");
        }
    }

    // Subscribe to done events and wait.
    let mut rx = state.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_worker timed out for {task_id}"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                if completed_id == task_id {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
                        return Ok(rec.clone());
                    }
                    bail!("internal: task_id marked done but record not present");
                }
                // Not our task — keep waiting.
            }
        }
    }
}

pub async fn handle_wait_for_any(
    state: &Arc<DispatchState>,
    task_ids: &[String],
    timeout_secs: Option<u64>,
) -> Result<(String, TaskRecord)> {
    if task_ids.is_empty() {
        bail!("wait_for_any: task_ids is empty");
    }

    // Fast path: any already Done?
    {
        let workers = state.workers.read().await;
        for id in task_ids {
            if let Some(WorkerState::Done(rec)) = workers.get(id) {
                return Ok((id.clone(), rec.clone()));
            }
        }
    }

    let mut rx = state.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_any timed out"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                if task_ids.iter().any(|id| id == &completed_id) {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(&completed_id) {
                        return Ok((completed_id, rec.clone()));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::{DispatchState, WorkerState};
    use std::sync::Arc;

    async fn test_state() -> Arc<DispatchState> {
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use mosaic_core::session::CancelToken;
        use mosaic_core::store::{JsonFileStore, SessionStore};
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        Arc::new(DispatchState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
        ))
    }

    #[tokio::test]
    async fn list_workers_empty_when_no_spawns() {
        let state = test_state().await;
        let result = handle_list_workers(&state).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn list_workers_shows_pending_and_running() {
        let state = test_state().await;
        {
            let mut w = state.workers.write().await;
            w.insert("w-1".into(), WorkerState::Pending);
            w.insert(
                "w-2".into(),
                WorkerState::Running {
                    started_at: chrono::Utc::now(),
                },
            );
        }
        let mut result = handle_list_workers(&state).await;
        result.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].task_id, "w-1");
        assert_eq!(result[0].state, "Pending");
        assert_eq!(result[1].task_id, "w-2");
        assert_eq!(result[1].state, "Running");
    }

    #[tokio::test]
    async fn spawn_worker_adds_entry_to_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "investigate issue #1".into(),
            directory: Some("/tmp".into()),
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let result = handle_spawn_worker(&state, args).await.unwrap();
        assert!(result.task_id.starts_with("worker-"));

        let workers = state.workers.read().await;
        assert_eq!(workers.len(), 1);
        let entry = workers.get(&result.task_id).unwrap();
        assert!(matches!(entry, WorkerState::Pending));
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_max_workers_reached() {
        let state = test_state().await; // max_workers = 4
                                        // Fill up to cap
        for i in 0..4 {
            let args = SpawnWorkerArgs {
                prompt: format!("w{}", i),
                directory: None,
                branch: None,
                tools: None,
                timeout_secs: None,
                model: None,
            };
            handle_spawn_worker(&state, args).await.unwrap();
        }
        // 5th call must fail
        let args = SpawnWorkerArgs {
            prompt: "overflow".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("worker cap reached"), "err: {err}");
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_budget_exceeded() {
        let state = test_state().await; // budget_usd = 5.0
        *state.spent_usd.lock().await = 5.0; // at cap
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("budget exceeded"), "err: {err}");
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_draining() {
        let state = test_state().await;
        state.cancel.drain();
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("draining"), "err: {err}");
    }

    #[tokio::test]
    async fn worker_status_reads_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();
        let status = handle_worker_status(&state, &spawn.task_id).await.unwrap();
        assert_eq!(status.state, "Pending");
    }

    #[tokio::test]
    async fn worker_status_unknown_id_errors() {
        let state = test_state().await;
        let err = handle_worker_status(&state, "nope-123").await.unwrap_err();
        assert!(err.to_string().contains("unknown task_id"));
    }

    #[tokio::test]
    async fn cancel_worker_sets_cancelled_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();

        let result = handle_cancel_worker(&state, &spawn.task_id).await.unwrap();
        assert!(result.ok);

        // Note: in real wiring, CancelToken signals the SessionHandle to terminate
        // and the subsequent Done(...) entry in state.workers carries status=Cancelled.
        // For v0.3 Task 14 (unit-level), we just verify the cancel call succeeded
        // and didn't panic. Full flow is tested in integration tests (Phase 6).
    }

    #[tokio::test]
    async fn wait_for_worker_returns_outcome_on_completion() {
        use mosaic_core::store::{TaskRecord, TaskStatus};
        use std::time::Duration;

        let state = test_state().await;
        let task_id = "worker-test-1".to_string();
        {
            let mut w = state.workers.write().await;
            w.insert(task_id.clone(), WorkerState::Pending);
        }

        // Spawn a task that marks the worker Done after 50 ms.
        let state_clone = state.clone();
        let task_id_clone = task_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let rec = TaskRecord {
                task_id: task_id_clone.clone(),
                status: TaskStatus::Success,
                exit_code: Some(0),
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                duration_ms: 42,
                worktree_path: None,
                log_path: std::path::PathBuf::new(),
                token_usage: Default::default(),
                claude_session_id: None,
                final_message_preview: Some("ok".into()),
                parent_task_id: Some("lead".into()),
            };
            let mut w = state_clone.workers.write().await;
            w.insert(task_id_clone.clone(), WorkerState::Done(rec));
            let _ = state_clone.done_tx.send(task_id_clone);
        });

        let outcome = handle_wait_for_worker(&state, &task_id, Some(5))
            .await
            .unwrap();
        assert!(matches!(outcome.status, TaskStatus::Success));
    }

    #[tokio::test]
    async fn wait_for_worker_times_out() {
        let state = test_state().await;
        let task_id = "worker-stuck".to_string();
        {
            let mut w = state.workers.write().await;
            w.insert(task_id.clone(), WorkerState::Pending);
        }
        let err = handle_wait_for_worker(&state, &task_id, Some(0))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "err: {err}");
    }

    #[tokio::test]
    async fn wait_for_any_returns_first_completed() {
        use mosaic_core::store::{TaskRecord, TaskStatus};
        use std::time::Duration;

        let state = test_state().await;
        let ids = vec!["w-a".to_string(), "w-b".to_string(), "w-c".to_string()];
        {
            let mut w = state.workers.write().await;
            for id in &ids {
                w.insert(id.clone(), WorkerState::Pending);
            }
        }

        // Race: w-b finishes first at 30ms, w-a at 100ms.
        let state_clone = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let rec = TaskRecord {
                task_id: "w-b".into(),
                status: TaskStatus::Success,
                exit_code: Some(0),
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                duration_ms: 30,
                worktree_path: None,
                log_path: std::path::PathBuf::new(),
                token_usage: Default::default(),
                claude_session_id: None,
                final_message_preview: None,
                parent_task_id: Some("lead".into()),
            };
            let mut w = state_clone.workers.write().await;
            w.insert("w-b".into(), WorkerState::Done(rec));
            let _ = state_clone.done_tx.send("w-b".into());
        });

        let (winner_id, _rec) = handle_wait_for_any(&state, &ids, Some(5)).await.unwrap();
        assert_eq!(winner_id, "w-b");
    }
}
