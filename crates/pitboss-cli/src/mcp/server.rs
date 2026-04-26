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
use rmcp::{tool, tool_router, ErrorData, ServerHandler};

use crate::dispatch::layer::LayerState;
use crate::dispatch::signals::cancel_actor_with_reason;
use crate::dispatch::state::{ActorIdentity, DispatchState};
use crate::mcp::tools::{
    handle_continue_worker, handle_list_workers, handle_pause_worker, handle_permission_prompt,
    handle_propose_plan, handle_reprompt_worker, handle_request_approval, handle_spawn_worker,
    handle_wait_for_actor, handle_wait_for_any, handle_wait_for_worker, handle_worker_status,
    ContinueWorkerArgs, PauseWorkerArgs, PermissionPromptArgs, ProposePlanArgs, RepromptWorkerArgs,
    RequestApprovalArgs, SpawnWorkerArgs, TaskIdArgs, WaitActorRequest, WaitForAnyArgs,
    WaitForWorkerArgs,
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
    /// Environment variables to pass to the sub-lead's claude subprocess.
    /// Merged on top of pitboss's own defaults (e.g. `CLAUDE_CODE_ENTRYPOINT
    /// = "sdk-ts"`); operator-set keys win over defaults. Use this when a
    /// specific sub-lead needs a different env than the root, e.g. an
    /// `ANTHROPIC_API_KEY` override or a worker-toolset-specific flag.
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    /// Tool list override for the sub-lead's `--allowedTools`. If empty, uses
    /// the standard sublead toolset. If non-empty, the listed tools are
    /// passed to claude verbatim; pitboss MCP tools (`mcp__pitboss__*`) are
    /// always included on top so the sub-lead can still spawn workers etc.
    #[serde(default)]
    tools: Vec<String>,
    /// When set, pass `--resume <id>` to the sub-lead's claude subprocess.
    /// The root lead discovers prior session IDs from `/resume/subleads` in
    /// the shared store after `pitboss resume` seeds it at startup. Callers
    /// that omit this field get a fresh sub-lead session (default behavior).
    #[serde(default)]
    resume_session_id: Option<String>,
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

/// Request for `cancel_worker` — extends Task-4.5 with an optional `reason`
/// field. Existing callers that omit `reason` continue to work unchanged
/// (the field is skipped if absent on the wire).
/// Accepts both `target` (new) and `task_id` (v0.5 wire compat) as field names.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct CancelWorkerRequest {
    /// The actor id (worker task_id or sub-lead id) to cancel.
    /// Accepts both `target` and `task_id` parameter names for wire compatibility.
    #[serde(alias = "task_id")]
    target: String,
    /// Optional corrective context. When supplied, delivered to the killed
    /// actor's parent lead as a synthetic `[SYSTEM]` reprompt so the lead
    /// can adjust its plan without a separate operator round-trip.
    #[serde(default)]
    reason: Option<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RunLeaseAcquireRequest {
    key: String,
    ttl_secs: u64,
    /// Caller identity injected by mcp-bridge (actor_id + actor_role).
    #[serde(rename = "_meta", default)]
    #[schemars(skip)]
    meta: Option<CallerMeta>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RunLeaseReleaseRequest {
    key: String,
    /// Caller identity injected by mcp-bridge (actor_id + actor_role).
    #[serde(rename = "_meta", default)]
    #[schemars(skip)]
    meta: Option<CallerMeta>,
}

/// Compute the socket path for a given run. Falls back to the run_dir if
/// $XDG_RUNTIME_DIR is unset or non-writable.
pub fn socket_path_for_run(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("pitboss");
        if std::fs::create_dir_all(&p).is_ok() {
            // XDG_RUNTIME_DIR itself is 0o700, but our subdirectory inherits
            // the process umask; lock it down so other local users cannot
            // observe the socket file's metadata.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
            }
            return p.join(format!("{}.sock", run_id));
        }
    }
    // Fallback: alongside the run artifacts.
    let p = run_dir.join(run_id.to_string());
    let _ = std::fs::create_dir_all(&p);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
    }
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
async fn resolve_layer_for_caller<'a>(
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
            // Use .read().await instead of try_read().ok() to ensure we wait
            // for the lock rather than silently falling back to root layer if
            // the lock is contended. worker_layer_index and subleads are
            // independent RwLocks, so we safely await here.
            let layer_opt = state.worker_layer_index.read().await.get(actor_id).cloned();
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
    /// Per-connection canonical identity, bound on the FIRST tools/call
    /// that carries a valid `_meta.token`. Once bound, all subsequent
    /// calls on this connection use this identity for authz — the wire
    /// `_meta.actor_id` / `_meta.actor_role` are rewritten to match
    /// before downstream handlers see them. Closes #145.
    connection_identity: Arc<tokio::sync::Mutex<Option<ActorIdentity>>>,
}

impl PitbossHandler {
    pub fn new(state: Arc<DispatchState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
            connection_actor: Arc::new(tokio::sync::Mutex::new(None)),
            connection_identity: Arc::new(tokio::sync::Mutex::new(None)),
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
            connection_identity: Arc::new(tokio::sync::Mutex::new(None)),
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

    /// Validate the request's `_meta.token` against the dispatcher's
    /// token table and bind connection identity on first sight.
    /// Subsequent calls on the same connection skip token re-validation
    /// (the connection-bound identity wins).
    ///
    /// On every call, `_meta.actor_id` / `_meta.actor_role` are
    /// rewritten in-place to the bound canonical identity so downstream
    /// handlers cannot be tricked by wire-forged values. This is the
    /// core of issue #145.
    ///
    /// Errors:
    /// - `_meta` missing entirely: returns `Ok(())` (per-tool MetaField
    ///   handlers will reject if they require it; read-only tools that
    ///   accept `Option<MetaField>` continue to work without identity).
    /// - `_meta.token` present but unknown / unbindable: returns an
    ///   invalid-request error (the connection has presented a token
    ///   we did not mint).
    /// - `_meta.token` missing AND no identity already bound: returns
    ///   an invalid-request error if the wire claims an actor_id (a
    ///   weak forgery — likely a direct socket connection that bypasses
    ///   the bridge). Calls without any `_meta` at all are still
    ///   rejected by per-tool handlers that require it.
    async fn authenticate_and_rebind(
        &self,
        request: &mut rmcp::model::CallToolRequestParam,
    ) -> Result<(), ErrorData> {
        // Reach into params.arguments._meta. If the caller didn't supply
        // arguments at all, there's nothing to authenticate — let the
        // per-tool handler decide how to react.
        let Some(args) = request.arguments.as_mut() else {
            return Ok(());
        };
        // Pull out _meta as a mutable JSON object slot.
        let Some(meta_val) = args.get_mut("_meta") else {
            // No _meta on the wire. Per-tool handlers that require
            // identity will reject; bail-out tools (read-only KV) accept.
            return Ok(());
        };
        let Some(meta_obj) = meta_val.as_object_mut() else {
            return Err(ErrorData::invalid_request(
                "_meta must be an object".to_string(),
                None,
            ));
        };

        // Try connection-bound identity first — once a connection is
        // bound, every later call inherits the same canonical identity
        // regardless of what the wire claims.
        let mut bound = self.connection_identity.lock().await;
        if let Some(id) = bound.as_ref() {
            // Rewrite the wire `_meta` to the bound identity. Strip the
            // `token` field — handlers don't need it and we don't want
            // it leaking into logs.
            meta_obj.insert(
                "actor_id".into(),
                serde_json::Value::String(id.actor_id.clone()),
            );
            meta_obj.insert(
                "actor_role".into(),
                serde_json::Value::String(id.actor_role.clone()),
            );
            meta_obj.remove("token");
            return Ok(());
        }

        // No bound identity yet — try to extract and validate the token.
        let token_str = meta_obj
            .get("token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(token) = token_str {
            match self.state.lookup_token(&token).await {
                Some(identity) => {
                    // Bind for the rest of this connection's lifetime.
                    let canonical = identity.clone();
                    *bound = Some(identity);
                    // Rewrite the wire `_meta` in-place with the
                    // validated identity, and strip the token.
                    meta_obj.insert(
                        "actor_id".into(),
                        serde_json::Value::String(canonical.actor_id),
                    );
                    meta_obj.insert(
                        "actor_role".into(),
                        serde_json::Value::String(canonical.actor_role),
                    );
                    meta_obj.remove("token");
                    return Ok(());
                }
                None => {
                    // Token presented but unknown — reject hard. This
                    // catches both forged tokens and stale tokens from
                    // a previous run.
                    return Err(ErrorData::invalid_request(
                        "invalid actor token in _meta.token; reconnect via pitboss mcp-bridge"
                            .to_string(),
                        None,
                    ));
                }
            }
        }

        // No bound identity AND no token. The wire might still carry an
        // unauthenticated actor_id — accept it for backward compatibility
        // with code paths that don't have a token (none in production,
        // but tests construct DispatchState directly without minting).
        // The defense-in-depth `extract_bound_identity` below treats this
        // as "no bound identity" so downstream authz checks (Phase 3)
        // still fire correctly.
        Ok(())
    }

    /// Return the canonical (actor_id, actor_role) for this connection,
    /// or `None` if no token has been bound yet. Used by Phase-3 authz
    /// checks (`authorize_target`) to identify the caller without
    /// trusting the wire `_meta`.
    #[allow(dead_code)]
    pub(crate) async fn bound_identity(&self) -> Option<ActorIdentity> {
        self.connection_identity.lock().await.clone()
    }

    /// Issue #144 — gate a mutating call against `authorize_target`.
    ///
    /// Resolves the caller from connection-bound identity (Phase 2) when
    /// available; otherwise treats the connection as the root lead — the
    /// only legitimate path to a missing bound identity is a legacy
    /// caller that predates token issuance, which only the operator's
    /// own root-lead spawn produces (sub-leads and workers always carry
    /// tokens minted at spawn time).
    async fn authorize(&self, target_id: &str) -> Result<(), ErrorData> {
        let bound = self.connection_identity.lock().await.clone();
        let (caller_id, caller_role) = match bound {
            Some(id) => (id.actor_id, id.actor_role),
            None => (self.state.root.lead_id.clone(), "root_lead".to_string()),
        };
        authorize_target(&self.state, &caller_id, &caller_role, target_id).await
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

        // Manifest guard: spawn_sublead is only available when allow_subleads=true.
        // This is the secondary line of defense; the primary gate is list_tools
        // filtering (spawn_sublead is absent from the toolset when allow_subleads=false).
        let allow_subleads = self
            .state
            .root
            .manifest
            .lead
            .as_ref()
            .is_some_and(|l| l.allow_subleads);
        if !allow_subleads {
            return Err(ErrorData::invalid_request(
                String::from(
                    "spawn_sublead requires allow_subleads=true in the manifest [lead] block",
                ),
                None,
            ));
        }

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
            env: req.env,
            tools: req.tools,
            resume_session_id: req.resume_session_id,
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

    #[tool(
        description = "Cancel an actor (worker or sub-lead) by id. When `reason` is supplied, it is delivered to the actor's parent lead as a synthetic [SYSTEM] reprompt so the lead can adjust its plan without a separate operator round-trip. Existing callers that omit `reason` behave identically to the pre-4.5 cancel path."
    )]
    async fn cancel_worker(
        &self,
        Parameters(req): Parameters<CancelWorkerRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        // #144: only the root lead, the target's parent sublead, or the
        // target itself may cancel. Without this, any authenticated
        // worker could cancel the root lead and abort the run.
        self.authorize(&req.target).await?;
        // Fast-path: no reason supplied — use the existing single-layer path
        // for root-layer workers (preserves v0.5 exact behavior for that case).
        // If the target is in a sub-tree, cancel_actor_with_reason handles it.
        match cancel_actor_with_reason(&self.state, &req.target, req.reason).await {
            Ok(()) => to_structured_result(&crate::mcp::tools::CancelResult { ok: true }),
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
        self.authorize(&args.task_id).await?;
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
        self.authorize(&args.task_id).await?;
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
        self.authorize(&args.task_id).await?;
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
                resolve_layer_for_caller(&self.state, &m.actor_id, m.actor_role, &subleads).await?;
            (layer, m.actor_id.clone())
        } else {
            (&self.state.root, String::new())
        };

        // Strict peer-visibility check: /peer/<X>/* is readable only by X or
        // the layer's lead. Applied before the store lookup (fast-reject).
        //
        // An empty caller_id (no `_meta`) can only come from a direct socket
        // connection that bypassed the bridge — legitimate agents always
        // carry `_meta`. Reject /peer/* access from such connections rather
        // than falling through to a root-layer read; otherwise any local
        // process with socket access could enumerate peer slots.
        if let Some(target_id) = parse_peer_path(&args.path) {
            if caller_id.is_empty() {
                return Err(shared_store_err(
                    &crate::shared_store::StoreError::Forbidden(format!(
                        "strict peer visibility: /peer/{target_id}/* requires caller \
                         identity (_meta); rejecting anonymous read"
                    )),
                ));
            }
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
        )
        .await?;
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
        )
        .await?;
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
                resolve_layer_for_caller(&self.state, &m.actor_id, m.actor_role, &subleads).await?;
            (layer, m.actor_id.clone())
        } else {
            (&self.state.root, String::new())
        };

        // Strict peer-visibility check for /peer/<X>/* globs.
        // Only exact /peer/<id>/... prefix patterns are checked — a broad
        // glob like /peer/** is rejected unless the caller is the layer lead.
        // Empty caller_id (no `_meta`) is rejected outright — legitimate
        // agents always carry `_meta`, so falling through to a root-layer
        // read would let a raw-socket client bypass peer isolation.
        if let Some(target_id) = parse_peer_path(&args.glob) {
            if caller_id.is_empty() {
                return Err(shared_store_err(
                    &crate::shared_store::StoreError::Forbidden(format!(
                        "strict peer visibility: /peer/{target_id}/* requires caller \
                         identity (_meta); rejecting anonymous list"
                    )),
                ));
            }
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

        match crate::shared_store::tools::handle_kv_list(&layer.shared_store, args).await {
            // Wrap ListResult in an object — MCP spec requires
            // structuredContent to be a record. `truncated` + `total_matched`
            // are surfaced so callers can detect that the result is
            // partial (rather than guessing from `entries.len()`).
            Ok(r) => to_structured_result(&serde_json::json!({
                "entries": r.entries,
                "truncated": r.truncated,
                "total_matched": r.total_matched,
            })),
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
        let (layer, caller_id) = if let Some(m) = &args.meta {
            let layer =
                resolve_layer_for_caller(&self.state, &m.actor_id, m.actor_role, &subleads).await?;
            (layer, m.actor_id.clone())
        } else {
            (&self.state.root, String::new())
        };

        // Strict peer-visibility check: /peer/<X>/* is waiterable only by X or
        // the layer's lead. Applied before the store wait (fast-reject).
        // Empty caller_id (no `_meta`) is rejected outright — otherwise a
        // raw-socket client could block on another actor's slot
        // indefinitely, acting as a side-channel oracle on slot writes.
        if let Some(target_id) = parse_peer_path(&args.path) {
            if caller_id.is_empty() {
                return Err(shared_store_err(
                    &crate::shared_store::StoreError::Forbidden(format!(
                        "strict peer visibility: /peer/{target_id}/* requires caller \
                         identity (_meta); rejecting anonymous wait"
                    )),
                ));
            }
            if !can_read_peer_slot(layer, &caller_id, target_id) {
                return Err(shared_store_err(
                    &crate::shared_store::StoreError::Forbidden(format!(
                        "strict peer visibility: {caller_id} cannot wait on /peer/{target_id}/*; \
                         only {target_id} itself or the layer lead ({}) may wait on this slot",
                        layer.lead_id,
                    )),
                ));
            }
        }

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
        )
        .await?;
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
        )
        .await?;
        match crate::shared_store::tools::handle_lease_release(&layer.shared_store, args).await {
            Ok(()) => Ok(CallToolResult::structured(serde_json::json!({"ok": true}))),
            Err(e) => Err(shared_store_err(&e)),
        }
    }

    #[tool(
        name = "run_lease_acquire",
        description = "Acquire a run-global lease for cross-sub-tree resource coordination. Use for resources accessed from multiple sub-trees (e.g., operator's filesystem). Use per-layer /leases/* for sub-tree-internal coordination."
    )]
    async fn run_lease_acquire(
        &self,
        Parameters(req): Parameters<RunLeaseAcquireRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor_id = extract_actor_id(&req.meta)?;
        // Record actor for connection-drop cleanup (server.rs:934).
        // Without this, a connection whose only traffic is run_lease_acquire
        // leaves `connection_actor` unset and the run-global lease leaks
        // until TTL when the socket drops.
        self.note_actor(&actor_id).await;
        let ttl = std::time::Duration::from_secs(req.ttl_secs);
        match self
            .state
            .run_leases
            .try_acquire(&req.key, &actor_id, ttl)
            .await
        {
            Ok(handle) => to_structured_result(&serde_json::json!({
                "acquired": true,
                "key": handle.key,
                "holder": handle.holder
            })),
            Err(current_holder) => Err(ErrorData::invalid_request(
                format!("lease '{}' currently held by {}", req.key, current_holder),
                None,
            )),
        }
    }

    #[tool(
        name = "run_lease_release",
        description = "Release a run-global lease previously acquired via run_lease_acquire. No-op if not held by caller."
    )]
    async fn run_lease_release(
        &self,
        Parameters(req): Parameters<RunLeaseReleaseRequest>,
    ) -> Result<CallToolResult, ErrorData> {
        let actor_id = extract_actor_id(&req.meta)?;
        self.note_actor(&actor_id).await;
        let released = self.state.run_leases.release(&req.key, &actor_id).await;
        to_structured_result(&serde_json::json!({
            "released": released
        }))
    }

    /// Path B permission gate. Claude calls this when it encounters a
    /// tool-use that requires operator approval. Only visible (list_tools)
    /// and callable when `[lead] permission_routing = "path_b"` is set.
    #[tool(
        name = "permission_prompt",
        description = "Route a Claude tool-permission request to pitboss's approval queue. Returns {decision, behavior}."
    )]
    async fn permission_prompt(
        &self,
        Parameters(args): Parameters<PermissionPromptArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_permission_prompt(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }
}

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

    /// Delegate all tool calls to the rmcp tool router.
    /// Equivalent to what `#[tool_handler]` would generate automatically, but
    /// written manually so we can:
    ///   - Validate the per-actor auth token in `_meta.token` and bind the
    ///     connection's canonical identity (issue #145).
    ///   - Rewrite `_meta.actor_id` / `_meta.actor_role` to the bound values
    ///     so downstream handlers see the canonical identity rather than
    ///     whatever the wire claimed (defends against forged direct-socket
    ///     connections).
    ///   - Filter `list_tools` based on manifest capabilities (below).
    async fn call_tool(
        &self,
        mut request: rmcp::model::CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        // Phase 2 (#145): authenticate the call against the token table
        // and rewrite `_meta.{actor_id,actor_role}` to the bound canonical
        // identity. After this block, downstream handlers can trust the
        // wire `_meta` because it has been replaced with the validated
        // identity.
        self.authenticate_and_rebind(&mut request).await?;

        let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    /// Return the tool list, conditionally excluding tools based on manifest
    /// configuration:
    /// - `spawn_sublead`: hidden unless `allow_subleads = true` (v0.6 depth-2 gate)
    /// - `permission_prompt`: hidden unless `permission_routing = "path_b"` (v0.8)
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParam>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        use crate::manifest::schema::PermissionRouting;
        let lead = self.state.root.manifest.lead.as_ref();
        let allow_subleads = lead.is_some_and(|l| l.allow_subleads);
        let path_b = lead.is_some_and(|l| l.permission_routing == PermissionRouting::PathB);

        let tools: Vec<rmcp::model::Tool> = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| allow_subleads || t.name != "spawn_sublead")
            .filter(|t| path_b || t.name != "permission_prompt")
            .collect();

        Ok(rmcp::model::ListToolsResult::with_all_items(tools))
    }
}

/// Issue #144 — verify the caller (resolved from connection-bound
/// identity in Phase 2 OR from the wire `_meta` for legacy callers
/// without a token) is permitted to act on `target_id`.
///
/// Rules:
/// - Self-target is always allowed (`caller_id == target_id`).
/// - `root_lead` / `lead` may target anything in the run (root scope).
/// - `sublead` may target itself OR any worker registered to its own
///   sub-tree (resolved via `state.worker_layer_index`).
/// - `worker` may target only itself.
///
/// Apply this at the top of every mutating handler that accepts a
/// caller-supplied target id (cancel/pause/continue/reprompt + the
/// `request_approval` blocks_target plumbing). Without this check, any
/// authenticated worker could `cancel_worker(root_lead_id)` and abort
/// the entire run.
pub(crate) async fn authorize_target(
    state: &Arc<DispatchState>,
    caller_id: &str,
    caller_role: &str,
    target_id: &str,
) -> Result<(), ErrorData> {
    if caller_id == target_id {
        return Ok(());
    }
    match caller_role {
        "root_lead" | "lead" => Ok(()),
        "sublead" => {
            // Sublead may act on any worker in its own sub-tree. Look
            // the target up in `worker_layer_index`: a value of
            // `Some(caller_id)` means the worker belongs to THIS
            // sublead's layer.
            let idx = state.worker_layer_index.read().await;
            match idx.get(target_id) {
                Some(Some(owner_sublead_id)) if owner_sublead_id == caller_id => Ok(()),
                _ => Err(ErrorData::invalid_request(
                    format!(
                        "authz: sublead {caller_id} may only target workers in its own \
                         sub-tree (target {target_id} is not registered to this sublead)"
                    ),
                    None,
                )),
            }
        }
        "worker" => Err(ErrorData::invalid_request(
            format!("authz: worker {caller_id} may only target itself (cannot act on {target_id})"),
            None,
        )),
        other => Err(ErrorData::invalid_request(
            format!("authz: unknown actor role {other:?}"),
            None,
        )),
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

/// Extract the caller's actor_id from the request's _meta field.
/// Available to all actors (root lead, sub-leads, workers).
fn extract_actor_id(meta: &Option<CallerMeta>) -> Result<String, ErrorData> {
    let Some(m) = meta else {
        return Err(ErrorData::invalid_request(
            String::from("caller identity required (missing _meta)"),
            None,
        ));
    };
    Ok(m.actor_id.clone())
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
        // Explicit hardening — inherited umask (e.g. 0022) would leave the
        // socket world-readable, letting any local user connect and inject
        // `actor_role: root_lead` in _meta to call spawn_worker / kv_set.
        // 0o600 restricts to the running user.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        }
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
                                let store_for_cleanup = h.state.root.shared_store.clone();
                                let run_leases_for_cleanup = h.state.run_leases.clone();
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
                                    // Connection-drop cleanup: release every lease
                                    // (per-layer and run-global) held by this
                                    // session's actor. Until this hook existed,
                                    // dropped bridges left leases held until TTL
                                    // expiry — fine for short TTLs, but blocked
                                    // other workers on long-held leases when a
                                    // worker crashed.
                                    let actor = actor_slot.lock().await.clone();
                                    if let Some(id) = actor {
                                        store_for_cleanup.release_all_for_actor(&id).await;
                                        run_leases_for_cleanup.release_all_held_by(&id).await;
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

    // ── Issue #145 (per-actor token binding) ───────────────────────────
    //
    // Helper that builds a minimal DispatchState + McpServer pair so each
    // token-binding test can run in isolation against a real socket.

    async fn token_test_state() -> (
        std::path::PathBuf,
        Arc<DispatchState>,
        super::McpServer,
        tempfile::TempDir,
    ) {
        use crate::dispatch::state::DispatchState;
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
            crate::dispatch::state::ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ));
        let sock = dir.path().join("token.sock");
        let server = super::McpServer::start(sock.clone(), state.clone())
            .await
            .unwrap();
        (sock, state, server, dir)
    }

    /// Issue #145: a connection that supplies an unknown / forged token
    /// must be rejected on the very first tools/call. The bridge always
    /// supplies a token the dispatcher minted, so any token outside the
    /// `actor_tokens` table is by definition forged.
    #[tokio::test]
    async fn invalid_token_is_rejected_on_tools_call() {
        let (sock, _state, _server, _dir) = token_test_state().await;
        let mut client = fake_mcp_client::FakeMcpClient::connect_with_token(
            &sock,
            "worker-1",
            "worker",
            "definitely-not-a-real-token",
        )
        .await
        .unwrap();
        // Any tool call should be rejected by the auth gate before the
        // tool router runs. `kv_get` is a benign read used as the probe.
        let err = client
            .call_tool("kv_get", serde_json::json!({"path": "/ref/anything"}))
            .await
            .unwrap_err();
        // FakeMcpClient wraps the underlying rmcp error; the message we
        // emit ("invalid actor token in _meta.token; ...") appears in
        // the chain of source errors, not in the top-level Display.
        let mut found = false;
        let mut cur: Option<&dyn std::error::Error> = Some(err.as_ref());
        while let Some(e) = cur {
            if e.to_string().contains("invalid actor token") {
                found = true;
                break;
            }
            cur = e.source();
        }
        assert!(
            found,
            "expected token-rejection somewhere in the error chain; full chain: {err:#}"
        );
    }

    /// Issue #145: a connection that presents a valid token gets bound
    /// to the canonical (actor_id, actor_role) the token resolves to.
    /// Even if the wire claims a DIFFERENT actor_id / actor_role, the
    /// server uses the bound identity.
    ///
    /// We probe this via `kv_set` to /peer/self/...: the path resolves
    /// against the caller's bound actor_id, so a successful write to
    /// `/peer/<bound>/x` proves the binding took priority over the wire.
    #[tokio::test]
    async fn valid_token_overrides_forged_wire_identity() {
        let (sock, state, _server, _dir) = token_test_state().await;
        // Mint a token bound to "worker-real" with role "worker".
        let token = state.mint_token("worker-real", "worker").await;

        // Connect claiming to be "root_lead" — the wire identity is a
        // lie. The server must ignore the wire and use the token's
        // bound identity.
        let mut client = fake_mcp_client::FakeMcpClient::connect_with_token(
            &sock,
            "root",      // forged
            "root_lead", // forged role
            &token,
        )
        .await
        .unwrap();

        // Use kv_set to /peer/self/audit — the path expands against the
        // caller's resolved identity. Authz allows a worker to write its
        // OWN /peer/self/* slot. If the bound identity is "worker-real",
        // this succeeds. If the server trusted the wire ("root_lead"),
        // the path expansion would land on /peer/root/audit (the lead's
        // slot), and the authz layer treats lead writes to /peer/self
        // differently — but more importantly, this proves the rewrite is
        // happening: kv_set's MetaField deserializer reads the wire
        // _meta.actor_id, and after the rewrite that's "worker-real".
        let ok = client
            .call_tool(
                "kv_set",
                serde_json::json!({
                    "path": "/peer/self/audit",
                    "value": b"hello".to_vec(),
                }),
            )
            .await;
        assert!(
            ok.is_ok(),
            "kv_set with valid token should succeed; got {ok:?}"
        );
        // Confirm the write landed under the BOUND identity, not the
        // forged one. Read back via the kv_get on the bound path.
        let got = client
            .call_tool(
                "kv_get",
                serde_json::json!({"path": "/peer/worker-real/audit"}),
            )
            .await
            .unwrap();
        assert!(
            got["entry"].is_object(),
            "expected /peer/worker-real/audit to exist after bound write, got: {got:?}"
        );
    }

    // ── Issue #144 (authorize_target) ──────────────────────────────────

    /// A worker authenticated as worker-A must NOT be able to cancel
    /// the root lead. Pre-fix: cancel_worker accepted any target_id
    /// from any caller. Now `authorize_target` rejects.
    #[tokio::test]
    async fn worker_cannot_cancel_root_lead() {
        let (sock, state, _server, _dir) = token_test_state().await;
        let token = state.mint_token("worker-A", "worker").await;
        let mut client =
            fake_mcp_client::FakeMcpClient::connect_with_token(&sock, "worker-A", "worker", &token)
                .await
                .unwrap();
        // Try to cancel the root lead — `state.root.lead_id` is "lead".
        let err = client
            .call_tool("cancel_worker", serde_json::json!({"target": "lead"}))
            .await
            .unwrap_err();
        let mut found = false;
        let mut cur: Option<&dyn std::error::Error> = Some(err.as_ref());
        while let Some(e) = cur {
            if e.to_string().contains("authz: worker") {
                found = true;
                break;
            }
            cur = e.source();
        }
        assert!(
            found,
            "expected authz rejection for worker→lead cancel; chain: {err:#}"
        );
    }

    /// Worker A must NOT be able to cancel worker B (sibling).
    #[tokio::test]
    async fn worker_cannot_cancel_sibling_worker() {
        let (sock, state, _server, _dir) = token_test_state().await;
        let token = state.mint_token("worker-A", "worker").await;

        // Register worker-B in the index so authorize_target sees it as
        // a real worker (not strictly required — authz fires on role
        // before checking target existence — but matches realistic
        // conditions).
        state
            .worker_layer_index
            .write()
            .await
            .insert("worker-B".into(), None);

        let mut client =
            fake_mcp_client::FakeMcpClient::connect_with_token(&sock, "worker-A", "worker", &token)
                .await
                .unwrap();
        let err = client
            .call_tool("pause_worker", serde_json::json!({"task_id": "worker-B"}))
            .await
            .unwrap_err();
        let mut found = false;
        let mut cur: Option<&dyn std::error::Error> = Some(err.as_ref());
        while let Some(e) = cur {
            if e.to_string().contains("authz: worker") {
                found = true;
                break;
            }
            cur = e.source();
        }
        assert!(
            found,
            "expected authz rejection for sibling-worker pause; chain: {err:#}"
        );
    }

    /// A sublead must NOT be able to act on a worker registered to
    /// another sublead's sub-tree.
    #[tokio::test]
    async fn sublead_cannot_pause_other_subleads_worker() {
        let (sock, state, _server, _dir) = token_test_state().await;
        let token = state.mint_token("sublead-A", "sublead").await;

        // Register worker-X under SUBLEAD-B's sub-tree.
        state
            .worker_layer_index
            .write()
            .await
            .insert("worker-X".into(), Some("sublead-B".to_string()));

        let mut client = fake_mcp_client::FakeMcpClient::connect_with_token(
            &sock,
            "sublead-A",
            "sublead",
            &token,
        )
        .await
        .unwrap();
        let err = client
            .call_tool("pause_worker", serde_json::json!({"task_id": "worker-X"}))
            .await
            .unwrap_err();
        let mut found = false;
        let mut cur: Option<&dyn std::error::Error> = Some(err.as_ref());
        while let Some(e) = cur {
            if e.to_string().contains("authz: sublead") {
                found = true;
                break;
            }
            cur = e.source();
        }
        assert!(
            found,
            "expected authz rejection for sublead→other-sub-tree worker; chain: {err:#}"
        );
    }

    /// Direct (no-token) test of authorize_target behaviour. Documents
    /// the rule table; a defensive backstop if call-site refactors ever
    /// drop the gate.
    #[tokio::test]
    async fn authorize_target_rule_table() {
        use super::authorize_target;
        let (_sock, state, _server, _dir) = token_test_state().await;

        // Register worker-S under sublead-A.
        state
            .worker_layer_index
            .write()
            .await
            .insert("worker-S".into(), Some("sublead-A".into()));

        // Self-target always allowed.
        assert!(authorize_target(&state, "x", "worker", "x").await.is_ok());
        // Root lead may target anything.
        assert!(authorize_target(&state, "root", "root_lead", "anything")
            .await
            .is_ok());
        assert!(authorize_target(&state, "root", "lead", "anything")
            .await
            .is_ok());
        // Sublead targeting its own sub-tree worker is allowed.
        assert!(authorize_target(&state, "sublead-A", "sublead", "worker-S")
            .await
            .is_ok());
        // Sublead targeting another sublead's worker is rejected.
        assert!(authorize_target(&state, "sublead-B", "sublead", "worker-S")
            .await
            .is_err());
        // Worker cannot target another actor.
        assert!(authorize_target(&state, "worker-A", "worker", "worker-B")
            .await
            .is_err());
        // Unknown role rejected.
        assert!(authorize_target(&state, "x", "alien", "y").await.is_err());
    }
}
