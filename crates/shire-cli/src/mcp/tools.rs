//! The six MCP tool handlers exposed to the lead. Real implementations
//! land in Tasks 10-16; this file establishes the types + signatures.

#![allow(dead_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpawnWorkerResult {
    pub task_id: String,
    pub worktree_path: Option<String>,
}

/// Local JsonSchema mirror for `mosaic_core::parser::TokenUsage`.
///
/// `mosaic-core` does not depend on `schemars`, so we can't derive `JsonSchema`
/// on the upstream type without adding a new dep to a low-level crate. This
/// struct lives here purely to satisfy the schema derivation for `WorkerStatus`
/// via `#[schemars(with = "TokenUsageSchema")]` — the actual field is still
/// `mosaic_core::parser::TokenUsage` at the type level, and `Serialize` /
/// `Deserialize` are wire-compatible because the field layout matches.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct TokenUsageSchema {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerStatus {
    pub state: String,
    pub started_at: Option<String>,
    #[schemars(with = "TokenUsageSchema")]
    pub partial_usage: mosaic_core::parser::TokenUsage,
    pub last_text_preview: Option<String>,
    #[serde(default)]
    pub prompt_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerSummary {
    pub task_id: String,
    pub state: String,
    pub prompt_preview: String,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CancelResult {
    pub ok: bool,
}

// ---- Tool arg wrappers (for tools that take primitive or multi-arg input) ----
//
// The rmcp tool macros use `Parameters<T>` where T: JsonSchema to deserialize
// arguments from an incoming JSON object. We define small wrapper structs for
// each tool whose args aren't already represented by one of the structs above.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskIdArgs {
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitForWorkerArgs {
    pub task_id: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitForAnyArgs {
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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

    let worker_cancel = mosaic_core::session::CancelToken::new();
    state
        .worker_cancels
        .write()
        .await
        .insert(task_id.clone(), worker_cancel);

    // Record the prompt preview before spawning the background task.
    let prompt_preview: String = args.prompt.chars().take(80).collect();
    state
        .worker_prompts
        .write()
        .await
        .insert(task_id.clone(), prompt_preview);

    // Resolve the worker's directory: args override -> lead.directory fallback.
    let worker_dir: std::path::PathBuf = args
        .directory
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            state
                .manifest
                .lead
                .as_ref()
                .map(|l| l.directory.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        });

    // Resolve model, tools, timeout: per-args override -> lead defaults -> fallback.
    let lead = state.manifest.lead.as_ref();
    let worker_model = args
        .model
        .clone()
        .or_else(|| lead.map(|l| l.model.clone()))
        .unwrap_or_else(|| "claude-haiku-4-5".to_string());
    let worker_tools = args
        .tools
        .clone()
        .or_else(|| lead.map(|l| l.tools.clone()))
        .unwrap_or_default();
    let worker_timeout_secs = args
        .timeout_secs
        .or_else(|| lead.map(|l| l.timeout_secs))
        .unwrap_or(3600);
    let worker_branch = args.branch.clone();
    let worker_use_worktree = lead.is_none_or(|l| l.use_worktree);

    // Retrieve the per-worker cancel token we inserted above.
    let worker_cancel_bg = state
        .worker_cancels
        .read()
        .await
        .get(&task_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("internal: worker_cancel missing after insert"))?;

    let state_bg = Arc::clone(state);
    let task_id_bg = task_id.clone();
    let lead_id_bg = state.lead_id.clone();
    let prompt_bg = args.prompt.clone();

    tokio::spawn(async move {
        run_worker(
            state_bg,
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
    task_id: String,
    lead_id: String,
    prompt: String,
    directory: std::path::PathBuf,
    branch: Option<String>,
    model: String,
    tools: Vec<String>,
    timeout_secs: u64,
    use_worktree: bool,
    cancel: mosaic_core::session::CancelToken,
) {
    use chrono::Utc;
    use mosaic_core::process::SpawnCmd;
    use mosaic_core::session::SessionHandle;
    use mosaic_core::store::TaskStatus;
    use std::time::Duration;

    let task_dir = state.run_subdir.join("tasks").join(&task_id);
    let _ = tokio::fs::create_dir_all(&task_dir).await;
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");

    // Optional worktree prep.
    let mut worktree_handle: Option<mosaic_core::worktree::Worktree> = None;
    let cwd = if use_worktree {
        let name = format!("shire-worker-{}-{}", task_id, state.run_id);
        match state.wt_mgr.prepare(&directory, &name, branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                worktree_handle = Some(wt);
                p
            }
            Err(e) => {
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
                    parent_task_id: Some(lead_id),
                };
                let _ = state.store.append_record(state.run_id, &rec).await;
                state
                    .workers
                    .write()
                    .await
                    .insert(task_id.clone(), WorkerState::Done(rec));
                let _ = state.done_tx.send(task_id);
                return;
            }
        }
    } else {
        directory.clone()
    };

    // Transition Pending → Running.
    state.workers.write().await.insert(
        task_id.clone(),
        WorkerState::Running {
            started_at: Utc::now(),
        },
    );

    let cmd = SpawnCmd {
        program: state.claude_binary.clone(),
        args: worker_spawn_args(&prompt, &model, &tools),
        cwd: cwd.clone(),
        env: Default::default(),
    };

    let outcome = SessionHandle::new(task_id.clone(), Arc::clone(&state.spawner), cmd)
        .with_log_path(log_path.clone())
        .with_stderr_log_path(stderr_path)
        .run_to_completion(cancel, Duration::from_secs(timeout_secs))
        .await;

    let status = match outcome.final_state {
        mosaic_core::session::SessionState::Completed => TaskStatus::Success,
        mosaic_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
        mosaic_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
        mosaic_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
        mosaic_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
        _ => TaskStatus::Failed,
    };

    // Cleanup worktree per policy.
    if let Some(wt) = worktree_handle {
        let succeeded = matches!(status, TaskStatus::Success);
        let _ = state.wt_mgr.cleanup(wt, state.cleanup_policy, succeeded);
    }

    let worktree_path = if use_worktree { Some(cwd) } else { None };
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
        parent_task_id: Some(lead_id),
    };

    // Persist record.
    let _ = state.store.append_record(state.run_id, &rec).await;

    // Accumulate cost into spent_usd.
    if let Some(cost) = mosaic_core::prices::cost_usd(&model, &rec.token_usage) {
        *state.spent_usd.lock().await += cost;
    }

    // Transition to Done + broadcast.
    state
        .workers
        .write()
        .await
        .insert(task_id.clone(), WorkerState::Done(rec));
    let _ = state.done_tx.send(task_id);
}

fn worker_spawn_args(prompt: &str, model: &str, tools: &[String]) -> Vec<String> {
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if !tools.is_empty() {
        args.push("--allowedTools".into());
        args.push(tools.join(","));
    }
    args.push("--model".into());
    args.push(model.to_string());
    args.push("-p".into());
    args.push(prompt.to_string());
    args
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
    let prompts = state.worker_prompts.read().await;
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
            let prompt_preview = prompts.get(id).cloned().unwrap_or_default();
            WorkerSummary {
                task_id: id.clone(),
                state: state_str,
                prompt_preview,
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
    let prompt_preview = state
        .worker_prompts
        .read()
        .await
        .get(task_id)
        .cloned()
        .unwrap_or_default();
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
    let cancels = state.worker_cancels.read().await;
    let Some(token) = cancels.get(task_id) else {
        anyhow::bail!("unknown task_id: {task_id}");
    };
    token.terminate();
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
                // Defensive: our target may actually be Done now; re-check.
                let workers = state.workers.read().await;
                if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
                    return Ok(rec.clone());
                }
                // Not our task and target not yet done — keep waiting.
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
                // Primary path: our target completed.
                if task_ids.iter().any(|id| id == &completed_id) {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(&completed_id) {
                        return Ok((completed_id, rec.clone()));
                    }
                }
                // Defensive re-scan: a prior broadcast we missed, or a write-ordering race,
                // might mean one of our targets is actually Done now even though the recv'd
                // id isn't in our set. Cheap to check; returns only if found.
                let workers = state.workers.read().await;
                for id in task_ids {
                    if let Some(WorkerState::Done(rec)) = workers.get(id) {
                        return Ok((id.clone(), rec.clone()));
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
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use mosaic_core::process::fake::{FakeScript, FakeSpawner};
        use mosaic_core::process::ProcessSpawner;
        use mosaic_core::session::CancelToken;
        use mosaic_core::store::{JsonFileStore, SessionStore};
        use mosaic_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        // Minimal lead that turns off worktree prep so the background worker
        // spawn path doesn't require a real git repo to run against.
        let lead = ResolvedLead {
            id: "lead".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "lead prompt".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 3600,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        // Use a FakeSpawner that holds its children open until terminated.
        // This keeps spawned workers in the Running state throughout the test
        // (rather than transitioning to Done quickly as TokioSpawner + /bin/true
        // would), which keeps the `active_worker_count()` guard deterministic.
        let script = FakeScript::new().hold_until_signal();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        // Leak the TempDir — the state holds paths into it and the test
        // may spawn background workers that write logs inside it.
        let dir_path = dir.path().to_path_buf();
        std::mem::forget(dir);
        let _ = dir_path;
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
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

        // The background task may have already transitioned the worker to
        // Running or Done by the time we read, so we just assert the key
        // exists and is in a valid state (Pending / Running / Done).
        let workers = state.workers.read().await;
        assert_eq!(workers.len(), 1);
        let entry = workers.get(&result.task_id).unwrap();
        assert!(matches!(
            entry,
            WorkerState::Pending | WorkerState::Running { .. } | WorkerState::Done(_)
        ));

        // Verify prompt_preview was recorded.
        let prompts = state.worker_prompts.read().await;
        assert_eq!(
            prompts.get(&result.task_id).unwrap(),
            "investigate issue #1"
        );
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
            prompt: "investigate bug".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();
        let status = handle_worker_status(&state, &spawn.task_id).await.unwrap();
        // The background task may have already transitioned the worker to
        // Running; we accept either state here. Done is not expected because
        // the test FakeSpawner holds its children open until signalled.
        assert!(
            matches!(status.state.as_str(), "Pending" | "Running"),
            "unexpected state: {}",
            status.state
        );
        // prompt_preview is populated synchronously before the background task.
        assert_eq!(status.prompt_preview, "investigate bug");
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
    async fn cancel_worker_unknown_id_errors() {
        let state = test_state().await;
        let err = handle_cancel_worker(&state, "never-existed")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown task_id"));
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

    /// Build a test_state whose FakeSpawner produces a completed session
    /// (with a result event carrying a known token_usage), so the
    /// backgrounded worker actually transitions through the full spawn path.
    async fn completing_test_state() -> Arc<DispatchState> {
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use mosaic_core::process::fake::{FakeScript, FakeSpawner};
        use mosaic_core::process::ProcessSpawner;
        use mosaic_core::session::CancelToken;
        use mosaic_core::store::{JsonFileStore, SessionStore};
        use mosaic_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let lead = ResolvedLead {
            id: "lead".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "lead prompt".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: None,
            lead_timeout_secs: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        // Emit a single result event with known token usage, then exit 0.
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(
                r#"{"type":"result","session_id":"sess_ok","usage":{"input_tokens":1000,"output_tokens":2000}}"#,
            )
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        std::mem::forget(dir);
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
        ))
    }

    #[tokio::test]
    async fn spawn_worker_completes_and_updates_spent_usd_and_parent_task_id() {
        use mosaic_core::store::TaskStatus;
        use std::time::Duration;

        let state = completing_test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "analyze bug #42".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None, // falls back to lead model (claude-haiku-4-5)
        };

        // Subscribe to done events BEFORE spawning.
        let mut rx = state.done_tx.subscribe();
        let spawn = handle_spawn_worker(&state, args).await.unwrap();

        // Wait for the broadcast.
        let id = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("broadcast arrives in time")
            .expect("broadcast channel open");
        assert_eq!(id, spawn.task_id, "broadcast id matches spawn id");

        // Verify Done state + Success + parent_task_id.
        let workers = state.workers.read().await;
        let entry = workers.get(&spawn.task_id).expect("worker recorded");
        match entry {
            WorkerState::Done(rec) => {
                assert!(
                    matches!(rec.status, TaskStatus::Success),
                    "status is Success"
                );
                assert_eq!(rec.parent_task_id.as_deref(), Some("lead"));
                assert_eq!(rec.token_usage.input, 1000);
                assert_eq!(rec.token_usage.output, 2000);
                assert_eq!(rec.claude_session_id.as_deref(), Some("sess_ok"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
        drop(workers);

        // Verify cost accumulation. claude-haiku-4-5: input $0.80/1M, output $4.00/1M.
        // 1000 input = $0.0008; 2000 output = $0.008; total = $0.0088.
        let spent = *state.spent_usd.lock().await;
        assert!(
            (spent - 0.0088).abs() < 1e-6,
            "expected spent_usd ≈ 0.0088, got {spent}"
        );

        // Verify prompt_preview is present.
        let preview = state
            .worker_prompts
            .read()
            .await
            .get(&spawn.task_id)
            .cloned()
            .unwrap_or_default();
        assert_eq!(preview, "analyze bug #42");
    }
}
