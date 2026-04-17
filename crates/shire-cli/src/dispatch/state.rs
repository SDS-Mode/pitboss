//! Shared state for a single hierarchical run. Held in an Arc and shared
//! between the dispatch runner (which writes TaskRecords) and the MCP server
//! (which reads worker status, enforces caps, enqueues spawns).

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use mosaic_core::session::CancelToken;
use mosaic_core::store::{SessionStore, TaskRecord};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::manifest::resolve::ResolvedManifest;

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum WorkerState {
    Pending,
    Running {
        started_at: chrono::DateTime<chrono::Utc>,
    },
    Done(TaskRecord),
}

pub struct DispatchState {
    pub run_id: Uuid,
    pub manifest: ResolvedManifest,
    pub store: Arc<dyn SessionStore>,
    pub cancel: CancelToken,
    pub lead_id: String,
    /// Map of task_id → worker state. Lead is also tracked here for convenience.
    pub workers: RwLock<HashMap<String, WorkerState>>,
    /// Total USD cost spent so far (updated after each worker completes).
    pub spent_usd: Mutex<f64>,
}

impl DispatchState {
    pub fn new(
        run_id: Uuid,
        manifest: ResolvedManifest,
        store: Arc<dyn SessionStore>,
        cancel: CancelToken,
        lead_id: String,
    ) -> Self {
        Self {
            run_id,
            manifest,
            store,
            cancel,
            lead_id,
            workers: RwLock::new(HashMap::new()),
            spent_usd: Mutex::new(0.0),
        }
    }

    pub async fn active_worker_count(&self) -> usize {
        self.workers
            .read()
            .await
            .values()
            .filter(|w| matches!(w, WorkerState::Pending | WorkerState::Running { .. }))
            .count()
    }

    pub async fn budget_remaining(&self) -> Option<f64> {
        let budget = self.manifest.budget_usd?;
        let spent = *self.spent_usd.lock().await;
        Some((budget - spent).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use mosaic_core::store::JsonFileStore;
    use tempfile::TempDir;

    fn mk_state(budget: Option<f64>, max_workers: Option<u32>) -> Arc<DispatchState> {
        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers,
            budget_usd: budget,
            lead_timeout_secs: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let cancel = CancelToken::new();
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            cancel,
            "lead-1".into(),
        ))
    }

    #[tokio::test]
    async fn active_worker_count_is_zero_on_new_state() {
        let st = mk_state(None, None);
        assert_eq!(st.active_worker_count().await, 0);
    }

    #[tokio::test]
    async fn budget_remaining_reflects_spent() {
        let st = mk_state(Some(10.0), None);
        assert_eq!(st.budget_remaining().await, Some(10.0));
        *st.spent_usd.lock().await = 3.5;
        assert_eq!(st.budget_remaining().await, Some(6.5));
    }

    #[tokio::test]
    async fn budget_remaining_is_none_when_uncapped() {
        let st = mk_state(None, None);
        assert_eq!(st.budget_remaining().await, None);
    }
}
