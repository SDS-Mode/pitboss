//! Shared state for a single hierarchical run. Held in an Arc and shared
//! between the dispatch runner (which writes TaskRecords) and the MCP server
//! (which reads worker status, enforces caps, enqueues spawns).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{SessionStore, TaskRecord};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use uuid::Uuid;

use crate::manifest::resolve::ResolvedManifest;

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum WorkerState {
    Pending,
    Running {
        started_at: chrono::DateTime<chrono::Utc>,
        /// Populated once the worker's claude subprocess emits its
        /// `{"type":"system","subtype":"init"}` event. `None` during the brief
        /// window between spawn and first init event (≤ ~1s in practice);
        /// pause/reprompt fail with `op_unknown_state{current_state:"spawning"}`
        /// when None.
        session_id: Option<String>,
    },
    Paused {
        /// Captured from the Running variant at pause time.
        session_id: String,
        paused_at: chrono::DateTime<chrono::Utc>,
        /// Snapshot of token usage at pause time, so continue's final
        /// TaskRecord knows what the prior subprocess cost.
        prior_token_usage: pitboss_core::parser::TokenUsage,
    },
    Done(TaskRecord),
}

/// Response returned to a lead that called `request_approval`.
#[derive(Debug, Clone)]
pub struct ApprovalResponse {
    pub approved: bool,
    pub comment: Option<String>,
    pub edited_summary: Option<String>,
}

/// Policy for approval requests when no TUI is attached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    #[default]
    Block,
    AutoApprove,
    AutoReject,
}

/// An approval request that arrived before a TUI attached. Block-mode runs
/// queue these; they drain when the next TUI connects.
pub struct QueuedApproval {
    pub request_id: String,
    pub task_id: String,
    pub summary: String,
    pub responder: oneshot::Sender<ApprovalResponse>,
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
    /// USD reserved for in-flight workers at spawn time. Incremented when a
    /// worker is spawned (with its per-model cost estimate), decremented when
    /// the worker completes and its actual cost is added to `spent_usd`.
    /// The budget guard checks `spent + reserved + estimate > budget` so a
    /// burst of spawns can't all pass before any completion updates state.
    pub reserved_usd: Mutex<f64>,
    /// Broadcast channel that emits a `task_id` whenever a worker transitions
    /// to `Done`. Subscribed to by `wait_for_worker` handlers.
    pub done_tx: broadcast::Sender<String>,
    /// Per-worker CancelToken, keyed by task_id. Registered on spawn,
    /// terminated by `cancel_worker`.
    pub worker_cancels: RwLock<HashMap<String, CancelToken>>,
    /// Per-worker prompt preview (first 80 chars of the worker's prompt).
    /// Populated at spawn time; surfaced by `list_workers` / `worker_status`.
    pub worker_prompts: RwLock<HashMap<String, String>>,
    /// Per-worker resolved model, keyed by task_id. Populated at spawn time so
    /// `estimate_new_worker_cost` and cost accumulation know the right rate.
    pub worker_models: RwLock<HashMap<String, String>>,
    /// Per-worker reserved cost (USD) at spawn time. On completion, the
    /// reservation is removed from `reserved_usd` and the worker's *actual*
    /// cost is added to `spent_usd`.
    pub worker_reservations: RwLock<HashMap<String, f64>>,
    /// Dependencies needed to actually launch worker subprocesses. These are
    /// threaded from `run_hierarchical` so the MCP tool handlers can call
    /// into the same SessionHandle/WorktreeManager pipeline used by the flat
    /// dispatcher.
    pub spawner: Arc<dyn ProcessSpawner>,
    pub claude_binary: PathBuf,
    pub wt_mgr: Arc<WorktreeManager>,
    pub cleanup_policy: CleanupPolicy,
    /// The per-run subdirectory where worker logs/artifacts land (`run_dir/<run_id>/`).
    pub run_subdir: PathBuf,
    /// Approval bridge: maps request_id → sender that completes when the
    /// TUI responds to an approval request. Seeded by `ApprovalBridge::request`,
    /// drained by the `approve` control op.
    pub approval_bridge: Mutex<HashMap<String, oneshot::Sender<ApprovalResponse>>>,
    /// Queued approval requests waiting for a TUI to attach.
    pub approval_queue: Mutex<VecDeque<QueuedApproval>>,
    /// Approval policy from the manifest.
    pub approval_policy: ApprovalPolicy,
    /// Outbound control-socket event channel. Set when a TUI is connected; the
    /// control server clears it on disconnect.
    pub control_writer:
        Mutex<Option<mpsc::UnboundedSender<crate::control::protocol::ControlEvent>>>,
}

impl DispatchState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: Uuid,
        manifest: ResolvedManifest,
        store: Arc<dyn SessionStore>,
        cancel: CancelToken,
        lead_id: String,
        spawner: Arc<dyn ProcessSpawner>,
        claude_binary: PathBuf,
        wt_mgr: Arc<WorktreeManager>,
        cleanup_policy: CleanupPolicy,
        run_subdir: PathBuf,
        approval_policy: ApprovalPolicy,
    ) -> Self {
        let (done_tx, _) = broadcast::channel(64);
        Self {
            run_id,
            manifest,
            store,
            cancel,
            lead_id,
            workers: RwLock::new(HashMap::new()),
            spent_usd: Mutex::new(0.0),
            reserved_usd: Mutex::new(0.0),
            done_tx,
            worker_cancels: RwLock::new(HashMap::new()),
            worker_prompts: RwLock::new(HashMap::new()),
            worker_models: RwLock::new(HashMap::new()),
            worker_reservations: RwLock::new(HashMap::new()),
            spawner,
            claude_binary,
            wt_mgr,
            cleanup_policy,
            run_subdir,
            approval_bridge: Mutex::new(HashMap::new()),
            approval_queue: Mutex::new(VecDeque::new()),
            approval_policy,
            control_writer: Mutex::new(None),
        }
    }

    pub async fn active_worker_count(&self) -> usize {
        self.workers
            .read()
            .await
            .values()
            .filter(|w| {
                matches!(
                    w,
                    WorkerState::Pending | WorkerState::Running { .. } | WorkerState::Paused { .. }
                )
            })
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
    use pitboss_core::process::TokioSpawner;
    use pitboss_core::store::JsonFileStore;
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
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            cancel,
            "lead-1".into(),
            spawner,
            PathBuf::from("/bin/false"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
        ));
        // Keep the TempDir alive for the test by leaking it — the state holds
        // PathBufs into it, and dropping `dir` at end of scope would invalidate
        // on-disk paths for any test that reads them.
        std::mem::forget(dir);
        state
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

    #[test]
    fn running_worker_state_captures_session_id() {
        let started_at = chrono::Utc::now();
        let sid: Option<String> = Some("sess-abc".into());
        let w = WorkerState::Running {
            started_at,
            session_id: sid.clone(),
        };
        match w {
            WorkerState::Running {
                session_id,
                started_at: _,
            } => {
                assert_eq!(session_id, Some("sess-abc".to_string()));
            }
            other => panic!("expected Running, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn state_initializes_new_v04_fields() {
        let st = mk_state(None, None);
        assert!(st.approval_bridge.lock().await.is_empty());
        assert!(st.approval_queue.lock().await.is_empty());
        assert!(matches!(
            st.approval_policy,
            crate::dispatch::state::ApprovalPolicy::Block
        ));
        assert!(st.control_writer.lock().await.is_none());
    }
}
