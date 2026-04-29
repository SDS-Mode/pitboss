//! Approval-side MCP handlers: `request_approval` (per-action),
//! `propose_plan` (pre-flight plan gate, flips per-layer
//! `plan_approved`), and `permission_prompt` (Path-B Claude Code
//! permission gate). Houses the `PermissionPromptArgs` /
//! `PermissionPromptResponse` types alongside their handler.

use std::sync::Arc;

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::time::Duration;

use super::{ApprovalToolResponse, ProposePlanArgs, RequestApprovalArgs};
use crate::dispatch::layer::LayerState;
use crate::dispatch::state::DispatchState;

/// Build the caller's `(actor_id, ActorPath)` from the optional `_meta` field.
///
/// Falls back to the root-lead identity when `_meta` is absent, which is
/// correct for depth-1 runs and backward-compatible with callers that predate
/// the `_meta` injection.
///
/// Path construction:
/// - `Lead` (root lead, incl. `root_lead` alias): `[root_lead_id]`
/// - `Sublead` with id S: `[root_lead_id, S]`
/// - `Worker` with id W:
///   - root-layer worker (not in any sub-tree): `[root_lead_id, W]`
///   - sub-tree worker of sublead S: `[root_lead_id, S, W]`
async fn build_caller_identity(
    state: &Arc<DispatchState>,
    meta: Option<&crate::shared_store::tools::MetaField>,
) -> (String, crate::dispatch::actor::ActorPath) {
    use crate::dispatch::actor::ActorPath;
    use crate::shared_store::ActorRole;

    let root_lead_id = state.root.lead_id.as_str();

    let Some(m) = meta else {
        // No _meta → treat as root lead (backward-compatible).
        return (root_lead_id.to_owned(), ActorPath::new([root_lead_id]));
    };

    match m.actor_role {
        ActorRole::Lead => {
            // Root lead (or root_lead alias).
            (m.actor_id.clone(), ActorPath::new([root_lead_id]))
        }
        ActorRole::Sublead => {
            // Sub-lead S → path is [root_lead_id, S].
            (
                m.actor_id.clone(),
                ActorPath::new([root_lead_id, m.actor_id.as_str()]),
            )
        }
        ActorRole::Worker => {
            // Look up which sub-tree (if any) this worker belongs to.
            let layer_opt = state
                .worker_layer_index
                .read()
                .await
                .get(m.actor_id.as_str())
                .cloned();
            match layer_opt {
                // Root-layer worker: [root_lead_id, worker_id]
                None | Some(None) => (
                    m.actor_id.clone(),
                    ActorPath::new([root_lead_id, m.actor_id.as_str()]),
                ),
                // Sub-tree worker: [root_lead_id, sublead_id, worker_id]
                Some(Some(sublead_id)) => (
                    m.actor_id.clone(),
                    ActorPath::new([root_lead_id, sublead_id.as_str(), m.actor_id.as_str()]),
                ),
            }
        }
    }
}

pub async fn handle_request_approval(
    state: &Arc<DispatchState>,
    args: RequestApprovalArgs,
) -> Result<ApprovalToolResponse> {
    use crate::dispatch::state::PendingApproval;
    use crate::mcp::approval::{ApprovalCategory, ApprovalFallback};
    use crate::mcp::policy::ApprovalAction;

    // Determine the caller's identity from the _meta field injected by
    // mcp-bridge. Falls back to treating the caller as the root lead when
    // _meta is absent (backward-compatible with callers that omit it).
    let (caller_id, actor_path) = build_caller_identity(state, args.meta.as_ref()).await;

    // Build a PendingApproval for policy evaluation. actor_path is now
    // correctly set based on the actual caller role (root lead, sub-lead, or
    // worker), so per-sub-lead policy rules (e.g. actor = "root→S1") match.
    let pending = PendingApproval {
        id: uuid::Uuid::now_v7(),
        requesting_actor_id: caller_id.clone(),
        actor_path,
        category: ApprovalCategory::ToolUse,
        summary: args.summary.clone(),
        plan: args.plan.clone(),
        blocks: vec![caller_id.clone()],
        created_at: chrono::Utc::now(),
        ttl_secs: args
            .timeout_secs
            .or(state.root.manifest.lead_timeout_secs)
            .unwrap_or(3600),
        fallback: ApprovalFallback::AutoReject,
    };

    // Evaluate operator-declared policy before falling through to the legacy queue.
    {
        let matcher_guard = state.root.policy_matcher.lock().await;
        if let Some(matcher) = matcher_guard.as_ref() {
            match matcher.evaluate(&pending, args.tool_name.as_deref(), args.cost_estimate) {
                Some(ApprovalAction::AutoApprove) => {
                    tracing::info!(
                        actor = %pending.requesting_actor_id,
                        "auto-approved by policy"
                    );
                    state
                        .record_last_approval_response(&pending.requesting_actor_id, true, false)
                        .await;
                    return Ok(ApprovalToolResponse {
                        approved: true,
                        comment: Some("auto-approved by policy".into()),
                        edited_summary: None,
                        reason: None,
                    });
                }
                Some(ApprovalAction::AutoReject) => {
                    tracing::info!(
                        actor = %pending.requesting_actor_id,
                        "auto-rejected by policy"
                    );
                    state
                        .record_last_approval_response(&pending.requesting_actor_id, false, false)
                        .await;
                    return Ok(ApprovalToolResponse {
                        approved: false,
                        comment: Some("auto-rejected by policy".into()),
                        edited_summary: None,
                        reason: None,
                    });
                }
                Some(ApprovalAction::Block) | None => {
                    // fall through to operator queue
                }
            }
        }
    }

    let ttl_secs = args
        .timeout_secs
        .or(state.root.manifest.lead_timeout_secs)
        .unwrap_or(3600);
    let timeout = Duration::from_secs(ttl_secs);
    let caller_id_for_record = caller_id.clone();
    let bridge = crate::mcp::approval::ApprovalBridge::new(Arc::clone(state));
    match bridge
        .request(
            caller_id,
            args.summary,
            args.plan,
            crate::control::protocol::ApprovalKind::Action,
            timeout,
            Some(ttl_secs),
            Some(crate::mcp::approval::ApprovalFallback::AutoReject),
        )
        .await
    {
        Ok(resp) => {
            state
                .record_last_approval_response(&caller_id_for_record, resp.approved, resp.from_ttl)
                .await;
            Ok(ApprovalToolResponse {
                approved: resp.approved,
                comment: resp.comment,
                edited_summary: resp.edited_summary,
                reason: resp.reason,
            })
        }
        Err(e) => anyhow::bail!("approval failed: {e}"),
    }
}

/// Handle `propose_plan`: a lead submits a full execution plan for
/// pre-flight operator approval, distinct from `request_approval`'s
/// in-flight per-action gating. Flips the caller's **layer** `plan_approved`
/// to true on approval; leaves it false on rejection (lead can revise and
/// retry).
///
/// Plan approval is per-layer: the root lead's approval gates root-layer
/// worker spawns; each sub-lead's approval gates its own sub-tree's
/// worker spawns. Previously, every `propose_plan` acceptance flipped
/// `state.root.plan_approved`, so a sub-lead's approval silently
/// unblocked worker spawns for the root lead and every sibling sub-lead
/// — bypassing the root's own plan gate. `spawn_worker` now reads from
/// the target layer, and this handler writes to the caller's layer.
///
/// Workers cannot call propose_plan.
///
/// Returns the same `ApprovalToolResponse` shape as `request_approval`
/// so leads can share response-handling code — they just dispatch on
/// the tool name.
pub async fn handle_propose_plan(
    state: &Arc<DispatchState>,
    args: ProposePlanArgs,
) -> Result<ApprovalToolResponse> {
    use crate::dispatch::state::PendingApproval;
    use crate::mcp::approval::{ApprovalCategory, ApprovalFallback};
    use crate::mcp::policy::ApprovalAction;
    use crate::shared_store::ActorRole;

    // Resolve the caller's layer — `plan_approved` is stored there, not
    // on `state.root`, so a sub-lead's approval does not inadvertently
    // open the root lead's spawn gate.
    let caller_layer: Arc<LayerState> = match args.meta.as_ref() {
        None => Arc::clone(&state.root),
        Some(meta) => match meta.actor_role {
            ActorRole::Lead => Arc::clone(&state.root),
            ActorRole::Sublead => {
                let subleads = state.subleads.read().await;
                subleads
                    .get(meta.actor_id.as_str())
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("unknown sublead_id: {}", meta.actor_id))?
            }
            ActorRole::Worker => anyhow::bail!(
                "propose_plan is not available to workers; only leads \
                 and sub-leads may propose a plan"
            ),
        },
    };

    // Determine the caller's identity from the _meta field injected by
    // mcp-bridge. Falls back to treating the caller as the root lead when
    // _meta is absent (backward-compatible with callers that omit it).
    let (caller_id, actor_path) = build_caller_identity(state, args.meta.as_ref()).await;

    // Build a PendingApproval for policy evaluation. actor_path is now
    // correctly set based on the actual caller role.
    let pending = PendingApproval {
        id: uuid::Uuid::now_v7(),
        requesting_actor_id: caller_id.clone(),
        actor_path,
        category: ApprovalCategory::Plan,
        summary: args.plan.summary.clone(),
        plan: Some(args.plan.clone()),
        blocks: vec![caller_id.clone()],
        created_at: chrono::Utc::now(),
        ttl_secs: args
            .timeout_secs
            .or(state.root.manifest.lead_timeout_secs)
            .unwrap_or(3600),
        fallback: ApprovalFallback::AutoReject,
    };

    // Evaluate operator-declared policy before falling through to the legacy queue.
    {
        let matcher_guard = state.root.policy_matcher.lock().await;
        if let Some(matcher) = matcher_guard.as_ref() {
            // #151 M5: forward the caller-supplied cost estimate (if any)
            // so cost_over rules can fire on plan-level approvals. Pre-fix
            // this was hard-coded to None and cost_over rules silently
            // never matched for propose_plan.
            match matcher.evaluate(&pending, None, args.cost_estimate) {
                Some(ApprovalAction::AutoApprove) => {
                    tracing::info!(
                        actor = %pending.requesting_actor_id,
                        "plan auto-approved by policy"
                    );
                    caller_layer
                        .plan_approved
                        .store(true, std::sync::atomic::Ordering::Release);
                    state
                        .record_last_approval_response(&pending.requesting_actor_id, true, false)
                        .await;
                    return Ok(ApprovalToolResponse {
                        approved: true,
                        comment: Some("auto-approved by policy".into()),
                        edited_summary: None,
                        reason: None,
                    });
                }
                Some(ApprovalAction::AutoReject) => {
                    tracing::info!(
                        actor = %pending.requesting_actor_id,
                        "plan auto-rejected by policy"
                    );
                    state
                        .record_last_approval_response(&pending.requesting_actor_id, false, false)
                        .await;
                    return Ok(ApprovalToolResponse {
                        approved: false,
                        comment: Some("auto-rejected by policy".into()),
                        edited_summary: None,
                        reason: None,
                    });
                }
                Some(ApprovalAction::Block) | None => {
                    // fall through to operator queue
                }
            }
        }
    }

    let ttl_secs = args
        .timeout_secs
        .or(state.root.manifest.lead_timeout_secs)
        .unwrap_or(3600);
    let timeout = Duration::from_secs(ttl_secs);
    // Reuse the summary from the plan as the modal headline — a plan
    // approval without the structured fields would be useless, but the
    // summary still anchors the audit trail.
    let summary = args.plan.summary.clone();
    let caller_id_for_record = caller_id.clone();
    let bridge = crate::mcp::approval::ApprovalBridge::new(Arc::clone(state));
    match bridge
        .request(
            caller_id,
            summary,
            Some(args.plan),
            crate::control::protocol::ApprovalKind::Plan,
            timeout,
            Some(ttl_secs),
            Some(crate::mcp::approval::ApprovalFallback::AutoReject),
        )
        .await
    {
        Ok(resp) => {
            if resp.approved {
                caller_layer
                    .plan_approved
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            state
                .record_last_approval_response(&caller_id_for_record, resp.approved, resp.from_ttl)
                .await;
            Ok(ApprovalToolResponse {
                approved: resp.approved,
                comment: resp.comment,
                edited_summary: resp.edited_summary,
                reason: resp.reason,
            })
        }
        Err(e) => anyhow::bail!("plan approval failed: {e}"),
    }
}

/// Claude Code's permission gate payload for the `permission_prompt` MCP tool.
/// Fields are forwarded as-is from Claude's permission check request.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PermissionPromptArgs {
    /// Name of the tool Claude wants to use (e.g. "Bash", "Write").
    pub tool_name: String,
    /// Optional structured input Claude intends to pass. Shown to the
    /// operator in the approval modal for context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    /// Optional cost estimate (USD) hint for policy matching. When
    /// provided, the policy matcher can evaluate `match.cost_over`
    /// rules against this value — e.g. for tools whose cost varies
    /// by input size, the gating side can supply an estimate so an
    /// operator-declared `cost_over` rule fires for expensive
    /// permission requests. Falls through to `None` matching when
    /// omitted, matching the pre-#151-M5 behavior. (#151 M5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_estimate: Option<f64>,
    /// Caller identity injected by mcp-bridge.
    #[serde(rename = "_meta", default, skip_serializing)]
    #[schemars(skip)]
    pub meta: Option<crate::shared_store::tools::MetaField>,
}

/// Response shape matching Claude Code's permission gate contract.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PermissionPromptResponse {
    /// `"allow"` or `"deny"`.
    pub decision: String,
    /// Present when `decision == "allow"`. `"allow_once"` keeps the gate
    /// active for future tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<String>,
}

/// Handle Path B `permission_prompt`: routes claude's per-tool permission
/// check through pitboss's approval queue and TUI. Returns the Claude Code
/// permission gate response shape (`decision` + `behavior`).
pub async fn handle_permission_prompt(
    state: &Arc<DispatchState>,
    args: PermissionPromptArgs,
) -> Result<PermissionPromptResponse> {
    let (caller_id, actor_path) = build_caller_identity(state, args.meta.as_ref()).await;

    let summary = format!("Permission request: {}", args.tool_name);
    let pending = crate::dispatch::state::PendingApproval {
        id: uuid::Uuid::now_v7(),
        requesting_actor_id: caller_id.clone(),
        actor_path,
        category: crate::mcp::approval::ApprovalCategory::ToolUse,
        summary: summary.clone(),
        plan: None,
        blocks: vec![caller_id.clone()],
        created_at: chrono::Utc::now(),
        ttl_secs: state.root.manifest.lead_timeout_secs.unwrap_or(3600),
        fallback: crate::mcp::approval::ApprovalFallback::AutoReject,
    };

    // Evaluate operator-declared policy first.
    {
        let matcher_guard = state.root.policy_matcher.lock().await;
        if let Some(matcher) = matcher_guard.as_ref() {
            // #151 M5: forward the caller-supplied cost estimate (if any)
            // so cost_over rules can fire on permission_prompt. Pre-fix
            // this was hard-coded to None and cost_over rules silently
            // never matched for permission_prompt.
            match matcher.evaluate(&pending, Some(&args.tool_name), args.cost_estimate) {
                Some(crate::mcp::policy::ApprovalAction::AutoApprove) => {
                    return Ok(PermissionPromptResponse {
                        decision: "allow".into(),
                        behavior: Some("allow_once".into()),
                    });
                }
                Some(crate::mcp::policy::ApprovalAction::AutoReject) => {
                    return Ok(PermissionPromptResponse {
                        decision: "deny".into(),
                        behavior: None,
                    });
                }
                Some(crate::mcp::policy::ApprovalAction::Block) | None => {}
            }
        }
    }

    let ttl_secs = state.root.manifest.lead_timeout_secs.unwrap_or(3600);
    let bridge = crate::mcp::approval::ApprovalBridge::new(Arc::clone(state));
    match bridge
        .request(
            caller_id,
            summary,
            None,
            crate::control::protocol::ApprovalKind::Action,
            Duration::from_secs(ttl_secs),
            Some(ttl_secs),
            Some(crate::mcp::approval::ApprovalFallback::AutoReject),
        )
        .await
    {
        Ok(resp) if resp.approved => Ok(PermissionPromptResponse {
            decision: "allow".into(),
            behavior: Some("allow_once".into()),
        }),
        Ok(_) => Ok(PermissionPromptResponse {
            decision: "deny".into(),
            behavior: None,
        }),
        Err(e) => anyhow::bail!("permission_prompt failed: {e}"),
    }
}
