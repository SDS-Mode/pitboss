//! Per-layer dispatch state. A run has one `LayerState` for the root
//! layer (root lead + workers + sub-leads-as-peers) and one
//! `LayerState` per sub-lead (sub-lead + its workers). Structurally
//! identical at every layer; only the actor population differs.
//!
//! In the depth-1 (no sub-leads) case, only the root layer exists and
//! `LayerState` behaves exactly like the v0.5 `DispatchState`. The
//! split is a refactor, not a behavior change.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::SessionStore;
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::dispatch::state::{
    ApprovalPolicy, BridgeEntry, QueuedApproval, WorkerCounters, WorkerState,
};
use crate::manifest::resolve::ResolvedManifest;
use crate::mcp::policy::PolicyMatcher;

/// Callback type for test-only synthetic reprompt capture.
/// See `LayerState::reprompt_hook` and `install_reprompt_capture`.
type RepromptHook = Arc<dyn Fn(String) + Send + Sync + 'static>;

/// Upper bound on queued outbound control events per TUI connection.
/// Producers use `try_send` and drop on a full queue rather than block,
/// so a frozen TUI socket does not wedge the dispatcher.
///
/// 512 events covers multi-second bursts (approvals, worker lifecycle
/// fan-out) without meaningful memory cost. A frozen TUI simply misses
/// events past the cap — an acceptable failure mode since reconnect
/// re-syncs via the Hello snapshot.
pub const CONTROL_EVENT_QUEUE_CAP: usize = 512;

/// One installed outbound control-event sender.
///
/// `id` is a UUID minted when the connection is accepted. The disconnect
/// cleanup path compares `id` against whatever currently occupies the
/// slot before clearing — a racing reconnect that swapped in its own
/// writer before the outgoing connection's cleanup runs will have a
/// different id, and the clear is skipped.
pub struct ControlWriterSlot {
    pub id: Uuid,
    pub sender: mpsc::Sender<crate::control::protocol::ControlEvent>,
}

/// All state owned by a single coordination layer (root layer or a
/// sub-tree layer).
///
/// Field names and types mirror the v0.5 `DispatchState` fields exactly so
/// that all existing callsites continue to work via `Deref<Target = LayerState>`
/// on `DispatchState`.
pub struct LayerState {
    pub run_id: Uuid,
    pub manifest: ResolvedManifest,
    pub store: Arc<dyn SessionStore>,
    pub cancel: CancelToken,
    pub lead_id: String,
    /// Map of task_id → worker state. Lead is also tracked here for convenience.
    pub workers: RwLock<HashMap<String, WorkerState>>,
    /// Total USD cost spent so far (updated after each worker completes).
    pub spent_usd: Mutex<f64>,
    /// USD reserved for in-flight workers at spawn time.
    pub reserved_usd: Mutex<f64>,
    /// Broadcast channel that emits a `task_id` whenever a worker transitions
    /// to `Done`. Subscribed to by `wait_for_worker` handlers.
    pub done_tx: broadcast::Sender<String>,
    /// Per-worker CancelToken, keyed by task_id.
    pub worker_cancels: RwLock<HashMap<String, CancelToken>>,
    /// Per-worker prompt preview (first 80 chars of the worker's prompt).
    pub worker_prompts: RwLock<HashMap<String, String>>,
    /// Per-worker resolved model, keyed by task_id.
    pub worker_models: RwLock<HashMap<String, String>>,
    /// Per-worker reserved cost (USD) at spawn time.
    pub worker_reservations: RwLock<HashMap<String, f64>>,
    /// Dependencies needed to actually launch worker subprocesses.
    pub spawner: Arc<dyn ProcessSpawner>,
    pub claude_binary: PathBuf,
    pub wt_mgr: Arc<WorktreeManager>,
    pub cleanup_policy: CleanupPolicy,
    /// The per-run subdirectory where worker logs/artifacts land.
    pub run_subdir: PathBuf,
    /// Approval bridge: maps request_id → sender that completes when the
    /// TUI responds to an approval request.
    /// Live-TUI approval requests keyed by request_id.
    ///
    /// Values are `BridgeEntry` (not bare senders) so the TTL watcher can
    /// expire bridge entries that the operator never acted on. Without TTL
    /// metadata here, an approval that moves from `approval_queue` to the
    /// bridge on TUI connect loses its auto-resolve guarantee.
    pub approval_bridge: Mutex<HashMap<String, BridgeEntry>>,
    /// Queued approval requests waiting for a TUI to attach.
    pub approval_queue: Mutex<VecDeque<QueuedApproval>>,
    /// Approval policy from the manifest.
    pub approval_policy: ApprovalPolicy,
    /// Outbound control-socket event channel.
    ///
    /// `ControlWriterSlot` carries a per-connection `id` so disconnect
    /// cleanup can verify it's clearing ITS OWN writer rather than one
    /// a later connection installed mid-cleanup. Without the id check,
    /// a reconnecting TUI races the outgoing connection's cleanup and
    /// has its writer silently cleared.
    ///
    /// The sender is bounded (see `CONTROL_EVENT_QUEUE_CAP`) so a slow or
    /// frozen TUI cannot cause unbounded memory growth on the producer
    /// side — broadcasts use `try_send` and drop on a full queue rather
    /// than block the dispatcher.
    pub control_writer: Mutex<Option<ControlWriterSlot>>,
    /// Per-task event counters.
    pub worker_counters: RwLock<HashMap<String, WorkerCounters>>,
    /// v0.4.1: notification router.
    pub notification_router: Option<std::sync::Arc<crate::notify::NotificationRouter>>,
    /// In-memory shared store for hub-mediated lead ↔ worker coordination.
    pub shared_store: std::sync::Arc<crate::shared_store::SharedStore>,
    /// Per-worker OS pid.
    pub worker_pids: RwLock<HashMap<String, std::sync::Arc<std::sync::atomic::AtomicU32>>>,
    /// Plan-approval gate.
    pub plan_approved: std::sync::atomic::AtomicBool,
    /// Original reservation amount (USD) at sub-lead spawn time.
    /// Only set for sub-leads; None for root layer.
    pub original_reservation_usd: Option<f64>,
    /// Operator-declared approval policy matcher. Loaded from manifest
    /// `[[approval_policy]]` blocks at run startup. `None` means no
    /// declarative rules; every approval falls through to the legacy
    /// ApprovalPolicy / operator queue path.
    ///
    /// NOTE: This is a run-level (root-layer) policy for v0.6. Per-sub-lead
    /// policy is deferred to Phase 4.x.
    pub policy_matcher: Mutex<Option<PolicyMatcher>>,
    /// Test-only hook: intercepts synthetic reprompts that would otherwise be
    /// delivered to this layer's Claude session. `None` in production (reprompt
    /// goes through the real MCP/subprocess path). Set via
    /// `install_reprompt_capture` to capture messages for assertion.
    ///
    /// The hook is `Arc` so it can be cloned cheaply in `send_synthetic_reprompt`
    /// without holding the lock across the async delivery path.
    pub reprompt_hook: Mutex<Option<RepromptHook>>,
    /// Delivery channel for synthetic reprompts to the running lead subprocess.
    ///
    /// For sub-lead layers: set by `spawn_sublead_session` just before launching
    /// the subprocess; the receiving end is held by the kill+resume loop inside
    /// `spawn_sublead_session`.
    ///
    /// For the root layer: set by `run_hierarchical` via `set_reprompt_tx` after
    /// constructing the `DispatchState`, and consumed by the kill+resume loop
    /// inside `run_hierarchical`. Cleared via `clear_reprompt_tx` when the loop
    /// exits.
    ///
    /// `None` until `set_reprompt_tx` is called, or after `clear_reprompt_tx`
    /// is called (lead terminated). Sending on a closed channel is a no-op
    /// (logged at INFO level by `send_synthetic_reprompt`).
    pub reprompt_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
}

impl std::fmt::Debug for LayerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerState")
            .field("run_id", &self.run_id)
            .field("lead_id", &self.lead_id)
            .field("workers", &self.workers.try_read().map(|g| g.len()).ok())
            .field(
                "worker_cancels",
                &self.worker_cancels.try_read().map(|g| g.len()).ok(),
            )
            .finish_non_exhaustive()
    }
}

impl LayerState {
    /// Constructor mirroring the existing `DispatchState::new` 13-argument
    /// signature exactly. The `lead_id` argument names the root lead (or
    /// sub-lead) that owns this layer. The `original_reservation_usd`
    /// parameter is `None` for the root layer and `Some(amount)` for
    /// sub-tree layers (the budget reserved at spawn time).
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
        original_reservation_usd: Option<f64>,
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
            worker_counters: RwLock::new(HashMap::new()),
            notification_router,
            shared_store,
            worker_pids: RwLock::new(HashMap::new()),
            plan_approved: std::sync::atomic::AtomicBool::new(false),
            original_reservation_usd,
            policy_matcher: Mutex::new(None),
            reprompt_hook: Mutex::new(None),
            reprompt_tx: Mutex::new(None),
        }
    }

    /// Install a `PolicyMatcher` on this layer. Called at run startup after
    /// resolving `[[approval_policy]]` blocks from the manifest. Can also be
    /// called in tests to inject policy without manifests.
    pub async fn set_policy_matcher(&self, matcher: PolicyMatcher) {
        *self.policy_matcher.lock().await = Some(matcher);
    }

    /// Populate the reprompt delivery channel for this layer's lead subprocess.
    ///
    /// Called by `run_hierarchical` after the `DispatchState` is constructed but
    /// before the lead subprocess is spawned. The channel is consumed by the
    /// kill+resume loop inside `run_hierarchical` itself.
    ///
    /// A separate setter is used (rather than a constructor parameter) to keep
    /// `LayerState::new` backwards-compatible: all existing callers continue to
    /// work unchanged, and root simply calls `set_reprompt_tx` after construction.
    pub async fn set_reprompt_tx(&self, tx: mpsc::UnboundedSender<String>) {
        *self.reprompt_tx.lock().await = Some(tx);
    }

    /// Clear the reprompt delivery channel. Called after the kill+resume loop
    /// exits so that further sends from `send_synthetic_reprompt` fail fast
    /// with a channel-closed error rather than queueing messages to a dead loop.
    pub async fn clear_reprompt_tx(&self) {
        *self.reprompt_tx.lock().await = None;
    }

    /// Install a test-only reprompt capture hook. When set, synthetic
    /// reprompts delivered via `send_synthetic_reprompt` call this callback
    /// instead of (or before) the real delivery path.
    ///
    /// Use in tests to assert that the correct message was delivered to this
    /// layer's lead without spinning up a real Claude subprocess.
    pub async fn install_reprompt_capture<F>(&self, hook: F)
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        *self.reprompt_hook.lock().await = Some(Arc::new(hook));
    }

    /// Deliver a synthetic reprompt message to this layer's lead.
    ///
    /// ## Test path (hook installed)
    ///
    /// Calls the test-only `reprompt_hook` callback with the message text.
    /// This path is used by unit tests (e.g. Task 4.5) to assert delivery
    /// without spinning up a real subprocess. The hook takes priority over
    /// the real delivery path.
    ///
    /// ## Production path (no hook)
    ///
    /// Sends the message to the `reprompt_tx` channel. The receiving end is
    /// held by the subprocess-management loop — either `spawn_sublead_session`
    /// (for sub-lead layers) or the kill+resume loop in `run_hierarchical`
    /// (for the root layer). The loop handles the message by killing the
    /// current subprocess and re-spawning with `claude --resume <session_id>
    /// -p <message>`.
    ///
    /// The channel send returns immediately; the actual kill+resume is
    /// asynchronous. If the channel send fails (lead already terminated or
    /// channel dropped), the message is logged and delivery is skipped.
    ///
    /// ## No-channel fallback
    ///
    /// If `reprompt_tx` is `None` (layer not yet started, or already
    /// terminated), the message is logged at INFO level and dropped.
    pub async fn send_synthetic_reprompt(&self, message: &str) {
        // Test hook takes priority — allows unit tests to assert on delivery
        // without spinning up a real subprocess.
        let hook = self.reprompt_hook.lock().await.clone();
        if let Some(cb) = hook {
            cb(message.to_string());
            return;
        }

        // Real delivery path: send via the channel managed by spawn_sublead_session.
        let tx = self.reprompt_tx.lock().await.clone();
        match tx {
            Some(sender) => {
                if sender.send(message.to_string()).is_err() {
                    tracing::info!(
                        lead_id = %self.lead_id,
                        "synthetic reprompt: delivery channel closed (lead already terminated); \
                         message dropped: {}",
                        message
                    );
                } else {
                    tracing::debug!(
                        lead_id = %self.lead_id,
                        "synthetic reprompt queued for delivery: {}",
                        message
                    );
                }
            }
            None => {
                // No channel available: layer not yet started, or already terminated.
                tracing::info!(
                    lead_id = %self.lead_id,
                    "synthetic reprompt: no delivery channel (layer not yet started or \
                     already terminated); message dropped: {}",
                    message
                );
            }
        }
    }

    /// Broadcast a control-plane event wrapped in an `EventEnvelope` to any
    /// connected TUI. The `actor_path` carried in the envelope is preserved
    /// in the serialized JSON when non-empty (v0.6+ TUI clients); when empty
    /// it is elided so v0.5 clients parse the event unchanged.
    ///
    /// If no TUI is currently connected the event is silently dropped — the
    /// control socket is best-effort (same semantics as the existing
    /// `control_writer.send(...)` pattern throughout the codebase).
    pub async fn broadcast_control_event(&self, envelope: crate::control::protocol::EventEnvelope) {
        if let Some(w) = self.control_writer.lock().await.as_ref() {
            // The channel carries ControlEvent; the actor_path in the
            // envelope is available for future TUI display but is not
            // threaded through the channel in this task. Emit the inner
            // event so the TUI gets the lifecycle notification.
            //
            // `try_send` drops the event on a full queue (slow/frozen
            // TUI) rather than blocking the broadcaster.
            if let Err(e) = w.sender.try_send(envelope.event) {
                tracing::debug!(
                    "control_writer try_send dropped event: {} ({})",
                    match &e {
                        tokio::sync::mpsc::error::TrySendError::Full(_) => "queue full",
                        tokio::sync::mpsc::error::TrySendError::Closed(_) => "receiver dropped",
                    },
                    std::any::type_name::<crate::control::protocol::ControlEvent>(),
                );
            }
        }
    }

    pub async fn active_worker_count(&self) -> usize {
        self.workers
            .read()
            .await
            .iter()
            .filter(|(id, w)| {
                *id != &self.lead_id
                    && matches!(
                        w,
                        WorkerState::Pending
                            | WorkerState::Running { .. }
                            | WorkerState::Paused { .. }
                            | WorkerState::Frozen { .. }
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
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::JsonFileStore;
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use tempfile::TempDir;

    fn mk_layer() -> (TempDir, LayerState) {
        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            default_approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            lifecycle: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(FakeScript::new()));
        let layer = LayerState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            ApprovalPolicy::Block,
            None,
            Arc::new(crate::shared_store::SharedStore::new()),
            None,
        );
        (dir, layer)
    }

    #[tokio::test]
    async fn new_layer_starts_empty() {
        let (_dir, layer) = mk_layer();
        assert!(layer.workers.read().await.is_empty());
        assert!(layer.worker_cancels.read().await.is_empty());
        assert_eq!(*layer.spent_usd.lock().await, 0.0);
        assert_eq!(*layer.reserved_usd.lock().await, 0.0);
    }

    #[tokio::test]
    async fn layer_lead_identity_persists() {
        let (_dir, layer) = mk_layer();
        assert_eq!(layer.lead_id, "lead");
        assert_eq!(layer.run_id.get_version(), Some(uuid::Version::SortRand));
    }

    /// `send_synthetic_reprompt` with a hook installed should call the hook.
    #[tokio::test]
    async fn send_synthetic_reprompt_calls_hook() {
        let (_dir, layer) = mk_layer();
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let cap = received.clone();
        layer
            .install_reprompt_capture(move |msg| {
                let cap = cap.clone();
                // tokio::spawn not needed here since tests use #[tokio::test]
                let _ = cap.try_lock().map(|mut g| g.push(msg));
            })
            .await;

        layer.send_synthetic_reprompt("hello world").await;

        let msgs = received.lock().await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "hello world");
    }

    /// Without a hook or channel, `send_synthetic_reprompt` should not panic.
    #[tokio::test]
    async fn send_synthetic_reprompt_no_op_without_channel() {
        let (_dir, layer) = mk_layer();
        // Should complete without panic or error.
        layer.send_synthetic_reprompt("no channel installed").await;
    }

    /// With a `reprompt_tx` channel installed, messages should be deliverable.
    #[tokio::test]
    async fn send_synthetic_reprompt_delivers_via_channel() {
        let (_dir, layer) = mk_layer();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *layer.reprompt_tx.lock().await = Some(tx);

        layer.send_synthetic_reprompt("via channel").await;

        let msg = rx.recv().await.expect("channel should have a message");
        assert_eq!(msg, "via channel");
    }

    /// Hook takes priority over channel.
    #[tokio::test]
    async fn reprompt_hook_takes_priority_over_channel() {
        let (_dir, layer) = mk_layer();

        // Install both hook and channel.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *layer.reprompt_tx.lock().await = Some(tx);

        let hook_fired = Arc::new(Mutex::new(false));
        let fired = hook_fired.clone();
        layer
            .install_reprompt_capture(move |_msg| {
                let fired = fired.clone();
                let _ = fired.try_lock().map(|mut g| *g = true);
            })
            .await;

        layer.send_synthetic_reprompt("priority test").await;

        // Hook should have fired, channel should be empty.
        assert!(*hook_fired.lock().await, "hook should fire before channel");
        assert!(
            rx.try_recv().is_err(),
            "channel should be empty when hook fires"
        );
    }
}
