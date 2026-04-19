//! Lifecycle of the pitboss MCP server (unix socket transport).

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::service::ServiceExt;
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};

use crate::dispatch::layer::LayerState;
use crate::dispatch::state::DispatchState;
use crate::mcp::tools::{
    handle_cancel_worker, handle_continue_worker, handle_list_workers, handle_pause_worker,
    handle_propose_plan, handle_reprompt_worker, handle_request_approval, handle_spawn_worker,
    handle_wait_for_actor, handle_wait_for_any, handle_wait_for_worker, handle_worker_status,
    ContinueWorkerArgs, PauseWorkerArgs, ProposePlanArgs, RepromptWorkerArgs, RequestApprovalArgs,
    SpawnWorkerArgs, TaskIdArgs, WaitActorRequest, WaitForAnyArgs, WaitForWorkerArgs,
};

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SpawnSubleadRequest {
    /// The prompt the sub-lead's Claude session will start with.
    prompt: String,
    /// Model name for the sub-lead.
    model: String,
    /// Hard budget cap for this sub-tree, USD. Required unless
    /// read_down=true (then None means "share root's pool").
    #[serde(default)]
    budget_usd: Option<f64>,
    /// Hard worker count cap for this sub-tree.
    #[serde(default)]
    max_workers: Option<u32>,
    /// Wall-clock cap on the sub-lead's Claude session, seconds.
    #[serde(default)]
    lead_timeout_secs: Option<u64>,
    /// Snapshot data copied into the sub-tree's /ref/* at spawn time.
    #[serde(default)]
    initial_ref: std::collections::HashMap<String, serde_json::Value>,
    /// If true, root gets read-only visibility into the sub-tree's
    /// store; required for shared-pool resource mode (omitted budget/
    /// max_workers).
    #[serde(default)]
    read_down: bool,
    /// Caller identity injected by mcp-bridge (actor_id + actor_role).
    #[serde(rename = "_meta", default)]
    #[schemars(skip)]
    meta: Option<CallerMeta>,
}

/// Caller identity metadata injected into tool requests by the MCP bridge.
#[allow(dead_code)]
#[derive(serde::Deserialize, Debug, Clone)]
struct CallerMeta {
    actor_id: String,
    actor_role: String,
}

/// Compute the socket path for a given run. Falls back to the run_dir if
/// $XDG_RUNTIME_DIR is unset or non-writable.
pub fn socket_path_for_run(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("pitboss");
        if std::fs::create_dir_all(&p).is_ok() {
            return p.join(format!("{}.sock", run_id));
        }
    }
    // Fallback: alongside the run artifacts.
    let p = run_dir.join(run_id.to_string());
    let _ = std::fs::create_dir_all(&p);
    p.join("mcp.sock")
}

// ── Per-layer KV routing helpers (Phase 3.1) ────────────────────────────────

use crate::shared_store::ActorRole;

/// Resolve the `LayerState` whose KvStore should service a KV operation.
///
/// - `Lead` (root lead) → always the root layer.
/// - `Sublead` with id S → the sub-tree layer for S.
/// - `Worker` → look up which layer registered this worker at spawn time via
///   `DispatchState::worker_layer_index`. `None` (root-layer worker) returns
///   the root layer; `Some(sublead_id)` returns that sub-tree's layer.
///
/// The `subleads_guard` is passed in so the caller can hold the read-lock
/// across the full KV operation (single lock acquisition per MCP tool call).
fn resolve_layer_for_caller<'a>(
    state: &'a DispatchState,
    actor_id: &str,
    actor_role: ActorRole,
    subleads_guard: &'a tokio::sync::RwLockReadGuard<
        'a,
        std::collections::HashMap<String, Arc<LayerState>>,
    >,
) -> Result<&'a Arc<LayerState>, ErrorData> {
    match actor_role {
        ActorRole::Lead => Ok(&state.root),
        ActorRole::Sublead => subleads_guard.get(actor_id).ok_or_else(|| {
            ErrorData::invalid_request(format!("unknown sublead_id: {actor_id}"), None)
        }),
        ActorRole::Worker => {
            // Use the worker_layer_index for O(1) lookup. We use try_read()
            // since we already hold a write-free path (this is called under
            // the subleads read-lock, not a write-lock).
            let layer_opt = state
                .worker_layer_index
                .try_read()
                .ok()
                .and_then(|idx| idx.get(actor_id).cloned());
            match layer_opt {
                // None (or missing from index) → root layer.
                None | Some(None) => Ok(&state.root),
                // Some(sublead_id) → sub-tree layer.
                Some(Some(sublead_id)) => subleads_guard.get(&sublead_id).ok_or_else(|| {
                    ErrorData::invalid_request(
                        format!("worker {actor_id} registered in unknown sub-tree {sublead_id}"),
                        None,
                    )
                }),
            }
        }
    }
}

/// Returns the peer-slot owner id if `key` is under `/peer/<id>/...`,
/// or `None` for other namespaces.
fn parse_peer_path(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("/peer/")?;
    // Exclude the /peer/self/... alias — it should be resolved before this
    // point, but guard defensively.
    if rest.starts_with("self/") || rest == "self" {
        return None;
    }
    let id = rest.split('/').next()?;
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Strict peer-visibility predicate (spec §4.2).
///
/// `/peer/<X>/*` is readable at `layer` by:
/// - X itself (the slot owner).
/// - The layer's lead (`layer.lead_id`).
///
/// Workers within the same layer CANNOT read each other's peer slots.
/// Sibling sub-leads CANNOT read each other's peer slots.
/// The TUI / operator bypasses this predicate entirely (it reads directly
/// from the `SharedStore` without going through this MCP handler).
fn can_read_peer_slot(layer: &LayerState, caller_id: &str, target_id: &str) -> bool {
    caller_id == target_id || caller_id == layer.lead_id
}

pub struct McpServer {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

/// The rmcp `ServerHandler` that exposes the six pitboss tools to the lead
/// Hobbit via a per-connection MCP session.
///
/// Holds an `Arc<Mutex<Option<String>>>` that records the first actor_id
/// observed in this connection's `_meta` fields. Used for per-connection
/// lease cleanup: on disconnect, we call
/// `SharedStore::release_all_for_actor(actor_id)` so dropped bridges
/// don't leave their leases held until TTL expiry.
#[derive(Clone)]
pub struct PitbossHandler {
    state: Arc<DispatchState>,
    tool_router: ToolRouter<Self>,
    connection_actor: Arc<tokio::sync::Mutex<Option<String>>>,
}

impl PitbossHandler {
    pub fn new(state: Arc<DispatchState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
            connection_actor: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Return a fresh handler instance for a new MCP connection — same
    /// shared dispatcher state and tool router, but a dedicated
    /// `connection_actor` slot so different connections don't trample
    /// each other's identity tracking.
    pub fn for_connection(&self) -> Self {
        Self {
            state: self.state.clone(),
            tool_router: self.tool_router.clone(),
            connection_actor: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Handle to the per-connection actor_id slot. Caller holds an
    /// `Arc::clone` so it can read the observed actor_id after the
    /// session ends (for `release_all_for_actor` cleanup).
    pub fn connection_actor_handle(&self) -> Arc<tokio::sync::Mutex<Option<String>>> {
        self.connection_actor.clone()
    }

    /// Record the actor_id on the first MCP tool call that carries one.
    /// Later calls on the same connection are no-ops (first-seen wins).
    async fn note_actor(&self, actor_id: &str) {
        if actor_id.is_empty() {
            return;
        }
        let mut slot = self.connection_actor.lock().await;
        if slot.is_none() {
            *slot = Some(actor_id.to_string());
        }
    }
}

#[tool_router]
impl PitbossHandler {
    #[tool(description = "Spawn a worker Hobbit. Returns {task_id, worktree_path}.")]
    async fn spawn_worker(
        &self,
        Parameters(args): Parameters<SpawnWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_spawn_worker(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        name = "spawn_sublead",
        description = "Create a new sub-tree with its own envelope. Only available to the root lead when allow_subleads=true. Returns {sublead_id}."
    )]
    async fn spawn_sublead(
        &self,
        Parameters(req): Parameters<SpawnSubleadRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        use crate::dispatch::sublead::{spawn_sublead as do_spawn, SubleadSpawnRequest};

        // Role check: only root_lead (or "lead" for v0.5 compat) may spawn sub-leads.
        extract_and_check_root_lead(&req.meta)?;

        let spawn_req = SubleadSpawnRequest {
            prompt: req.prompt,
            model: req.model,
            budget_usd: req.budget_usd,
            max_workers: req.max_workers,
            lead_timeout_secs: req.lead_timeout_secs,
            initial_ref: req.initial_ref,
            read_down: req.read_down,
        };

        match do_spawn(&self.state, spawn_req).await {
            Ok(sublead_id) => {
                to_structured_result(&serde_json::json!({ "sublead_id": sublead_id }))
            }
            Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
        }
    }

    #[tool(description = "Non-blocking status poll for a worker. Returns state + partial data.")]
    async fn worker_status(
        &self,
        Parameters(args): Parameters<TaskIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_worker_status(&self.state, &args.task_id).await {
            Ok(status) => to_structured_result(&status),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "Block until a specific worker exits (or timeout).")]
    async fn wait_for_worker(
        &self,
        Parameters(args): Parameters<WaitForWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_wait_for_worker(&self.state, &args.task_id, args.timeout_secs).await {
            Ok(rec) => to_structured_result(&rec),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Block until the named actor (worker or sub-lead) emits a terminal event."
    )]
    async fn wait_actor(
        &self,
        Parameters(req): Parameters<WaitActorRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_wait_for_actor(&self.state, &req.actor_id, req.timeout_secs).await {
            Ok(rec) => to_structured_result(&rec),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "Block until any of the listed workers exits.")]
    async fn wait_for_any(
        &self,
        Parameters(args): Parameters<WaitForAnyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_wait_for_any(&self.state, &args.task_ids, args.timeout_secs).await {
            Ok((id, rec)) => {
                let value = serde_json::json!({
                    "task_id": id,
                    "record": rec,
                });
                Ok(CallToolResult::structured(value))
            }
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "List all workers in the current run (excludes the lead).")]
    async fn list_workers(&self) -> Result<CallToolResult, ErrorData> {
        let summaries = handle_list_workers(&self.state).await;
        // MCP spec: structuredContent MUST be a record/object. Bare arrays
        // (as `Vec<WorkerSummary>` would serialize) are rejected by the
        // client's schema validator with "expected record, received array".
        to_structured_result(&serde_json::json!({ "workers": summaries }))
    }

    #[tool(description = "Cancel a worker by task_id. Sends SIGTERM, grace, SIGKILL.")]
    async fn cancel_worker(
        &self,
        Parameters(args): Parameters<TaskIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_cancel_worker(&self.state, &args.task_id).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Pause a running worker. Two modes: \"cancel\" (default — terminates subprocess, snapshots session_id; continue_worker re-spawns via claude --resume) or \"freeze\" (SIGSTOP the process in place; continue_worker SIGCONTs — zero state loss but risks dropped HTTP session on long pauses)."
    )]
    async fn pause_worker(
        &self,
        Parameters(args): Parameters<PauseWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_pause_worker(&self.state, &args.task_id, args.mode).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Continue a previously-paused worker. For Paused (cancel-mode): spawns claude --resume with prompt (default \"continue\"). For Frozen (freeze-mode): SIGCONT — prompt is ignored."
    )]
    async fn continue_worker(
        &self,
        Parameters(args): Parameters<ContinueWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_continue_worker(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Reprompt a running or paused worker with a new prompt via claude --resume. Preserves the worker's claude session for context continuity."
    )]
    async fn reprompt_worker(
        &self,
        Parameters(args): Parameters<RepromptWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_reprompt_worker(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Request operator approval before proceeding. Blocks until operator responds or timeout."
    )]
    async fn request_approval(
        &self,
        Parameters(args): Parameters<RequestApprovalArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_request_approval(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Propose a full execution plan for pre-flight operator approval. When [run].require_plan_approval=true, spawn_worker is blocked until a plan submitted via this tool is approved. Plan carries typed rationale/resources/risks/rollback for structured review. Blocks until operator responds or timeout. Distinct from request_approval, which gates individual in-flight actions."
    )]
    async fn propose_plan(
        &self,
        Parameters(args): Parameters<ProposePlanArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_propose_plan(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Read a value from the shared store. Returns { entry: null } when the key is missing. Paths starting with /peer/self/ are resolved against the caller's actor_id."
    )]
    async fn kv_get(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::KvGetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(m) = &args.meta {
            self.note_actor(&m.actor_id).await;
        }

        // Per-layer routing: resolve which LayerState's KvStore to target.
        // Falls back to the root-layer store when no identity is present
        // (backward-compatible with callers that omit _meta on reads).
        let subleads = self.state.subleads.read().await;
        let (layer, caller_id) = if let Some(m) = &args.meta {
            let layer =
                resolve_layer_for_caller(&self.state, &m.actor_id, m.actor_role, &subleads)?;
            (layer, m.actor_id.clone())
        } else {
            (&self.state.root, String::new())
        };

        // Strict peer-visibility check: /peer/<X>/* is readable only by X or
        // the layer's lead. Applied before the store lookup (fast-reject).
        if !caller_id.is_empty() {
            if let Some(target_id) = parse_peer_path(&args.path) {
                if !can_read_peer_slot(layer, &caller_id, target_id) {
                    return Err(shared_store_err(
                        &crate::shared_store::StoreError::Forbidden(format!(
                            "strict peer visibility: {caller_id} cannot read /peer/{target_id}/*; \
                             only {target_id} itself or the layer lead ({}) may read this slot",
                            layer.lead_id,
                        )),
                    ));
                }
            }
        }

        match crate::shared_store::tools::handle_kv_get(&layer.shared_store, args).await {
            // Wrap Option<Entry> in an object so structuredContent is a
            // record (per MCP spec). A bare null was rejected by the
            // client's schema validator in early dogfood runs.
            Ok(v) => to_structured_result(&serde_json::json!({ "entry": v })),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        description = "Write a value to the shared store. Namespace-authz checked against the caller's actor_role + actor_id. Workers should write to /peer/self/... (auto-resolves to /peer/<your-actor-id>/...) or /shared/..."
    )]
    async fn kv_set(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::KvSetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.note_actor(&args.meta.actor_id).await;
        // Per-layer routing: writes go to the caller's layer's KvStore.
        let subleads = self.state.subleads.read().await;
        let layer = resolve_layer_for_caller(
            &self.state,
            &args.meta.actor_id,
            args.meta.actor_role,
            &subleads,
        )?;
        match crate::shared_store::tools::handle_kv_set(&layer.shared_store, args).await {
            Ok(v) => to_structured_result(&v),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        description = "Atomic compare-and-swap. expected_version=0 means the key must not exist. Paths starting with /peer/self/ are resolved against the caller's actor_id."
    )]
    async fn kv_cas(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::KvCasArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.note_actor(&args.meta.actor_id).await;
        // Per-layer routing: CAS goes to the caller's layer's KvStore.
        let subleads = self.state.subleads.read().await;
        let layer = resolve_layer_for_caller(
            &self.state,
            &args.meta.actor_id,
            args.meta.actor_role,
            &subleads,
        )?;
        match crate::shared_store::tools::handle_kv_cas(&layer.shared_store, args).await {
            Ok(v) => to_structured_result(&v),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        description = "List metadata of entries matching a glob pattern. * is single-segment; ** is cross-segment. Caps at 1000 results. Returns { entries: [...] }. Patterns starting with /peer/self/ are resolved against the caller's actor_id (requires _meta)."
    )]
    async fn kv_list(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::KvListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(m) = &args.meta {
            self.note_actor(&m.actor_id).await;
        }

        // Per-layer routing.
        let subleads = self.state.subleads.read().await;
        let (layer, caller_id) = if let Some(m) = &args.meta {
            let layer =
                resolve_layer_for_caller(&self.state, &m.actor_id, m.actor_role, &subleads)?;
            (layer, m.actor_id.clone())
        } else {
            (&self.state.root, String::new())
        };

        // Strict peer-visibility check for /peer/<X>/* globs.
        // Only exact /peer/<id>/... prefix patterns are checked — a broad
        // glob like /peer/** is rejected unless the caller is the layer lead.
        if !caller_id.is_empty() {
            if let Some(target_id) = parse_peer_path(&args.glob) {
                if !can_read_peer_slot(layer, &caller_id, target_id) {
                    return Err(shared_store_err(
                        &crate::shared_store::StoreError::Forbidden(format!(
                            "strict peer visibility: {caller_id} cannot list /peer/{target_id}/*; \
                             only {target_id} itself or the layer lead ({}) may list this slot",
                            layer.lead_id,
                        )),
                    ));
                }
            }
        }

        match crate::shared_store::tools::handle_kv_list(&layer.shared_store, args).await {
            // Wrap Vec<ListMetadata> in an object — MCP spec requires
            // structuredContent to be a record.
            Ok(v) => to_structured_result(&serde_json::json!({ "entries": v })),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        description = "Block until a key is written (or exists with version >= min_version). Times out. Paths starting with /peer/self/ are resolved against the caller's actor_id."
    )]
    async fn kv_wait(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::KvWaitArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(m) = &args.meta {
            self.note_actor(&m.actor_id).await;
        }
        // Per-layer routing: wait on the caller's layer's KvStore.
        let subleads = self.state.subleads.read().await;
        let layer = if let Some(m) = &args.meta {
            resolve_layer_for_caller(&self.state, &m.actor_id, m.actor_role, &subleads)?
        } else {
            &self.state.root
        };
        match crate::shared_store::tools::handle_kv_wait(&layer.shared_store, args).await {
            Ok(v) => to_structured_result(&v),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        description = "Acquire a named lease with a TTL. wait_secs > 0 blocks up to that duration trying."
    )]
    async fn lease_acquire(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::LeaseAcquireArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.note_actor(&args.meta.actor_id).await;
        // Per-layer routing: acquire from the caller's layer's LeaseRegistry.
        let subleads = self.state.subleads.read().await;
        let layer = resolve_layer_for_caller(
            &self.state,
            &args.meta.actor_id,
            args.meta.actor_role,
            &subleads,
        )?;
        match crate::shared_store::tools::handle_lease_acquire(&layer.shared_store, args).await {
            Ok(v) => to_structured_result(&v),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        description = "Release a previously-acquired lease. Only the recorded holder can release."
    )]
    async fn lease_release(
        &self,
        Parameters(args): Parameters<crate::shared_store::tools::LeaseReleaseArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.note_actor(&args.meta.actor_id).await;
        // Per-layer routing: release from the caller's layer's LeaseRegistry.
        let subleads = self.state.subleads.read().await;
        let layer = resolve_layer_for_caller(
            &self.state,
            &args.meta.actor_id,
            args.meta.actor_role,
            &subleads,
        )?;
        match crate::shared_store::tools::handle_lease_release(&layer.shared_store, args).await {
            Ok(()) => Ok(CallToolResult::structured(serde_json::json!({"ok": true}))),
            Err(e) => Err(shared_store_err(&e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for PitbossHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "pitboss".into(),
                title: None,
                version: env!("CARGO_PKG_VERSION").into(),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Pitboss MCP server: coordinate worker Hobbits via six structured tools.".into(),
            ),
            ..Default::default()
        }
    }
}

/// Extract the caller's actor_role from the request's _meta field.
/// Rejects if _meta is missing or actor_role is not "root_lead" or "lead".
fn extract_and_check_root_lead(meta: &Option<CallerMeta>) -> Result<(), ErrorData> {
    let Some(m) = meta else {
        return Err(ErrorData::invalid_request(
            String::from("spawn_sublead requires caller identity (missing _meta)"),
            None,
        ));
    };

    if m.actor_role != "root_lead" && m.actor_role != "lead" {
        return Err(ErrorData::invalid_request(
            format!(
                "spawn_sublead is only available to the root lead (got role: {}; depth-2 invariant: workers and sub-leads cannot spawn sub-leads)",
                m.actor_role
            ),
            None,
        ));
    }

    Ok(())
}

/// Serialize a value to `CallToolResult::structured(json)`. Used for the
/// structured JSON payloads our tools return. Serialization failures are
/// reported as internal errors.
fn to_structured_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let v = serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialize: {e}"), None))?;
    Ok(CallToolResult::structured(v))
}

fn shared_store_err(e: &crate::shared_store::StoreError) -> ErrorData {
    use crate::shared_store::StoreError;
    let (code, msg, extra) = match e {
        StoreError::InvalidArg(m) => ("invalid_arg", m.as_str(), None),
        StoreError::Forbidden(m) => ("forbidden", m.as_str(), None),
        StoreError::Conflict => ("conflict", "conflict", None),
        StoreError::Timeout => ("timeout", "timeout", None),
        StoreError::LimitExceeded { which } => (
            "store_limit_exceeded",
            "store limit exceeded",
            Some(serde_json::json!({"which": which})),
        ),
        StoreError::Shutdown => ("store_shutdown", "store shutdown", None),
    };
    let mut data = serde_json::json!({"code": code});
    if let (Some(serde_json::Value::Object(inner)), Some(obj)) = (extra, data.as_object_mut()) {
        obj.extend(inner);
    }
    ErrorData::invalid_request(msg.to_string(), Some(data))
}

impl McpServer {
    /// Start serving on the given socket path. Binds to the unix socket,
    /// spawns an accept loop in a dedicated tokio task, returns a handle.
    ///
    /// Each accepted connection gets its own rmcp `ServiceExt::serve` session
    /// backed by a cloned `PitbossHandler`. The shared `DispatchState` is held
    /// behind `Arc` so all sessions see the same run.
    pub async fn start(socket_path: PathBuf, state: Arc<DispatchState>) -> Result<Self> {
        // If the socket file already exists (stale), remove it.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let handler = PitbossHandler::new(state);

        let tracker = TaskTracker::new();
        let cancel = CancellationToken::new();

        let tracker_outer = tracker.clone();
        let cancel_outer = cancel.clone();

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    _ = cancel_outer.cancelled() => break,
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, _addr)) => {
                                // `for_connection` gives this session its own
                                // `connection_actor` slot; cloning the Arc lets
                                // the cleanup branch below read it after `serve`
                                // returns without racing the moved handler.
                                let h = handler.for_connection();
                                let actor_slot = h.connection_actor_handle();
                                let store_for_cleanup = h.state.shared_store.clone();
                                let cancel_inner = cancel_outer.clone();
                                // Track the spawned session task so Drop can signal cancellation
                                // to per-connection tasks without waiting for MCP session timeouts.
                                tracker_outer.spawn(async move {
                                    tokio::select! {
                                        _ = cancel_inner.cancelled() => {}
                                        _ = async {
                                            match h.serve(stream).await {
                                                Ok(running) => {
                                                    if let Err(e) = running.waiting().await {
                                                        tracing::debug!("mcp session join error: {e}");
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::debug!("mcp session init error: {e}");
                                                }
                                            }
                                        } => {}
                                    }
                                    // Connection-drop cleanup: release every
                                    // lease held by this session's actor.
                                    // Until this hook existed, dropped bridges
                                    // left leases held until TTL expiry —
                                    // fine for short TTLs, but blocked other
                                    // workers on long-held leases when a
                                    // worker crashed.
                                    let actor = actor_slot.lock().await.clone();
                                    if let Some(id) = actor {
                                        store_for_cleanup.release_all_for_actor(&id).await;
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::debug!("mcp accept error: {e}");
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            socket_path,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
            tracker,
            cancel,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Signal shutdown to the accept loop.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Signal cancel to all per-connection tasks; they exit their select!
        // arms immediately rather than waiting for MCP session close / timeout.
        self.cancel.cancel();
        self.tracker.close();
        if let Some(h) = self.join_handle.take() {
            h.abort();
        }
        // Note: we can't `.await` tracker.wait() from a sync Drop. The
        // CancellationToken fires above let per-connection tasks exit quickly
        // without us blocking here. If a future async shutdown() method is
        // added, that would be the place to await the tracker.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use uuid::Uuid;

    // Serializes tests that mutate XDG_RUNTIME_DIR, since env vars are
    // process-global and cargo runs tests in parallel by default.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn socket_path_uses_xdg_runtime_dir_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, Path::new("/tmp"));
        assert!(p.starts_with(dir.path()));
        assert!(p.to_string_lossy().ends_with(".sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn socket_path_falls_back_to_run_dir_when_xdg_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XDG_RUNTIME_DIR");
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, dir.path());
        assert!(p.starts_with(dir.path()));
    }

    #[tokio::test]
    async fn server_starts_and_accepts_connection() {
        use crate::dispatch::state::{ApprovalPolicy, DispatchState};
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;

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
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ));

        let sock = dir.path().join("test.sock");
        let server = McpServer::start(sock.clone(), state).await.unwrap();
        assert!(sock.exists(), "socket file should exist after start");
        assert_eq!(server.socket_path(), sock.as_path());

        // Connect a raw unix stream to verify the server is listening.
        let stream = tokio::net::UnixStream::connect(&sock).await;
        assert!(stream.is_ok(), "server should accept connections");

        drop(server);
        // Socket is cleaned up on drop.
    }

    #[tokio::test]
    async fn server_drops_cleanly_even_with_active_connection() {
        use crate::dispatch::state::{ApprovalPolicy, DispatchState};
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;
        use tokio::time::Duration;

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
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ));

        let sock = dir.path().join("drop-test.sock");
        let server = McpServer::start(sock.clone(), state).await.unwrap();

        // Open a raw connection and hold it; the accept task will spawn a
        // tracked per-connection task to serve it.
        let _stream = tokio::net::UnixStream::connect(&sock).await.unwrap();

        // Give the server a moment to accept and spawn the session task so the
        // tracker is non-empty before we drop.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop the server while the connection is still open. Should complete
        // near-instantly via the cancellation token, not wait for MCP session
        // timeout (which can be up to an hour for wait_for_worker).
        let dropped_at = std::time::Instant::now();
        drop(server);
        let elapsed = dropped_at.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "Drop took too long: {:?}",
            elapsed
        );
        assert!(!sock.exists(), "socket file should be removed on drop");
    }

    // MCP spec requires CallToolResult.structuredContent to be a record/object.
    // Claude Code's MCP client validates it and rejects arrays / nulls with
    // `{"code":"invalid_type","message":"expected record, received array|null"}`.
    // These tests pin the wrapper shape for tools that return Option<_> / Vec<_>
    // so the shape can't regress silently.
    //
    // Earlier dogfood runs (2026-04-18) showed ~32 tool failures across four
    // runs, all rooted in this bug. Regression guard.

    #[test]
    fn kv_get_wraps_missing_entry_as_object_not_null() {
        let none: Option<crate::shared_store::Entry> = None;
        let result = to_structured_result(&serde_json::json!({ "entry": none })).unwrap();
        let v = result
            .structured_content
            .expect("structured content present");
        assert!(
            v.is_object(),
            "structuredContent must be a record, got {v:?}"
        );
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("entry"), "missing `entry` key: {obj:?}");
        assert!(
            obj["entry"].is_null(),
            "missing key should serialize as null INSIDE the record"
        );
    }

    #[test]
    fn kv_list_wraps_empty_result_as_object_not_array() {
        let empty: Vec<crate::shared_store::ListMetadata> = Vec::new();
        let result = to_structured_result(&serde_json::json!({ "entries": empty })).unwrap();
        let v = result
            .structured_content
            .expect("structured content present");
        assert!(
            v.is_object(),
            "structuredContent must be a record, got {v:?}"
        );
        assert!(v["entries"].is_array(), "entries should be an array");
    }

    #[test]
    fn list_workers_wraps_empty_result_as_object_not_array() {
        let empty: Vec<crate::mcp::tools::WorkerSummary> = Vec::new();
        let result = to_structured_result(&serde_json::json!({ "workers": empty })).unwrap();
        let v = result
            .structured_content
            .expect("structured content present");
        assert!(
            v.is_object(),
            "structuredContent must be a record, got {v:?}"
        );
        assert!(v["workers"].is_array(), "workers should be an array");
    }
}
