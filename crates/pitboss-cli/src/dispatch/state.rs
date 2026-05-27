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
    /// Typed terminal classification (#568). Wire/JSON shape is unchanged
    /// — see [`crate::control::protocol::TerminationOutcome`] for the
    /// closed value set and its on-the-wire string mapping.
    pub outcome: crate::control::protocol::TerminationOutcome,
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

/// Canonical actor identity, bound at token-mint time. The MCP server
/// validates `_meta.token` from each tools/call against the
/// `actor_tokens` table on `DispatchState` and uses the resolved
/// `ActorIdentity` for ALL authz — never trusting the wire's
/// `_meta.actor_id` / `_meta.actor_role`. Closes #145.
#[derive(Debug, Clone)]
pub struct ActorIdentity {
    pub actor_id: String,
    pub actor_role: String,
}

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

/// What happens to an actor's terminal status when its most recent
/// `permission_prompt` was denied. (#377)
///
/// Pre-#377, `approval_driven_termination` reclassified any clean exit
/// within 30s of a denial as `ApprovalRejected`. Post-#368 (when the
/// deny path actually delivered structured messages to the model),
/// models began genuinely adapting around denials — using alternative
/// allowlisted tools to complete their task and then exiting cleanly.
/// The 30s reclassification window mislabeled these as failures.
///
/// `Adapt` (default) trusts the actor's exit code: clean exit means
/// success regardless of denial history. The audit trail
/// (`events.jsonl::tool_denied`) and counters
/// (`approvals_rejected`) remain the source of truth for what was
/// blocked. `Reclassify` preserves the legacy 30s-window heuristic
/// for operators who want fast-give-up vs. completed-successfully
/// distinguished in the status table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialTerminationPolicy {
    /// Never reclassify. Actor's exit code is its terminal status.
    /// (Default — matches the user-facing "soft denial" UX.)
    #[default]
    Adapt,
    /// Legacy 30s-window reclassification: if the actor exited cleanly
    /// within 30s of a recent denial, status becomes `ApprovalRejected`
    /// (or `ApprovalTimedOut` for TTL-fired fallbacks).
    Reclassify,
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

/// Fields shared by `QueuedApproval` (pre-TUI-attach) and `BridgeEntry`
/// (post-attach). Extracted so adding a new approval-metadata field
/// (e.g. priority, category, retry budget) touches one type instead of
/// three. Equivalent to the "Phase 4 unification" TODO that previously
/// lived on `PendingApproval`'s doc comment — except that `PendingApproval`
/// is a richer record-level type (typed `ApprovalCategory`, required
/// `ttl_secs`/`fallback`, actor lineage) so it stays separate for now.
/// (F-ARCH-12)
#[derive(Debug)]
pub struct ApprovalMetadata {
    /// Actor that submitted the approval request (for counter attribution
    /// and audit-event correlation).
    pub task_id: String,
    /// Short summary line rendered in the TUI modal + approval-list pane.
    pub summary: String,
    /// Typed structured plan body (rationale, resources, risks, rollback).
    /// `None` for bare summary-only approvals.
    pub plan: Option<crate::mcp::approval::ApprovalPlan>,
    /// Discriminator between `Action` (in-flight) and `Plan` (pre-flight)
    /// approvals — controls which modal header the TUI renders. Carried
    /// through the queue so the TUI renders the right modal header when
    /// the queue drains.
    pub kind: crate::control::protocol::ApprovalKind,
    /// Seconds after `created_at` before the fallback fires. `None` = no
    /// TTL (preserves v0.5 behavior of "never expires").
    pub ttl_secs: Option<u64>,
    /// What to do when `ttl_secs` elapses. `None` = Block (never
    /// auto-resolve; preserves v0.5 behavior).
    pub fallback: Option<crate::mcp::approval::ApprovalFallback>,
    /// Wall-clock time the request was created (for age computation).
    /// Used only when `ttl_secs` is `Some`.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// An approval request that arrived before a TUI attached. Block-mode runs
/// queue these; they drain when the next TUI connects.
pub struct QueuedApproval {
    pub request_id: String,
    pub responder: oneshot::Sender<ApprovalResponse>,
    pub metadata: ApprovalMetadata,
}

/// An approval request that has been handed to a live TUI via the bridge map.
///
/// Carries TTL metadata (via `metadata`) so `expire_layer_approvals` can
/// expire bridge entries that the operator never acted on — the same
/// guarantee it provides for `approvals.queue` entries. Without this, an
/// approval that moves from queue to bridge (when a TUI connects) loses TTL
/// coverage: the queue is empty so the watcher does nothing, while the
/// bridge has no metadata to check against.
///
/// Also retains the display fields (`summary`, `plan`, `kind` on
/// `metadata`) so a TUI that connects after a previous TUI died without
/// responding can have the pending approval replayed from the bridge.
/// Before #102, transfer from queue→bridge dropped these fields, and a
/// reconnecting TUI saw nothing for the still-live responder until a TTL
/// fallback resolved it.
pub struct BridgeEntry {
    /// Oneshot sender; deliver the operator's decision here.
    pub responder: oneshot::Sender<ApprovalResponse>,
    pub metadata: ApprovalMetadata,
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
    /// Layer handles for sub-leads that have terminated (and therefore
    /// been removed from `subleads`). Kept alive for the duration of the
    /// run so `ListWorkers` can still surface the sub-tree's actors —
    /// otherwise a TUI/console attached late in a run with short-lived
    /// sub-trees sees only the actors that happened to still be active
    /// at the moment of the snapshot, with no way to inspect what came
    /// before.
    pub terminated_sublead_layers: RwLock<Vec<Arc<LayerState>>>,
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
    /// Run-global mailbox + artifact store for the
    /// [`crate::communication`] MCP tools. One run-wide plane (not
    /// per-layer) because parent mediation and `artifact_grant` cross
    /// sub-tree boundaries. Cheap when `[communication].mode = "disabled"`
    /// — the store still exists, but every handler returns
    /// `CommunicationError::Disabled` before touching it.
    pub communication: Arc<crate::communication::CommunicationStore>,
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
    /// Per-actor authentication tokens. Minted at spawn time (lead at
    /// dispatch start, workers at handle_spawn_worker, subleads at
    /// handle_spawn_sublead) and embedded in each actor's mcp-config.json
    /// via the bridge's `--token` arg. The MCP server validates the token
    /// on every tools/call and uses the bound identity (NOT the wire
    /// `_meta.actor_id`) for authz. Closes issue #145.
    pub actor_tokens: RwLock<HashMap<String, ActorIdentity>>,
    /// Sub-lead id → resolved `[[sublead_type]]` profile id, populated
    /// by `handle_spawn_sublead` after the sub-lead's `LayerState` is
    /// registered (#252). Run-global (only the root layer hosts
    /// sub-leads — depth-2 cap), so it lives on `DispatchState` rather
    /// than mirroring the per-layer `workers.actor_types` map.
    ///
    /// Read by `handle_permission_prompt` to look up a sub-lead
    /// caller's profile when applying the typed-profile auto-approve /
    /// auto-deny short-circuit. `None`/missing entry means the sub-lead
    /// was spawned untyped — the bridge fallback runs as before.
    pub sublead_actor_types: RwLock<HashMap<String, String>>,
    /// Per-run event-stream log (#259, co-spec'd with #438). Owns the
    /// run-scoped monotonic seq counter and an optional append-only
    /// `<run-dir>/events.jsonl` file gated by `[run].emit_event_stream`.
    /// Live wire emits read seq from here; persistence is a no-op when
    /// the flag is off. Cloneable via `Arc`.
    pub event_log: Arc<crate::control::event_log::EventLog>,
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
        let communication_config = manifest.communication.clone();
        let communication_dir = run_subdir.clone();
        let event_log = Arc::new(crate::control::event_log::EventLog::new(
            &run_subdir,
            manifest.emit_event_stream,
        ));
        // PR-H of #259: run-scoped event broadcast bus. Subscribers
        // are spawned below (persistence) and inside `serve_connection`
        // (per-connection pump). Sub-leads inherit this same Sender via
        // `with_events_tx` so the whole run funnels into one bus.
        let (events_tx, _) =
            tokio::sync::broadcast::channel(crate::dispatch::layer::EVENTS_BROADCAST_CAP);
        let root = Arc::new(
            LayerState::new(
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
            )
            .with_event_log(event_log.clone())
            .with_events_tx(events_tx.clone()),
        );

        // PR-H: persistent events.jsonl subscriber. Drains the bus
        // and persists every envelope, regardless of whether a TUI /
        // web bridge is connected. This is what makes headless
        // dispatches produce a complete log: the bus is alive for
        // the lifetime of the run, the subscriber is spawned before
        // any emission can happen, and `event_log.persist` no-ops
        // gracefully when `emit_event_stream = false`.
        //
        // The task exits when all `events_tx` clones (root + every
        // sub-tree layer) drop, which happens after `DispatchState`
        // is dropped at run finalization.
        {
            let mut rx = events_tx.subscribe();
            let log = event_log.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(envelope) => log.persist(&envelope).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                lagged = n,
                                "events.jsonl persistence subscriber lagged; \
                                 some envelopes will be missing from the log",
                            );
                            // Persist a sentinel so downstream replay can
                            // detect the discontinuity instead of silently
                            // reading a file with missing records.
                            let sentinel = crate::control::protocol::EventEnvelope {
                                actor_path: crate::dispatch::actor::ActorPath::default(),
                                seq: log.next_seq(),
                                event: crate::control::protocol::ControlEvent::PersistenceGap {
                                    dropped: n,
                                },
                            };
                            log.persist(&sentinel).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        Self {
            root,
            subleads: RwLock::new(HashMap::new()),
            sublead_results: RwLock::new(HashMap::new()),
            terminated_sublead_layers: RwLock::new(Vec::new()),
            worker_layer_index: RwLock::new(HashMap::new()),
            run_leases: Arc::new(RunLeaseRegistry::new()),
            communication: Arc::new(crate::communication::CommunicationStore::new(
                communication_config,
                communication_dir,
            )),
            last_approval_response: RwLock::new(HashMap::new()),
            api_health: Arc::new(crate::dispatch::failure_detection::ApiHealth::new()),
            actor_tokens: RwLock::new(HashMap::new()),
            sublead_actor_types: RwLock::new(HashMap::new()),
            event_log,
        }
    }

    /// Register a freshly-built sub-tree `LayerState` under `sublead_id`.
    /// Performs three coupled side-effects atomically (from the caller's
    /// perspective): (1) inserts the layer into `self.subleads`,
    /// (2) installs the per-sublead cancel-cascade watcher, and
    /// (3) propagates any in-flight drain/terminate state from
    /// `self.root.cancel` to the new sub-layer's cancel.
    ///
    /// Centralizes the previously-inlined sequence at
    /// `dispatch/sublead.rs:340-383` so the watcher install + eager
    /// cascade can never drift out of order.  Pinned by the integration
    /// tests in `tests/cancel_cascade_flows.rs`.
    ///
    /// **Step order is load-bearing — do not reorder (F-CONC-8 #498):**
    ///
    /// - `insert` MUST happen before `cascade_to`. The cascade-watcher
    ///   installed by `install_cascade_cancel_watcher` walks
    ///   `cascade_to_subleads`, which reads `self.subleads`; if the
    ///   sublead were not yet inserted, a concurrent root-cancel could
    ///   miss this sub-layer and the eager `cascade_to` at step 3 would
    ///   be the only signal that ever reaches it.
    /// - `install_sublead_cancel_watcher` MUST happen before the eager
    ///   `cascade_to`. The per-sub-layer watcher is fire-once on the
    ///   sub-layer's own `CancelToken`; if `cascade_to` runs first and
    ///   already toggles drain/terminate, the watcher started afterwards
    ///   still observes the canceled state and fans out to workers as
    ///   intended (the watcher's `select!` over `await_drain`/`await_terminate`
    ///   resolves immediately for an already-canceled token). The
    ///   ordering exists so the watcher is *registered* on the canonical
    ///   pre-cancel state for the (more common) non-canceled path.
    ///
    /// Between steps 1 and 3 there is a benign window where a concurrent
    /// root cancellation's `cascade_to_subleads` walk could fire and
    /// no-op (root not yet canceled when walked) — the eager `cascade_to`
    /// at step 3 closes this window. The sublead subprocess is not
    /// launched until after `register_sublead` returns
    /// (`dispatch/sublead.rs::spawn_sublead_session`), so no worker
    /// observes the transient state.
    pub async fn register_sublead(&self, sublead_id: String, sub_layer: Arc<LayerState>) {
        self.subleads
            .write()
            .await
            .insert(sublead_id, sub_layer.clone());
        crate::dispatch::signals::install_sublead_cancel_watcher(sub_layer.clone());
        self.root.cancel.cascade_to(&sub_layer.cancel);
    }

    /// Walk every registered sub-tree layer and cascade the root's
    /// current cancel state to it via `CancelToken::cascade_to`.  Used
    /// by the root cascade-watcher tasks installed by
    /// `install_cascade_cancel_watcher` — both the drain and terminate
    /// watchers funnel through this method so the cascade rule
    /// (terminate dominates drain) is encoded in exactly one place.
    pub async fn cascade_to_subleads(&self) {
        let subleads = self.subleads.read().await;
        for (sublead_id, sub_layer) in subleads.iter() {
            tracing::info!(sublead_id = %sublead_id, "cascading cancel state to sub-tree");
            self.root.cancel.cascade_to(&sub_layer.cancel);
        }
    }

    /// Mint a fresh authentication token bound to the (actor_id, role)
    /// pair. The token is later embedded into the actor's
    /// mcp-config.json (consumed by `pitboss mcp-bridge --token <hex>`)
    /// and validated on every inbound MCP tools/call. Closes #145.
    ///
    /// The token format is a UUIDv7 string. UUIDv7 is fine for this
    /// threat model — local same-UID processes are the boundary; the
    /// token raises the bar from "any local process" to "process that
    /// has read the actor's mcp-config.json file (mode 0o600 in the
    /// run_subdir)".
    pub async fn mint_token(&self, actor_id: &str, role: &str) -> String {
        let token = Uuid::now_v7().to_string();
        self.actor_tokens.write().await.insert(
            token.clone(),
            ActorIdentity {
                actor_id: actor_id.to_string(),
                actor_role: role.to_string(),
            },
        );
        token
    }

    /// Resolve an authentication token to its bound `ActorIdentity`,
    /// or `None` if the token is unknown / has been revoked. Used by
    /// the MCP server's per-call authz check.
    pub async fn lookup_token(&self, token: &str) -> Option<ActorIdentity> {
        self.actor_tokens.read().await.get(token).cloned()
    }

    /// Revoke a single token by its string value. Returns `true` if
    /// the token was present and removed, `false` if it was already
    /// absent (or never minted). Closes F-SEC-1 (#523) on actor-exit
    /// paths where each iteration of a subprocess owns exactly one
    /// token and the caller can identify which one to drop.
    ///
    /// Preferred over [`revoke_tokens_for_actor`] for workers because
    /// the worker has a kill+resume re-mint pattern: a stale
    /// `run_worker` tail revoking ALL tokens for the actor_id could
    /// race-drop a fresh token that `spawn_resume_worker` just minted
    /// for the resumed subprocess. Each iteration revoking only its
    /// own token-string is race-free.
    pub async fn revoke_token(&self, token: &str) -> bool {
        self.actor_tokens.write().await.remove(token).is_some()
    }

    /// Revoke every token bound to `actor_id`. Returns the count of
    /// tokens removed. Closes F-SEC-1 (#523) for actor-exit paths
    /// without a kill+resume re-mint pattern (sub-lead finalize, lead
    /// finalize). For worker exits, prefer [`revoke_token`] with the
    /// per-iteration token-string — see that method's docs.
    ///
    /// `actor_tokens` is keyed by token-string (UUIDv7), so revocation
    /// by `actor_id` is a scan over the table. The table is bounded by
    /// the run's concurrent-actor count (a few dozen), so the scan is
    /// negligible.
    ///
    /// Idempotent: calling twice for the same `actor_id` is safe; the
    /// second call returns 0.
    pub async fn revoke_tokens_for_actor(&self, actor_id: &str) -> usize {
        let mut tokens = self.actor_tokens.write().await;
        let before = tokens.len();
        tokens.retain(|_token, identity| identity.actor_id != actor_id);
        before - tokens.len()
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
    /// Behavior is gated on `[run].denial_termination_policy` (#377):
    ///
    /// - `Adapt` (default, post-#377) — always returns `None`. The
    ///   actor's exit code is its terminal status; the audit trail
    ///   (`events.jsonl::tool_denied`) and counters
    ///   (`approvals_rejected`) carry the denial detail. Right answer
    ///   when models actually adapt around denials, which became real
    ///   after #368 made the deny path deliver structured messages.
    /// - `Reclassify` — legacy 30-second window heuristic. Returns
    ///   `Rejected` (operator / policy rejection) or `TimedOut`
    ///   (TTL-fired) when the actor exited cleanly within 30 s of a
    ///   denial. Useful for operators who want fast-give-up vs.
    ///   completed-via-adaptation distinguished in `pitboss status`,
    ///   accepting that successful adaptations within the window are
    ///   misclassified as failures.
    pub async fn approval_driven_termination(
        &self,
        actor_id: &str,
    ) -> Option<ApprovalTerminationKind> {
        // Default to Adapt when the manifest leaves the field unset.
        let policy = self
            .root
            .manifest
            .denial_termination_policy
            .unwrap_or_default();
        if matches!(policy, DenialTerminationPolicy::Adapt) {
            return None;
        }
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
    use crate::test_support::state_builder::TestStateBuilder;

    fn mk_state(budget: Option<f64>, max_workers: Option<u32>) -> Arc<DispatchState> {
        let mut b = TestStateBuilder::new()
            .lead_id("lead-1")
            .tokio_spawner()
            .claude_binary(PathBuf::from("/bin/false"));
        b = match budget {
            Some(usd) => b.budget(usd),
            None => b.no_budget(),
        };
        b = match max_workers {
            Some(n) => b.max_workers(n),
            None => b.no_max_workers(),
        };
        let (dir, state) = b.build();
        // Leak the TempDir: the state holds PathBufs into it and dropping
        // would invalidate on-disk paths the tests later read.
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
        st.root.budget.spent_usd.store(3.5);
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
        assert!(st.root.approvals.bridge.lock().await.is_empty());
        assert!(st.root.approvals.queue.lock().await.is_empty());
        assert!(matches!(
            st.root.approvals.policy,
            crate::dispatch::state::ApprovalPolicy::Block
        ));
        assert!(st.root.control_writer.lock().await.is_none());
    }

    #[tokio::test]
    async fn worker_counters_default_zero() {
        let st = mk_state(None, None);
        let c = st
            .root
            .workers
            .counters
            .read()
            .await
            .get("absent")
            .cloned()
            .unwrap_or_default();
        assert_eq!(c.pause_count, 0);
    }

    /// F-SEC-1 (#523): revoke removes the token from the lookup table.
    #[tokio::test]
    async fn revoke_invalidates_token() {
        let st = mk_state(None, None);
        let tok = st.mint_token("w-1", "worker").await;
        assert!(st.lookup_token(&tok).await.is_some());

        let count = st.revoke_tokens_for_actor("w-1").await;
        assert_eq!(count, 1);
        assert!(st.lookup_token(&tok).await.is_none());
    }

    /// F-SEC-1 (#523): kill+resume mints multiple tokens for the same
    /// actor_id; revoke must drop all of them in one call.
    #[tokio::test]
    async fn revoke_clears_all_tokens_for_actor() {
        let st = mk_state(None, None);
        let t1 = st.mint_token("w-1", "worker").await;
        let t2 = st.mint_token("w-1", "worker").await;
        let t3 = st.mint_token("w-1", "worker").await;

        let count = st.revoke_tokens_for_actor("w-1").await;
        assert_eq!(count, 3);
        assert!(st.lookup_token(&t1).await.is_none());
        assert!(st.lookup_token(&t2).await.is_none());
        assert!(st.lookup_token(&t3).await.is_none());
    }

    /// F-SEC-1 (#523): revoking one actor must not affect peers.
    #[tokio::test]
    async fn revoke_scopes_to_named_actor_only() {
        let st = mk_state(None, None);
        let worker_tok = st.mint_token("w-1", "worker").await;
        let sublead_tok = st.mint_token("sub-A", "sublead").await;

        let count = st.revoke_tokens_for_actor("w-1").await;
        assert_eq!(count, 1);
        assert!(st.lookup_token(&worker_tok).await.is_none());
        assert!(
            st.lookup_token(&sublead_tok).await.is_some(),
            "sibling actor's token must survive a peer's revocation"
        );
    }

    /// F-SEC-1 (#523): revoke is idempotent — second call returns 0.
    #[tokio::test]
    async fn revoke_is_idempotent() {
        let st = mk_state(None, None);
        let _ = st.mint_token("w-1", "worker").await;
        assert_eq!(st.revoke_tokens_for_actor("w-1").await, 1);
        assert_eq!(st.revoke_tokens_for_actor("w-1").await, 0);
    }

    /// F-SEC-1 (#523): singular `revoke_token` drops exactly the named
    /// token-string and leaves siblings alone. Used by the worker
    /// per-iteration finalize path to avoid the kill+resume race.
    #[tokio::test]
    async fn revoke_token_drops_only_named_token() {
        let st = mk_state(None, None);
        let t1 = st.mint_token("w-1", "worker").await;
        let t2 = st.mint_token("w-1", "worker").await;

        assert!(st.revoke_token(&t1).await);
        assert!(st.lookup_token(&t1).await.is_none());
        assert!(
            st.lookup_token(&t2).await.is_some(),
            "sibling token for the same actor_id must survive a per-token revoke"
        );
    }

    /// F-SEC-1 (#523): per-token revoke is also idempotent — second
    /// call on an already-revoked token returns false.
    #[tokio::test]
    async fn revoke_token_is_idempotent() {
        let st = mk_state(None, None);
        let t = st.mint_token("w-1", "worker").await;
        assert!(st.revoke_token(&t).await);
        assert!(!st.revoke_token(&t).await);
    }

    /// F-SEC-1 (#523): revoking an unknown token-string is a no-op
    /// returning false (defensive: a buggy caller can't poison the
    /// table by passing garbage).
    #[tokio::test]
    async fn revoke_token_unknown_is_noop() {
        let st = mk_state(None, None);
        assert!(!st.revoke_token("not-a-real-token").await);
    }
}
