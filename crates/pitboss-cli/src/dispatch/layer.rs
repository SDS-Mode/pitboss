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

/// Capacity of the per-run event broadcast bus (PR-H of #259).
///
/// Sized 2× `CONTROL_EVENT_QUEUE_CAP` so the headless persistence
/// subscriber has enough slack to absorb a burst even while one
/// connected pump is briefly stalled on socket I/O. Lag past this
/// bound surfaces as `broadcast::error::RecvError::Lagged(n)` on the
/// affected subscriber and is logged but not fatal — the run's
/// canonical state still lands in `summary.jsonl`.
pub const EVENTS_BROADCAST_CAP: usize = 1024;

/// One installed outbound control-event sender.
///
/// `id` is a UUID minted when the connection is accepted. The disconnect
/// cleanup path compares `id` against whatever currently occupies the
/// slot before clearing — a racing reconnect that swapped in its own
/// writer before the outgoing connection's cleanup runs will have a
/// different id, and the clear is skipped.
pub struct ControlWriterSlot {
    pub id: Uuid,
    /// PR-G of #259: channel carries fully-wrapped envelopes (with
    /// `seq` already assigned via the per-run `EventLog`) so the
    /// pump just serializes; persistence happens at the broadcast
    /// site, not at the pump, so headless dispatches still produce
    /// `events.jsonl`.
    pub sender: mpsc::Sender<crate::control::protocol::EventEnvelope>,
}

/// Per-task_id maps + the worker-done broadcast bus.
///
/// Split out of `LayerState` (#487) to narrow the access surface: every
/// piece of state keyed by `task_id` lives here, plus the broadcast
/// channel that fires when a worker transitions to `Done`. Handlers that
/// only need worker bookkeeping can take `&WorkerRegistry` instead of
/// `&LayerState` and physically can't touch budget or approval state.
pub struct WorkerRegistry {
    /// Map of task_id → worker state. Lead is also tracked here for convenience.
    pub states: RwLock<HashMap<String, WorkerState>>,
    /// Per-worker CancelToken, keyed by task_id.
    pub cancels: RwLock<HashMap<String, CancelToken>>,
    /// Per-worker prompt preview (first 80 chars of the worker's prompt).
    pub prompts: RwLock<HashMap<String, String>>,
    /// Per-worker resolved model, keyed by task_id.
    pub models: RwLock<HashMap<String, String>>,
    /// Per-worker resolved `actor_type` (matches a `[[worker_type]].id` from
    /// the manifest), keyed by task_id. Populated at spawn time when the
    /// caller resolves to `WorkerProfileResolution::Typed`. Read on resume
    /// (continue/reprompt) so the rebuilt `mcp-config.json` can scope MCP
    /// servers and the appended `TaskRecord` keeps its profile attribution.
    /// (#252 Phase 1.5)
    pub actor_types: RwLock<HashMap<String, String>>,
    /// Per-task event counters.
    pub counters: RwLock<HashMap<String, WorkerCounters>>,
    /// Per-worker OS pid.
    pub pids: RwLock<HashMap<String, std::sync::Arc<std::sync::atomic::AtomicU32>>>,
    /// Per-worker reserved cost (USD) at spawn time.
    pub reservations: RwLock<HashMap<String, f64>>,
    /// Broadcast channel that emits a `task_id` whenever a worker transitions
    /// to `Done`. Subscribed to by `wait_for_worker` handlers.
    pub done_tx: broadcast::Sender<String>,
}

impl WorkerRegistry {
    /// Construct an empty registry with a fresh 64-slot `done_tx` channel.
    /// The capacity matches the pre-split `LayerState::new` default; the
    /// channel is mostly there for `wait_for_worker` consumers and a
    /// `Lagged` receiver re-syncs from the `states` map.
    pub fn new() -> Self {
        let (done_tx, _) = broadcast::channel(64);
        Self {
            states: RwLock::new(HashMap::new()),
            cancels: RwLock::new(HashMap::new()),
            prompts: RwLock::new(HashMap::new()),
            models: RwLock::new(HashMap::new()),
            actor_types: RwLock::new(HashMap::new()),
            counters: RwLock::new(HashMap::new()),
            pids: RwLock::new(HashMap::new()),
            reservations: RwLock::new(HashMap::new()),
            done_tx,
        }
    }
}

impl Default for WorkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Layer-aggregate USD spend counters + the budget-abort reason.
///
/// Split out of `LayerState` (#487 step 2) so handlers that only need
/// budget bookkeeping can take `&BudgetState` instead of `&LayerState`
/// and physically can't touch worker or approval state.
///
/// Per-worker USD reservations are NOT here — they're keyed by
/// `task_id` and live on `WorkerRegistry.reservations` alongside the
/// rest of the per-task_id maps. This struct only owns the
/// layer-aggregate counters and the abort signal.
pub struct BudgetState {
    /// Total USD spent so far on completed workers in this layer.
    /// Lead and sub-lead token spend are accounted in `lead_spent_usd`;
    /// helpers that compute the total run spend sum across layers.
    pub spent_usd: Mutex<f64>,
    /// USD reserved for in-flight workers at spawn time.
    pub reserved_usd: Mutex<f64>,
    /// Token spend (USD) attributed to *this layer's lead* (the root
    /// lead for the root layer; the sub-lead for a sub-tree layer).
    /// Updated live as each assistant turn's cumulative usage is
    /// observed from the stream-json output, so budget enforcement can
    /// abort mid-subprocess rather than waiting for terminal exit.
    /// Uses `std::sync::Mutex` rather than `tokio::sync::Mutex` because
    /// it is written from the session stream loop's synchronous event
    /// observer — no `.await` may cross the lock guard. (#253)
    pub lead_spent_usd: std::sync::Mutex<f64>,
    /// Set to `Some(reason)` when the dispatcher decides to abort the
    /// lead/sub-lead because of a budget cap breach. Cleared on dispatch
    /// startup and read by `failure_detection` so the lead's
    /// `TaskRecord.failure_reason` names the overspend rather than
    /// looking like a generic cancellation. (#253)
    pub abort_reason: std::sync::Mutex<Option<String>>,
}

impl BudgetState {
    /// Construct a zeroed budget state. All counters start at 0.0
    /// and `abort_reason` is `None`.
    pub fn new() -> Self {
        Self {
            spent_usd: Mutex::new(0.0),
            reserved_usd: Mutex::new(0.0),
            lead_spent_usd: std::sync::Mutex::new(0.0),
            abort_reason: std::sync::Mutex::new(None),
        }
    }
}

impl Default for BudgetState {
    fn default() -> Self {
        Self::new()
    }
}

/// Approval bookkeeping: live-TUI bridge, queued requests, the legacy
/// fall-through policy, the declarative `PolicyMatcher`, and the
/// plan-approval gate.
///
/// Split out of `LayerState` (#487 step 3) so handlers that only need
/// approval state can take `&ApprovalState` instead of `&LayerState`
/// and physically can't touch worker or budget bookkeeping.
///
/// The `plan_approved` gate stays as `AtomicBool` so reads on the
/// hot-path (`spawn_worker`) are lock-free; the bridge and queue
/// retain `Mutex` because the operations that mutate them
/// (insert/remove/take) span more than a single atomic op.
pub struct ApprovalState {
    /// Live-TUI approval requests keyed by request_id.
    ///
    /// Values are `BridgeEntry` (not bare senders) so the TTL watcher can
    /// expire bridge entries that the operator never acted on. Without TTL
    /// metadata here, an approval that moves from `queue` to the bridge
    /// on TUI connect loses its auto-resolve guarantee.
    pub bridge: Mutex<HashMap<String, BridgeEntry>>,
    /// Queued approval requests waiting for a TUI to attach.
    pub queue: Mutex<VecDeque<QueuedApproval>>,
    /// Legacy fall-through approval policy from `[run].default_approval_policy`.
    /// Applies when no `[[approval_policy]]` rule matches.
    pub policy: ApprovalPolicy,
    /// Plan-approval gate. Atomic so the hot-path `spawn_worker` check
    /// is lock-free.
    pub plan_approved: std::sync::atomic::AtomicBool,
    /// Operator-declared approval policy matcher. Loaded from manifest
    /// `[[approval_policy]]` blocks at run startup. `None` means no
    /// declarative rules; every approval falls through to the legacy
    /// `policy` / operator queue path.
    ///
    /// NOTE: This is a run-level (root-layer) policy for v0.6. Per-sub-lead
    /// policy is deferred to Phase 4.x.
    pub matcher: Mutex<Option<PolicyMatcher>>,
}

impl ApprovalState {
    /// Construct an approval state with the legacy fall-through policy
    /// taken from the manifest. Bridge/queue start empty, plan gate
    /// closed, no declarative matcher installed yet.
    pub fn new(policy: ApprovalPolicy) -> Self {
        Self {
            bridge: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            policy,
            plan_approved: std::sync::atomic::AtomicBool::new(false),
            matcher: Mutex::new(None),
        }
    }
}

/// All state owned by a single coordination layer (root layer or a
/// sub-tree layer).
pub struct LayerState {
    pub run_id: Uuid,
    pub manifest: ResolvedManifest,
    pub store: Arc<dyn SessionStore>,
    pub cancel: CancelToken,
    pub lead_id: String,
    /// Worker bookkeeping: per-task_id maps + the `Done` broadcast.
    /// Split out of this struct in #487 step 1.
    pub workers: WorkerRegistry,
    /// Layer-aggregate USD counters + budget-abort signal.
    /// Split out of this struct in #487 step 2.
    pub budget: BudgetState,
    /// Dependencies needed to actually launch worker subprocesses.
    pub spawner: Arc<dyn ProcessSpawner>,
    pub claude_binary: PathBuf,
    pub wt_mgr: Arc<WorktreeManager>,
    pub cleanup_policy: CleanupPolicy,
    /// The per-run subdirectory where worker logs/artifacts land.
    pub run_subdir: PathBuf,
    /// Approval bookkeeping: live-TUI bridge, queue, legacy fall-through
    /// policy, declarative matcher, and the plan-approval gate.
    /// Split out of this struct in #487 step 3.
    pub approvals: ApprovalState,
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
    /// Per-run event-stream log (#259, co-spec'd with #438). Cloned
    /// from `DispatchState.event_log` at LayerState construction so
    /// the per-layer `broadcast_control_event` can assign the
    /// run-scoped seq without going through the pump (which only runs
    /// when a client is connected). Shared across the root layer and
    /// every sub-tree layer.
    pub event_log: Arc<crate::control::event_log::EventLog>,
    /// Per-run event broadcast bus (#259 PR-H). Every envelope
    /// produced by [`Self::broadcast_control_event`] (and direct
    /// emitters in `control::server` / `mcp::approval`) is sent here
    /// **regardless of whether a client is connected**. Subscribers:
    ///
    /// 1. A persistent task spawned in `DispatchState::new` that
    ///    drains the broadcast and persists each envelope to
    ///    `events.jsonl` — this is what makes headless dispatches
    ///    produce a complete log.
    /// 2. Per-connection pump tasks in `control::server` that forward
    ///    to the connected socket.
    ///
    /// 1024 capacity covers multi-second bursts (sub-lead spawn fan-
    /// out, approvals) without unbounded memory. A lagged subscriber
    /// sees `broadcast::error::RecvError::Lagged(n)` and logs a warn;
    /// the headless persistence task is unlikely to lag because
    /// `event_log.persist` is a small append.
    ///
    /// Shared by clone across the root layer and every sub-tree
    /// layer so the entire run funnels into a single bus.
    pub events_tx: broadcast::Sender<crate::control::protocol::EventEnvelope>,
    /// v0.4.1: notification router.
    pub notification_router: Option<std::sync::Arc<crate::notify::NotificationRouter>>,
    /// In-memory shared store for hub-mediated lead ↔ worker coordination.
    pub shared_store: std::sync::Arc<crate::shared_store::SharedStore>,
    /// Original reservation amount (USD) at sub-lead spawn time.
    /// Only set for sub-leads; None for root layer.
    pub original_reservation_usd: Option<f64>,
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
            .field(
                "workers",
                &self.workers.states.try_read().map(|g| g.len()).ok(),
            )
            .field(
                "workers.cancels",
                &self.workers.cancels.try_read().map(|g| g.len()).ok(),
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
        // PR-G of #259: default to a disabled, root-`/` event log so the
        // 14-arg signature stays back-compat with the pre-PR-G call
        // shape (tests + ad-hoc constructors). Prod callers
        // (`DispatchState::new`, sub-lead spawn) override via
        // [`Self::with_event_log`] immediately after constructing.
        let event_log = Arc::new(crate::control::event_log::EventLog::new(
            std::path::Path::new("/"),
            false,
        ));
        // PR-H of #259: each `LayerState::new` mints its own broadcast
        // bus by default. Prod callers (`DispatchState::new`, sub-lead
        // spawn) override via [`Self::with_events_tx`] to share the
        // root layer's bus so the entire run funnels into one stream.
        let (events_tx, _) = broadcast::channel(EVENTS_BROADCAST_CAP);
        Self {
            run_id,
            manifest,
            store,
            cancel,
            lead_id,
            workers: WorkerRegistry::new(),
            budget: BudgetState::new(),
            spawner,
            claude_binary,
            wt_mgr,
            cleanup_policy,
            run_subdir,
            approvals: ApprovalState::new(approval_policy),
            control_writer: Mutex::new(None),
            event_log,
            events_tx,
            notification_router,
            shared_store,
            original_reservation_usd,
            reprompt_hook: Mutex::new(None),
            reprompt_tx: Mutex::new(None),
        }
    }

    /// Builder: install the run-scoped event log on this freshly-
    /// constructed layer. PR-G of #259 — call sites that have an
    /// `EventLog` available (`DispatchState::new` for the root,
    /// `dispatch::sublead::spawn_sublead` for sub-trees) chain this
    /// after [`Self::new`]. Test callers that don't care about
    /// persistence can omit the builder and inherit the disabled
    /// default.
    #[must_use]
    pub fn with_event_log(mut self, event_log: Arc<crate::control::event_log::EventLog>) -> Self {
        self.event_log = event_log;
        self
    }

    /// Builder: install the run-scoped event broadcast bus on this
    /// freshly-constructed layer. PR-H of #259 — sub-leads call this
    /// with the root layer's `events_tx` so the whole run funnels
    /// into one bus, which the headless persistence subscriber
    /// drains. Test callers that don't care about the broadcast can
    /// omit and inherit the per-layer default.
    #[must_use]
    pub fn with_events_tx(
        mut self,
        events_tx: broadcast::Sender<crate::control::protocol::EventEnvelope>,
    ) -> Self {
        self.events_tx = events_tx;
        self
    }

    /// Install a `PolicyMatcher` on this layer. Called at run startup after
    /// resolving `[[approval_policy]]` blocks from the manifest. Can also be
    /// called in tests to inject policy without manifests.
    pub async fn set_policy_matcher(&self, matcher: PolicyMatcher) {
        *self.approvals.matcher.lock().await = Some(matcher);
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

    /// Broadcast a control-plane event. PR-H of #259: this call
    /// assigns the run-scoped seq from [`Self::event_log`] and
    /// publishes the envelope onto [`Self::events_tx`]. Subscribers
    /// fan out from there:
    ///
    /// - The persistent task spawned in `DispatchState::new` reads
    ///   off the bus and appends to `events.jsonl` (gated by
    ///   `emit_event_stream`). This is what makes headless
    ///   dispatches produce a complete log.
    /// - The connected control-socket pump (if any) reads off the
    ///   bus and writes to the connected client.
    ///
    /// The caller's `envelope.seq` is overwritten; call sites pass
    /// `seq: 0` placeholders.
    ///
    /// `events_tx.send` returns `Err` only when there are zero
    /// subscribers — possible only during the narrow window before
    /// the persistence subscriber has been spawned (in tests that
    /// build a bare `LayerState` without a `DispatchState`). The
    /// error is logged and swallowed; the envelope is dropped.
    pub async fn broadcast_control_event(
        &self,
        mut envelope: crate::control::protocol::EventEnvelope,
    ) {
        envelope.seq = self.event_log.next_seq();
        if let Err(e) = self.events_tx.send(envelope) {
            tracing::debug!(
                "events_tx send dropped envelope: {} (no active subscribers)",
                e,
            );
        }
    }

    pub async fn active_worker_count(&self) -> usize {
        self.workers
            .states
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
        let spent = *self.budget.spent_usd.lock().await;
        Some((budget - spent).max(0.0))
    }

    /// Register a worker's `CancelToken` under `task_id` and immediately
    /// propagate any in-flight drain/terminate state from this layer's
    /// `cancel` to the worker's token. Centralizes the previously-inlined
    /// pattern at `mcp/tools.rs` (insert into `workers.cancels`, then
    /// check `target_layer.cancel.is_terminated()` / `is_draining()`).
    ///
    /// The eager propagation is what closes the post-register cascade
    /// gap (#99): the per-sublead watcher (`install_sublead_cancel_watcher`)
    /// is fire-once, so a worker registered after the watcher has fired
    /// would otherwise miss the cancel signal. Pinned by the integration
    /// tests in `tests/cancel_cascade_flows.rs`.
    pub async fn register_worker_cancel(&self, task_id: String, token: CancelToken) {
        self.workers
            .cancels
            .write()
            .await
            .insert(task_id, token.clone());
        self.cancel.cascade_to(&token);
    }

    /// Walk every registered `workers.cancels` entry and cascade this
    /// layer's current cancel state to it via `CancelToken::cascade_to`.
    /// Used by the per-sublead watcher tasks installed by
    /// `install_sublead_cancel_watcher` — both the drain and terminate
    /// watchers funnel through this method so the cascade rule
    /// (terminate dominates drain) is encoded in exactly one place.
    pub async fn cascade_to_workers(&self) {
        let workers = self.workers.cancels.read().await;
        for (worker_id, tok) in workers.iter() {
            tracing::debug!(worker_id = %worker_id, "cascading cancel state to sub-tree worker");
            self.cancel.cascade_to(tok);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::state_builder::TestStateBuilder;
    use tempfile::TempDir;

    fn mk_layer() -> (TempDir, LayerState) {
        TestStateBuilder::new().fake_spawner_no_hold().build_layer()
    }

    #[tokio::test]
    async fn new_layer_starts_empty() {
        let (_dir, layer) = mk_layer();
        assert!(layer.workers.states.read().await.is_empty());
        assert!(layer.workers.cancels.read().await.is_empty());
        assert_eq!(*layer.budget.spent_usd.lock().await, 0.0);
        assert_eq!(*layer.budget.reserved_usd.lock().await, 0.0);
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
