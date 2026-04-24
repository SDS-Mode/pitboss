//! Run-level dispatch state. Wraps a root `LayerState` (always present)
//! plus a map of sub-tree `LayerState`s (empty in depth-1 runs;
//! populated as the root lead spawns sub-leads). The run-global
//! `LeaseRegistry` lives here too — added in Phase 3.
//!
//! ## Layer access is explicit
//!
//! `DispatchState` does NOT implement `Deref<Target = LayerState>`. Every
//! caller picks a layer explicitly:
//!
//! - Root-layer access: `state.root.<field>` (or `state.root_layer()` for
//!   an `&Arc<LayerState>`).
//! - Sub-tree access: look up the sub-lead in `state.subleads.read().await`.
//! - Routed access (the canonical path for MCP tool handlers that dispatch
//!   on `_meta.actor_role`): use `crate::mcp::server::resolve_layer_for_caller`,
//!   which returns the correct layer for `Lead`/`Sublead`/`Worker` callers.
//!
//! The previous `Deref` impl silently aliased the root layer, which made it
//! easy for new handlers to misroute sub-lead operations to root-layer state
//! without any compile-time signal. See issue #56.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pitboss_core::process::ProcessSpawner;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{SessionStore, TaskRecord};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tokio::sync::{oneshot, RwLock};
use uuid::Uuid;

/// Terminal record stored when a sub-lead finishes (success, cancel,
/// timeout, or error). Allows `wait_actor(sublead_id)` callers to read
/// the outcome after `reconcile_terminated_sublead` has run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubleadTerminalRecord {
    pub sublead_id: String,
    /// "success" | "cancel" | "timeout" | "error"
    pub outcome: String,
    pub spent_usd: f64,
    pub unspent_usd: f64,
    pub terminated_at: chrono::DateTime<chrono::Utc>,
}

/// The return type of `wait_for_actor_internal`.
/// Workers return a `TaskRecord`; sub-leads return a `SubleadTerminalRecord`.
/// The MCP handler serializes whichever variant it gets.
///
/// The variant-size lint is allowed here: `TaskRecord` is ~300 B vs. ~80 B for
/// `SubleadTerminalRecord`, but boxing `TaskRecord` would ripple through every
/// `wait_actor` caller (plus pattern matches across TUI / dispatch / tests) to
/// dereference through a `Box`. This type is constructed once per actor
/// termination — a handful of times per run — so the extra inline bytes are
/// immaterial compared to the churn that boxing would introduce.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "actor_type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum ActorTerminalRecord {
    Worker(TaskRecord),
    Sublead(SubleadTerminalRecord),
}

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovalResponse {
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_summary: Option<String>,
    /// Optional corrective context for rejected approvals. Returned
    /// to the requesting actor's MCP call so its Claude session can
    /// adapt without a separate reprompt round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// True when this response was generated by the TTL watcher
    /// (`expire_approvals`) rather than an operator or policy action.
    /// Used at termination time to distinguish `ApprovalTimedOut` from
    /// `ApprovalRejected`.
    #[serde(default)]
    pub from_ttl: bool,
}

/// Most recent approval response delivered to a given actor. Populated by
/// `handle_request_approval` and `handle_propose_plan` immediately before
/// they return to the MCP caller. Consulted at actor-termination time by
/// `approval_driven_termination` to reclassify "silent exit after a
/// rejected approval" as `TaskStatus::ApprovalRejected` rather than the
/// misleading `Success`.
///
/// Entries are kept for the duration of the run — the map is small (one
/// per actor that ever requested an approval) and termination
/// reclassification can happen seconds to minutes after the last
/// approval.
#[derive(Debug, Clone)]
pub struct LastApprovalResponse {
    pub approved: bool,
    pub from_ttl: bool,
    pub received_at: chrono::DateTime<chrono::Utc>,
}

/// Why an actor's terminal status should be reclassified from `Success`.
/// Returned by `DispatchState::approval_driven_termination`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTerminationKind {
    /// Last approval returned `{approved: false}` from an operator action
    /// or a `[[approval_policy]]` `auto_reject` rule.
    Rejected,
    /// Last approval was rejected because the request's `ttl_secs` elapsed
    /// and the fallback fired (typically `auto_reject`). Reclassified as
    /// `TaskStatus::ApprovalTimedOut` rather than `ApprovalRejected`.
    TimedOut,
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

/// Rich approval record — the canonical representation of a pending operator
/// decision in Phase 4+. Carries actor lineage, downstream wait set, TTL,
/// and fallback policy in addition to the human-readable summary.
///
/// This is distinct from `QueuedApproval` (the lightweight queueing handle
/// used by the block-mode path). In Phase 4 the two will be unified; for now
/// `PendingApproval` is the record-level type while `QueuedApproval` remains
/// the transport-level handle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingApproval {
    pub id: uuid::Uuid,
    /// Actor that raised the approval request (worker id or lead id).
    pub requesting_actor_id: String,
    /// Full tree path from root to the requesting actor.
    pub actor_path: crate::dispatch::actor::ActorPath,
    /// Classifies the action under review.
    pub category: crate::mcp::approval::ApprovalCategory,
    /// One-line human-readable description of what needs approval.
    pub summary: String,
    /// Structured plan payload (rationale, resources, risks, rollback).
    /// `None` for simple summary-only approvals.
    pub plan: Option<crate::mcp::approval::ApprovalPlan>,
    /// Set of actor ids that are blocked waiting for this decision.
    /// At minimum contains `requesting_actor_id`.
    pub blocks: Vec<String>,
    /// Wall-clock time the request was created (for age computation).
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Seconds after `created_at` before the fallback fires.
    /// Default: 1800 (30 min).
    pub ttl_secs: u64,
    /// What to do when `ttl_secs` elapses with no operator response.
    pub fallback: crate::mcp::approval::ApprovalFallback,
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
    /// Seconds after `created_at` before the fallback fires (Task 4.4).
    /// `None` means never expires (preserves v0.5 behavior).
    pub ttl_secs: Option<u64>,
    /// What to do when `ttl_secs` elapses with no operator response (Task 4.4).
    /// `None` means Block (never expires, preserves v0.5 behavior).
    pub fallback: Option<crate::mcp::approval::ApprovalFallback>,
    /// Wall-clock time the request was created (Task 4.4, for age computation).
    /// Used only when `ttl_secs` is Some.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// An approval request that has been handed to a live TUI via the bridge map.
///
/// Carries TTL metadata so `expire_layer_approvals` can expire bridge entries
/// that the operator never acted on — the same guarantee it provides for
/// `approval_queue` entries. Without this, an approval that moves from queue
/// to bridge (when a TUI connects) loses TTL coverage: the queue is empty so
/// the watcher does nothing, while the bridge has no metadata to check against.
///
/// Also retains the display fields (`summary`, `plan`, `kind`) so a TUI that
/// connects after a previous TUI died without responding can have the pending
/// approval replayed from the bridge. Before #102, transfer from queue→bridge
/// dropped these fields, and a reconnecting TUI saw nothing for the still-live
/// responder until a TTL fallback resolved it.
pub struct BridgeEntry {
    /// Oneshot sender; deliver the operator's decision here.
    pub responder: oneshot::Sender<ApprovalResponse>,
    /// Actor that submitted the approval request (for counter attribution).
    pub task_id: String,
    /// Short summary line rendered in the TUI modal + approval-list pane.
    pub summary: String,
    /// Typed structured plan body (rationale, resources, risks, rollback).
    /// `None` for bare summary-only approvals.
    pub plan: Option<crate::mcp::tools::ApprovalPlan>,
    /// Discriminator between `Action` (in-flight) and `Plan` (pre-flight)
    /// approvals — controls which modal header the TUI renders.
    pub kind: crate::control::protocol::ApprovalKind,
    /// Seconds after `created_at` before the fallback fires. `None` = no TTL.
    pub ttl_secs: Option<u64>,
    /// What to do when `ttl_secs` elapses. `None` = Block (never auto-resolve).
    pub fallback: Option<crate::mcp::approval::ApprovalFallback>,
    /// Wall-clock time the request was created (for age computation).
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ── DispatchState ────────────────────────────────────────────────────────────

/// Run-level wrapper. Holds the root `LayerState` plus (in Phase 2+) a map
/// of sub-tree `LayerState`s keyed by sub-lead id.
///
/// Layer access is explicit: callers reach the root layer via
/// `state.root.<field>` (or `state.root_layer()`), and sub-tree layers via
/// `state.subleads.read().await.get(sublead_id)`. Handlers that dispatch on
/// `_meta.actor_role` should route through
/// `crate::mcp::server::resolve_layer_for_caller`.
///
/// ## Lock access rule (DO NOT VIOLATE)
///
/// Cross-layer lookups (reading `subleads`, `sublead_results`, or
/// `worker_layer_index`) on an async code path use `.read().await`,
/// **never** `.try_read().ok()`. A failed `try_read` followed by `.ok()`
/// is a silent-misrouting hazard — the caller quietly falls through to
/// the default (usually the root layer) and reads/writes the wrong
/// sub-tree's state without any error signal. This is how an earlier
/// version of `kv_wait` ended up returning peer-visibility-violating
/// data across sub-trees; see commit 00b05a2.
///
/// The `try_read` uses in the `Debug` impl below are the *only* allowed
/// uses: they are deliberately non-blocking because a blocking `Debug`
/// would deadlock under log-while-holding-lock patterns, and the lengths
/// they report are best-effort diagnostic output, not authoritative.
///
/// New cross-layer lookups MUST use `.read().await` — or, better, route
/// through `resolve_layer_for_caller` (see `crate::mcp::server`) which
/// encapsulates the correct lock discipline for depth-2 routing.
pub struct DispatchState {
    pub root: Arc<LayerState>,
    /// Sub-tree layers keyed by sub-lead id. Empty in the depth-1 case.
    /// Populated by `spawn_sublead` in Phase 2.
    pub subleads: RwLock<HashMap<String, Arc<LayerState>>>,
    /// Terminal records for sub-leads that have been reconciled. Keyed by
    /// sublead_id. Populated by `reconcile_terminated_sublead`; consulted by
    /// `wait_for_actor_internal` to satisfy `wait_actor(sublead_id)` calls.
    pub sublead_results: RwLock<HashMap<String, SubleadTerminalRecord>>,
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
    /// Most-recent approval response per actor id. Populated by the
    /// approval MCP handlers when they return; consulted at actor-
    /// termination time by `approval_driven_termination` to reclassify
    /// silent exits as `TaskStatus::ApprovalRejected`. See the
    /// `LastApprovalResponse` doc for the full lifecycle.
    pub last_approval_response: RwLock<HashMap<String, LastApprovalResponse>>,
    /// Rolling view of Anthropic API health derived from classified worker
    /// failures. Consulted by `handle_spawn_worker` /
    /// `handle_spawn_sublead` to refuse new spawns while rate-limit or
    /// auth conditions persist — otherwise a loop of failing workers
    /// burns budget faster than the operator can intervene. Updated
    /// alongside the `TaskRecord` persist in every completion path.
    pub api_health: Arc<crate::dispatch::failure_detection::ApiHealth>,
}

impl std::fmt::Debug for DispatchState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchState")
            .field("root", &self.root)
            .field("subleads", &self.subleads.try_read().map(|g| g.len()).ok())
            .field(
                "sublead_results",
                &self.sublead_results.try_read().map(|g| g.len()).ok(),
            )
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
            sublead_results: RwLock::new(HashMap::new()),
            worker_layer_index: RwLock::new(HashMap::new()),
            run_leases: Arc::new(RunLeaseRegistry::new()),
            last_approval_response: RwLock::new(HashMap::new()),
            api_health: Arc::new(crate::dispatch::failure_detection::ApiHealth::new()),
        }
    }

    /// Record the most recent approval response delivered to `actor_id`.
    /// Called by approval MCP handlers immediately before they return to
    /// the caller. Used downstream by `approval_driven_termination` to
    /// reclassify silent exits.
    pub async fn record_last_approval_response(
        &self,
        actor_id: &str,
        approved: bool,
        from_ttl: bool,
    ) {
        let mut slot = self.last_approval_response.write().await;
        slot.insert(
            actor_id.to_string(),
            LastApprovalResponse {
                approved,
                from_ttl,
                received_at: chrono::Utc::now(),
            },
        );
    }

    /// If the given actor's most-recent approval response was negative and
    /// recent (within 30 s of now), return the reclassification kind.
    /// Returns `None` if no reclassification applies.
    ///
    /// Used by actor-termination paths to turn `TaskStatus::Success` into
    /// `ApprovalRejected` (operator/policy rejection) or `ApprovalTimedOut`
    /// (TTL-fired fallback) when the actor clearly exited because of the
    /// rejection rather than by completing real work after it.
    ///
    /// The 30-second recency window is empirical: claude subprocesses that
    /// exit due to a rejected approval typically terminate within a few
    /// seconds (the apology message + exit). 30 seconds gives generous
    /// headroom while still excluding actors that received a rejection,
    /// did substantial subsequent work, and then terminated for unrelated
    /// reasons.
    pub async fn approval_driven_termination(
        &self,
        actor_id: &str,
    ) -> Option<ApprovalTerminationKind> {
        let slot = self.last_approval_response.read().await;
        let entry = slot.get(actor_id)?;
        if entry.approved {
            return None;
        }
        let age = chrono::Utc::now() - entry.received_at;
        if age.num_seconds() > 30 {
            return None;
        }
        if entry.from_ttl {
            Some(ApprovalTerminationKind::TimedOut)
        } else {
            Some(ApprovalTerminationKind::Rejected)
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
            approval_rules: vec![],
            container: None,
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
        assert_eq!(st.root.active_worker_count().await, 0);
    }

    #[tokio::test]
    async fn budget_remaining_reflects_spent() {
        let st = mk_state(Some(10.0), None);
        assert_eq!(st.root.budget_remaining().await, Some(10.0));
        *st.root.spent_usd.lock().await = 3.5;
        assert_eq!(st.root.budget_remaining().await, Some(6.5));
    }

    #[tokio::test]
    async fn budget_remaining_is_none_when_uncapped() {
        let st = mk_state(None, None);
        assert_eq!(st.root.budget_remaining().await, None);
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
        assert!(st.root.approval_bridge.lock().await.is_empty());
        assert!(st.root.approval_queue.lock().await.is_empty());
        assert!(matches!(
            st.root.approval_policy,
            crate::dispatch::state::ApprovalPolicy::Block
        ));
        assert!(st.root.control_writer.lock().await.is_none());
    }

    #[tokio::test]
    async fn worker_counters_default_zero() {
        let st = mk_state(None, None);
        let c = st
            .root
            .worker_counters
            .read()
            .await
            .get("absent")
            .cloned()
            .unwrap_or_default();
        assert_eq!(c.pause_count, 0);
    }
}
