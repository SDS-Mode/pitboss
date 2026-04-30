//! MCP tool surface exposed to the lead's claude session: argument and
//! result types, cross-handler helpers, and `pub use` re-exports of the
//! per-feature handler submodules.
//!
//! Per-feature submodules (split out in #151 L6):
//!   - [`spawn`] — `handle_spawn_worker` / `spawn_resume_worker` /
//!     budget reservation / `worker_spawn_args` argv builder /
//!     `initial_estimate_for`.
//!   - [`lifecycle`] — `list_workers` / `worker_status` /
//!     `cancel_worker` / `pause_worker` / `continue_worker` /
//!     `reprompt_worker`.
//!   - [`approval`] — `request_approval` / `propose_plan` /
//!     `permission_prompt` (and the Claude Code permission-gate types).
//!   - [`wait`] — `wait_for_worker` / `wait_for_actor` / `wait_for_any`.
//!
//! External callers continue to use `crate::mcp::tools::*`; the
//! re-exports below preserve the pre-split surface.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dispatch::layer::LayerState;
use crate::dispatch::state::{DispatchState, WorkerState};

mod approval;
mod lifecycle;
mod spawn;
mod wait;

#[cfg(test)]
mod tests;

pub use approval::{
    handle_permission_prompt, handle_propose_plan, handle_request_approval, PermissionPromptArgs,
    PermissionPromptResponse,
};
pub use lifecycle::{
    handle_cancel_worker, handle_continue_worker, handle_list_workers, handle_pause_worker,
    handle_reprompt_worker, handle_worker_status,
};
pub use spawn::{handle_spawn_worker, spawn_resume_worker, PITBOSS_WORKER_MCP_TOOLS};
pub use wait::{handle_wait_for_actor, handle_wait_for_any, handle_wait_for_worker};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct SpawnWorkerArgs {
    #[serde(default)]
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
    #[serde(default)]
    pub provider: Option<String>,
    /// Caller identity injected by mcp-bridge. Used to route the new worker
    /// into the caller's layer (sub-lead callers land in their sub-tree;
    /// root-lead callers land in root). Absent for v0.5 back-compat callers —
    /// treated as root-lead (unchanged behavior).
    #[serde(rename = "_meta", default, skip_serializing)]
    #[schemars(skip)]
    pub meta: Option<crate::shared_store::tools::MetaField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpawnWorkerResult {
    pub task_id: String,
    pub worktree_path: Option<String>,
}

/// Local JsonSchema mirror for `pitboss_core::parser::TokenUsage`.
///
/// `pitboss-core` does not depend on `schemars`, so we can't derive `JsonSchema`
/// on the upstream type without adding a new dep to a low-level crate. This
/// struct lives here purely to satisfy the schema derivation for `WorkerStatus`
/// via `#[schemars(with = "TokenUsageSchema")]` — the actual field is still
/// `pitboss_core::parser::TokenUsage` at the type level, and `Serialize` /
/// `Deserialize` are wire-compatible because the field layout matches.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
pub struct TokenUsageSchema {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    #[serde(default)]
    pub reasoning: Option<u64>,
}

// Compile-time structural parity guards between TokenUsageSchema and
// pitboss_core::parser::TokenUsage. If someone renames, adds, or removes
// a field on either struct, these `From` impls won't compile — the
// schema and the upstream type diverge loudly instead of silently.
//
// Previously these two structs could drift (the schema reported the
// wrong shape), because the `#[schemars(with = ...)]` attribute on
// WorkerStatus.partial_usage is a string reference checked only at
// schema-generation time, not at field-layout time.
impl From<pitboss_core::parser::TokenUsage> for TokenUsageSchema {
    fn from(u: pitboss_core::parser::TokenUsage) -> Self {
        let pitboss_core::parser::TokenUsage {
            input,
            output,
            cache_read,
            cache_creation,
            reasoning,
        } = u;
        Self {
            input,
            output,
            cache_read,
            cache_creation,
            reasoning,
        }
    }
}

impl From<TokenUsageSchema> for pitboss_core::parser::TokenUsage {
    fn from(s: TokenUsageSchema) -> Self {
        let TokenUsageSchema {
            input,
            output,
            cache_read,
            cache_creation,
            reasoning,
        } = s;
        Self {
            input,
            output,
            cache_read,
            cache_creation,
            reasoning,
        }
    }
}

// Size equality is not proof of field-shape equality (renames would
// still pass), but it's a cheap extra signal — breaks loudly if a new
// field lands on one side but not the other.
const _TOKEN_USAGE_SCHEMA_SIZE_CHECK: () = {
    assert!(
        std::mem::size_of::<TokenUsageSchema>()
            == std::mem::size_of::<pitboss_core::parser::TokenUsage>(),
        "TokenUsageSchema and pitboss_core::parser::TokenUsage must stay in sync"
    );
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerStatus {
    pub state: String,
    pub started_at: Option<String>,
    #[schemars(with = "TokenUsageSchema")]
    pub partial_usage: pitboss_core::parser::TokenUsage,
    pub last_text_preview: Option<String>,
    #[serde(default)]
    pub prompt_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkerSummary {
    pub task_id: String,
    pub state: String,
    pub prompt_preview: String,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CancelResult {
    pub ok: bool,
}

// ---- Tool arg wrappers (for tools that take primitive or multi-arg input) ----
//
// The rmcp tool macros use `Parameters<T>` where T: JsonSchema to deserialize
// arguments from an incoming JSON object. We define small wrapper structs for
// each tool whose args aren't already represented by one of the structs above.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskIdArgs {
    pub task_id: String,
}

/// Pause-mode selector.
///
/// - `Cancel` (default): terminates the claude subprocess, snapshots
///   its session_id. `continue_worker` re-spawns via `claude --resume`.
///   Works for arbitrarily long pauses; loses any in-flight state.
/// - `Freeze`: SIGSTOP's the process in place. `continue_worker` just
///   SIGCONT's. Zero state loss + instant resume, but Anthropic may
///   drop the HTTP session if the pause runs past their server-side
///   idle window — prefer for quick pauses (seconds to low minutes).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PauseMode {
    #[default]
    Cancel,
    Freeze,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PauseWorkerArgs {
    pub task_id: String,
    #[serde(default)]
    pub mode: PauseMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitForWorkerArgs {
    pub task_id: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitActorRequest {
    pub actor_id: String,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WaitForAnyArgs {
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContinueWorkerArgs {
    pub task_id: String,
    /// Optional prompt to send with --resume. Defaults to "continue".
    #[serde(default)]
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RepromptWorkerArgs {
    pub task_id: String,
    /// New prompt to send via `claude --resume`. Required — unlike
    /// `ContinueWorkerArgs::prompt`, reprompt semantically *is* a new
    /// prompt; defaulting to "continue" would conflate the operations.
    pub prompt: String,
}

/// Structured approval payload. One-line `summary` is still required
/// (it's what shows in the modal's title bar and in notification sinks);
/// every other field is optional. Leads that have non-trivial actions
/// to approve — deletions, multi-file edits, irreversible ops — should
/// populate the typed fields so reviewers can see plan, rationale, and
/// rollback at a glance instead of reading a paragraph.
///
/// Absent fields render as "—" or get elided entirely in the TUI.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default, PartialEq, Eq)]
pub struct ApprovalPlan {
    /// One-line headline. Required. Shown in the modal title, the
    /// notification payload, and the audit event.
    pub summary: String,
    /// Why the lead thinks this action should be taken. Multi-line OK.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Resources (files, databases, external APIs, GitHub PRs, etc.)
    /// that this action will read or modify. Rendered as a bulleted
    /// list so reviewers can skim.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    /// Known risks / failure modes. If non-empty the TUI highlights
    /// the list in the warning color so the reviewer sees it before
    /// approving.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    /// How to undo the action if something goes wrong. Reviewers
    /// should reject plans that can't answer this for irreversible
    /// operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RequestApprovalArgs {
    /// One-line summary of the action being approved. Required. For
    /// non-trivial approvals prefer the typed `plan` field below, which
    /// will supersede this for display but this stays as the audit
    /// headline.
    pub summary: String,
    /// Optional per-request timeout override. Falls back to lead_timeout_secs.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Typed plan. When set, the TUI renders structured fields
    /// (rationale / resources / risks / rollback) instead of just the
    /// summary blob. Leads should populate this for anything
    /// destructive or multi-step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<ApprovalPlan>,
    /// Optional tool name hint for policy matching. When provided, the
    /// policy matcher can evaluate `match.tool_name` rules against this
    /// value. Falls through to `None` matching when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Optional cost estimate (USD) hint for policy matching. When
    /// provided, the policy matcher can evaluate `match.cost_over` rules
    /// against this value. Falls through to `None` matching when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_estimate: Option<f64>,
    /// Caller identity injected by mcp-bridge. Used to build the correct
    /// actor_path for policy matching (sub-lead vs root-lead).
    #[serde(rename = "_meta", default, skip_serializing)]
    #[schemars(skip)]
    pub meta: Option<crate::shared_store::tools::MetaField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalToolResponse {
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Arguments for `propose_plan`: the lead submits a full execution plan
/// for operator pre-flight approval. Gated by `[run].require_plan_approval`.
/// When that flag is off, calling `propose_plan` is harmless — the plan
/// is approved via the usual modal/policy path, but `spawn_worker` never
/// checks the result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProposePlanArgs {
    /// The typed plan to review. `summary` is required; the rest
    /// (rationale / resources / risks / rollback) is optional but
    /// strongly recommended — the whole point of pre-flight approval is
    /// that the operator can evaluate *before* workers start.
    pub plan: ApprovalPlan,
    /// Optional per-request timeout override. Falls back to
    /// `lead_timeout_secs`.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Optional cost estimate (USD) hint for policy matching. When
    /// provided, the policy matcher can evaluate `match.cost_over`
    /// rules against this value — e.g. an operator can declare
    /// `cost_over = 5.0 → block` to escalate any plan whose total
    /// estimated cost exceeds $5. Falls through to `None` matching
    /// when omitted, matching the pre-#151-M5 behavior. (#151 M5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_estimate: Option<f64>,
    /// Caller identity injected by mcp-bridge. Used to build the correct
    /// actor_path for policy matching (sub-lead vs root-lead).
    #[serde(rename = "_meta", default, skip_serializing)]
    #[schemars(skip)]
    pub meta: Option<crate::shared_store::tools::MetaField>,
}

/// Look up a worker by task_id across the root layer AND every active
/// sub-lead layer. Returns the first matching `WorkerState`.
///
/// Workers spawned by a sub-lead are registered in that sub-lead's
/// `LayerState.workers`, not in root's. Before this helper existed the
/// v0.6 MCP handlers all read `state.root.workers` (= root via Deref), so a
/// sub-lead's own worker looked "unknown" to every downstream tool
/// (`wait_for_worker`, `wait_actor`, `worker_status`, `list_workers`).
/// Surfaced via the depth-2 smoke test: sub-lead calls `spawn_worker`,
/// gets a task_id, then `wait_actor` on that id immediately returns
/// `unknown actor_id: worker-...`.
pub(super) async fn find_worker_across_layers(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Option<WorkerState> {
    if let Some(w) = state.root.workers.read().await.get(task_id).cloned() {
        return Some(w);
    }
    let subleads = state.subleads.read().await;
    for layer in subleads.values() {
        if let Some(w) = layer.workers.read().await.get(task_id).cloned() {
            return Some(w);
        }
    }
    None
}

/// Resolve the `LayerState` that owns `task_id`. Used by mutating
/// handlers (cancel/pause/continue/reprompt) so they target the correct
/// layer's `workers` / `worker_cancels` / `worker_pids` /
/// `worker_counters` maps. Sub-lead-owned workers live in their layer,
/// NOT root — before this helper, every mutating handler hard-coded
/// `state.root.*` and silently failed for sub-tree workers (issue #146).
///
/// Strategy: prefer the O(1) `worker_layer_index` lookup; fall back to a
/// linear scan if the index hasn't been populated yet (race with
/// `spawn_worker` registration).
pub(super) async fn layer_for_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Option<Arc<LayerState>> {
    let layer_id = state.worker_layer_index.read().await.get(task_id).cloned();
    match layer_id {
        Some(None) => Some(state.root.clone()),
        Some(Some(sublead_id)) => state.subleads.read().await.get(&sublead_id).cloned(),
        None => {
            if state.root.workers.read().await.contains_key(task_id) {
                return Some(state.root.clone());
            }
            for layer in state.subleads.read().await.values() {
                if layer.workers.read().await.contains_key(task_id) {
                    return Some(layer.clone());
                }
            }
            None
        }
    }
}
