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

use crate::dispatch::state::{DispatchState, WorkerState};

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
}
