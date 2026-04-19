//! Run-level dispatch state. Wraps a root `LayerState` (always present)
//! plus a map of sub-tree `LayerState`s (empty in depth-1 runs;
//! populated as the root lead spawns sub-leads). The run-global
//! `LeaseRegistry` lives here too — added in Phase 3.
//!
//! Backward-compatible constructor signature: depth-1 callers continue
//! to use `DispatchState::new(...)` with the existing 13 arguments.
//!
//! `DispatchState` implements `Deref<Target = LayerState>` so every
//! existing field access (e.g. `state.workers`, `state.cancel`) continues
//! to work without callers knowing about the indirection.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{SessionStore, TaskRecord};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tokio::sync::{oneshot, RwLock};
use uuid::Uuid;

use crate::dispatch::layer::LayerState;
use crate::manifest::resolve::ResolvedManifest;
use crate::shared_store::RunLeaseRegistry;

// ── Re-exported public types (keep in this module for back-compat) ──────────
//
// Downstream code that does `use pitboss_cli::dispatch::state::WorkerState`
// etc. continues to compile unchanged.

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
    /// Frozen by SIGSTOP — claude subprocess is still alive but
    /// suspended at the kernel level. Distinct from Paused because
    /// `continue_worker` just SIGCONT's instead of respawning via
    /// `claude --resume`. Suitable for short pauses; long freezes risk
    /// Anthropic dropping the HTTP session on their side.
    Frozen {
        /// Session id captured at freeze time (same field semantics as
        /// Paused). Populated so `worker_status` still reports it and
        /// so callers that want to fall back to cancel-style resume can.
        session_id: String,
        frozen_at: chrono::DateTime<chrono::Utc>,
        /// Saved `started_at` from the Running state so `continue_worker`
        /// can transition back to Running without losing elapsed time.
        started_at: chrono::DateTime<chrono::Utc>,
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

#[derive(Default, Clone, Debug)]
pub struct WorkerCounters {
    pub pause_count: u32,
    pub reprompt_count: u32,
    pub approvals_requested: u32,
    pub approvals_approved: u32,
    pub approvals_rejected: u32,
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
    /// Typed approval plan — same option as on the request path. `None`
    /// for simple summary-only approvals, so pre-v0.4.5 queuers still
    /// round-trip through this struct without issue.
    pub plan: Option<crate::mcp::tools::ApprovalPlan>,
    /// Discriminator between in-flight action approvals and pre-flight
    /// plan approvals. Carried through the queue so the TUI renders
    /// the right modal header when the queue drains.
    pub kind: crate::control::protocol::ApprovalKind,
    pub responder: oneshot::Sender<ApprovalResponse>,
}

// ── DispatchState ────────────────────────────────────────────────────────────

/// Run-level wrapper. Holds the root `LayerState` plus (in Phase 2+) a map
/// of sub-tree `LayerState`s keyed by sub-lead id.
///
/// Implements `Deref<Target = LayerState>` so all existing callsites that
/// access fields like `state.workers`, `state.cancel`, etc. compile unchanged.
pub struct DispatchState {
    pub root: Arc<LayerState>,
    /// Sub-tree layers keyed by sub-lead id. Empty in the depth-1 case.
    /// Populated by `spawn_sublead` in Phase 2.
    pub subleads: RwLock<HashMap<String, Arc<LayerState>>>,
    /// Worker-id → layer-id index for O(1) KV routing.
    ///
    /// - Root-layer workers map to `None`.
    /// - Sub-tree workers map to `Some(sublead_id)`.
    ///
    /// Populated by `spawn_worker` at registration time; cleaned up when a
    /// worker is reaped. Consulted by `resolve_layer_for_caller` in the KV
    /// tool handlers to route each operation to the correct `LayerState`.
    pub worker_layer_index: RwLock<HashMap<String, Option<String>>>,
    /// Run-global lease registry for cross-sub-tree resource coordination.
    /// Distinct from per-layer /leases/* stored in each layer's KvStore.
    pub run_leases: Arc<RunLeaseRegistry>,
}

/// CAUTION: This Deref always resolves to the root layer, which is correct
/// for depth-1 code. Phase 2+ code dispatching on `_meta.actor_role` MUST
/// explicitly look up the correct layer: root-lead callers use `&state.root`,
/// while sub-lead callers must call `state.subleads.read().await.get(sublead_id)`.
/// See Phase 3.1's `resolve_layer_for_caller` helper for canonical resolution.
impl std::ops::Deref for DispatchState {
    type Target = LayerState;

    fn deref(&self) -> &Self::Target {
        &self.root
    }
}

impl std::fmt::Debug for DispatchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchState")
            .field("root", &self.root)
            .field("subleads", &self.subleads.try_read().map(|g| g.len()).ok())
            .field(
                "worker_layer_index",
                &self.worker_layer_index.try_read().map(|g| g.len()).ok(),
            )
            .field("run_leases", &self.run_leases)
            .finish()
    }
}

impl DispatchState {
    /// Create a new run-level state. Argument order and types are identical
    /// to v0.5 so every existing callsite compiles unchanged.
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
        notification_router: Option<std::sync::Arc<crate::notify::NotificationRouter>>,
        shared_store: std::sync::Arc<crate::shared_store::SharedStore>,
    ) -> Self {
        let root = Arc::new(LayerState::new(
            run_id,
            manifest,
            store,
            cancel,
            lead_id,
            spawner,
            claude_binary,
            wt_mgr,
            cleanup_policy,
            run_subdir,
            approval_policy,
            notification_router,
            shared_store,
            None,
        ));
        Self {
            root,
            subleads: RwLock::new(HashMap::new()),
            worker_layer_index: RwLock::new(HashMap::new()),
            run_leases: Arc::new(RunLeaseRegistry::new()),
        }
    }

    /// Accessor for the root layer. Used where callers need an explicit
    /// `Arc<LayerState>` rather than transparent field access.
    pub fn root_layer(&self) -> &Arc<LayerState> {
        &self.root
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
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let cancel = CancelToken::new();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(pitboss_core::worktree::WorktreeManager::new());
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
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
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

    #[tokio::test]
    async fn worker_counters_default_zero() {
        let st = mk_state(None, None);
        let c = st
            .worker_counters
            .read()
            .await
            .get("absent")
            .cloned()
            .unwrap_or_default();
        assert_eq!(c.pause_count, 0);
    }
}
