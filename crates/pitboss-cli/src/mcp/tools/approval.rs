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
        let matcher_guard = state.root.approvals.matcher.lock().await;
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
                    crate::mcp::approval::bump_approval_requested(
                        state,
                        &pending.requesting_actor_id,
                    )
                    .await;
                    crate::mcp::approval::record_approval_outcome(
                        state,
                        &pending.requesting_actor_id,
                        true,
                    )
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
                    crate::mcp::approval::bump_approval_requested(
                        state,
                        &pending.requesting_actor_id,
                    )
                    .await;
                    crate::mcp::approval::record_approval_outcome(
                        state,
                        &pending.requesting_actor_id,
                        false,
                    )
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
/// `state.root.approvals.plan_approved`, so a sub-lead's approval silently
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
        let matcher_guard = state.root.approvals.matcher.lock().await;
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
                        .approvals
                        .plan_approved
                        .store(true, std::sync::atomic::Ordering::Release);
                    state
                        .record_last_approval_response(&pending.requesting_actor_id, true, false)
                        .await;
                    crate::mcp::approval::bump_approval_requested(
                        state,
                        &pending.requesting_actor_id,
                    )
                    .await;
                    crate::mcp::approval::record_approval_outcome(
                        state,
                        &pending.requesting_actor_id,
                        true,
                    )
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
                    crate::mcp::approval::bump_approval_requested(
                        state,
                        &pending.requesting_actor_id,
                    )
                    .await;
                    crate::mcp::approval::record_approval_outcome(
                        state,
                        &pending.requesting_actor_id,
                        false,
                    )
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
                    .approvals
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
    ///
    /// **Advisory only — caller-supplied, not trustworthy.** A buggy
    /// or malicious caller (Claude's gate code is also a caller here)
    /// can pass `0.0` to bypass any `cost_over` threshold. Operators
    /// who need hard cost gates should use `auto_reject` rules on
    /// `tool_name`/`actor` or rely on the server-side
    /// `[run].budget_usd` cap. (F-SEC-8 / #531)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_estimate: Option<f64>,
    /// Caller identity injected by mcp-bridge.
    #[serde(rename = "_meta", default, skip_serializing)]
    #[schemars(skip)]
    pub meta: Option<crate::shared_store::tools::MetaField>,
}

/// Response shape matching Claude Code's `--permission-prompt-tool`
/// contract — the `PermissionResult` SDK type. Internally tagged on
/// `behavior` so `{"behavior": "allow", ...}` / `{"behavior": "deny", ...}`
/// JSON round-trips cleanly. (#368)
///
/// Pre-#368 this struct used `decision` / `behavior: "allow_once"` /
/// `reason`, which Claude's gate parser silently rejected at the wire
/// level — every Path B permission check failed with "Permission prompt
/// tool returned an invalid result," and the model treated denials as
/// generic tool errors rather than structured allow/deny.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "behavior", rename_all = "lowercase")]
pub enum PermissionPromptResponse {
    /// Permit the tool call. `updated_input` lets the gate edit the
    /// input claude proposed; pitboss currently passes the input through
    /// unchanged (no operator-side editing surface yet).
    Allow {
        #[serde(
            rename = "updatedInput",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        updated_input: Option<serde_json::Value>,
    },
    /// Deny the tool call. `message` is surfaced to the model so it can
    /// adapt (e.g. switch tools) without an operator round-trip.
    /// `interrupt` requests claude halt the current turn entirely;
    /// pitboss leaves it `false` so the model can keep working through
    /// non-denied tools.
    Deny {
        message: String,
        #[serde(default, skip_serializing_if = "is_default_false")]
        interrupt: bool,
    },
}

fn is_default_false(b: &bool) -> bool {
    !b
}

/// Re-export of the canonical denial-reason enum, owned by the events
/// module so the audit-log serialization shape is single-sourced.
/// (#370 item 4 — collapsed the previous duplicate
/// `PermissionDenialReason` into this type.)
pub use crate::dispatch::events::DeniedReasonKind as PermissionDenialReason;

/// Handle Path B `permission_prompt`: routes claude's per-tool permission
/// check through pitboss's approval queue and TUI. Returns the Claude Code
/// permission gate response shape (`decision` + `behavior` + optional
/// `reason` for denials).
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

    // Per-server [[mcp_server]].tools allowlist short-circuit (#391
    // slice 4 / #399). Goes BEFORE operator policy + typed-profile
    // because it represents a structural manifest constraint —
    // most-restrictive-wins. An operator's `[[approval_policy]]`
    // auto_approve rule cannot widen what the per-server allowlist
    // forbids; the operator must edit the server's `tools = […]` (or
    // remove it) to allow the tool. PR #400 already validates that
    // declared actor surfaces (worker_type/sublead_type/lead/task tools)
    // can't conflict; this gate catches programmatic spawns
    // (`spawn_worker(tools = […])`) and any defense-in-depth case where
    // a tool somehow reached `permission_prompt` despite the spawn-time
    // `--allowedTools` filter.
    //
    // **Layered ordering (deliberate asymmetry vs. typed-profile):**
    // 1. mcp_server allowlist (this gate, structural — un-overridable)
    // 2. operator policy rules (cross-cutting — can override profile)
    // 3. typed-profile short-circuit (per-class)
    // 4. bridge fallback (operator interactive)
    //
    // The asymmetry — operator `auto_approve` overrides typed-profile
    // but NOT mcp_server.tools — is intentional. `[[mcp_server]].tools`
    // is the operator's per-server consent contract; `[[approval_policy]]`
    // is the operator's behavioral exception layer. Allowing rules to
    // widen the server allowlist would make the per-server contract
    // operator-policy-dependent, which defeats the audit story
    // ("which tools is server X consented to expose?" must have a
    // single answer).
    if let Some(parsed) = crate::manifest::mcp_tools::parse_mcp_tool_name(
        &args.tool_name,
        &state.root.manifest.mcp_servers,
    ) {
        if !parsed.is_admitted() {
            let reason = PermissionDenialReason::mcp_server_allowlist_message(
                &args.tool_name,
                parsed.server_id,
                parsed.tool_name,
            );
            record_permission_denied(
                state,
                &caller_id,
                &args.tool_name,
                PermissionDenialReason::DeniedByMcpServerAllowlist,
                &reason,
            )
            .await;
            state
                .record_last_approval_response(&caller_id, false, false)
                .await;
            crate::mcp::approval::bump_approval_requested(state, &caller_id).await;
            crate::mcp::approval::record_approval_outcome(state, &caller_id, false).await;
            return Ok(PermissionPromptResponse::Deny {
                message: reason,
                interrupt: false,
            });
        }
    }

    // Evaluate operator-declared policy first.
    {
        let matcher_guard = state.root.approvals.matcher.lock().await;
        if let Some(matcher) = matcher_guard.as_ref() {
            // #151 M5: forward the caller-supplied cost estimate (if any)
            // so cost_over rules can fire on permission_prompt. Pre-fix
            // this was hard-coded to None and cost_over rules silently
            // never matched for permission_prompt.
            match matcher.evaluate(&pending, Some(&args.tool_name), args.cost_estimate) {
                Some(crate::mcp::policy::ApprovalAction::AutoApprove) => {
                    // Mirror `handle_request_approval`'s rule short-circuit:
                    // record the response so `approval_driven_termination`
                    // can see a recent decision, and bump both counters
                    // because the bridge is bypassed on this path. (#367)
                    state
                        .record_last_approval_response(&caller_id, true, false)
                        .await;
                    crate::mcp::approval::bump_approval_requested(state, &caller_id).await;
                    crate::mcp::approval::record_approval_outcome(state, &caller_id, true).await;
                    return Ok(PermissionPromptResponse::Allow {
                        updated_input: args.tool_input.clone(),
                    });
                }
                Some(crate::mcp::policy::ApprovalAction::AutoReject) => {
                    let reason = PermissionDenialReason::DeniedByRule.message(&args.tool_name);
                    record_permission_denied(
                        state,
                        &caller_id,
                        &args.tool_name,
                        PermissionDenialReason::DeniedByRule,
                        &reason,
                    )
                    .await;
                    // Same trio as the AutoApprove path. Without these,
                    // `approvals_requested` / `approvals_rejected` undercount
                    // and a worker that exits silently after a rule-driven
                    // deny is misclassified as `Success` instead of
                    // `ApprovalRejected`. (#367)
                    state
                        .record_last_approval_response(&caller_id, false, false)
                        .await;
                    crate::mcp::approval::bump_approval_requested(state, &caller_id).await;
                    crate::mcp::approval::record_approval_outcome(state, &caller_id, false).await;
                    return Ok(PermissionPromptResponse::Deny {
                        message: reason,
                        interrupt: false,
                    });
                }
                Some(crate::mcp::policy::ApprovalAction::Block) | None => {}
            }
        }
    }

    // Typed-profile short-circuit (#252). Order vs. operator rules above
    // is deliberate: rule auto-reject / auto-approve already short-circuited
    // earlier, so an operator's `[[approval_policy]]` retains the
    // cross-cutting override. When no rule decided (`Block` or no match)
    // and the caller has a resolved `[[worker_type]]` / `[[sublead_type]]`,
    // the profile's `tools` allowlist becomes the deciding signal:
    // membership ⇒ auto-approve; absence ⇒ auto-deny without an operator
    // round-trip. Untyped callers (lead, or pre-#252 manifests) fall
    // through to the bridge unchanged.
    if let Some((type_id, role_label, profile_tools)) =
        caller_profile(state, &caller_id, args.meta.as_ref()).await
    {
        if profile_tools.iter().any(|t| t == &args.tool_name) {
            record_permission_auto_approved(state, &caller_id, &args.tool_name, &type_id).await;
            state
                .record_last_approval_response(&caller_id, true, false)
                .await;
            crate::mcp::approval::bump_approval_requested(state, &caller_id).await;
            crate::mcp::approval::record_approval_outcome(state, &caller_id, true).await;
            return Ok(PermissionPromptResponse::Allow {
                updated_input: args.tool_input.clone(),
            });
        }
        let reason = PermissionDenialReason::profile_message(&args.tool_name, role_label, &type_id);
        record_permission_denied(
            state,
            &caller_id,
            &args.tool_name,
            PermissionDenialReason::DeniedByProfile,
            &reason,
        )
        .await;
        state
            .record_last_approval_response(&caller_id, false, false)
            .await;
        crate::mcp::approval::bump_approval_requested(state, &caller_id).await;
        crate::mcp::approval::record_approval_outcome(state, &caller_id, false).await;
        return Ok(PermissionPromptResponse::Deny {
            message: reason,
            interrupt: false,
        });
    }

    let ttl_secs = state.root.manifest.lead_timeout_secs.unwrap_or(3600);
    let bridge = crate::mcp::approval::ApprovalBridge::new(Arc::clone(state));
    match bridge
        .request(
            caller_id.clone(),
            summary,
            None,
            crate::control::protocol::ApprovalKind::Action,
            Duration::from_secs(ttl_secs),
            Some(ttl_secs),
            Some(crate::mcp::approval::ApprovalFallback::AutoReject),
        )
        .await
    {
        Ok(resp) if resp.approved => {
            // Bridge already bumped `approvals_requested` and
            // `approvals_approved` (or those will land via the
            // operator-response path in `respond()`). The handler is
            // still responsible for `record_last_approval_response`
            // so `approval_driven_termination` can see the outcome —
            // mirrors `handle_request_approval`. (#367)
            state
                .record_last_approval_response(&caller_id, true, resp.from_ttl)
                .await;
            Ok(PermissionPromptResponse::Allow {
                updated_input: args.tool_input.clone(),
            })
        }
        Ok(resp) => {
            // #373: distinguish three bridge-driven rejection sources.
            // `from_ttl=true` is the TTL watcher; the bridge's
            // `BRIDGE_AUTO_REJECT_COMMENT` marker means it was
            // `default_approval_policy = "auto_reject"` short-circuiting
            // (no operator involvement); anything else is operator-driven.
            // Pre-fix, `default_approval_policy` rejections were mislabeled
            // as `OperatorRejected` in both the audit log and the
            // model-facing message.
            let kind = if resp.from_ttl {
                PermissionDenialReason::TtlExpired
            } else if resp.comment.as_deref()
                == Some(crate::mcp::approval::BRIDGE_AUTO_REJECT_COMMENT)
            {
                PermissionDenialReason::DeniedByPolicy
            } else {
                PermissionDenialReason::OperatorRejected
            };
            // Prefer the operator-supplied reason if present (e.g. a
            // free-form rejection comment); otherwise fall back to the
            // canned per-kind message so the model still sees something
            // actionable.
            let reason = resp.reason.unwrap_or_else(|| kind.message(&args.tool_name));
            record_permission_denied(state, &caller_id, &args.tool_name, kind, &reason).await;
            // Same as the approve arm: bridge owns the counter bumps,
            // handler owns the last-response record so a fast-exit
            // worker is reclassified by `approval_driven_termination`
            // as `ApprovalRejected` (or `ApprovalTimedOut` when
            // `from_ttl=true`) instead of `Success`. (#367)
            state
                .record_last_approval_response(&caller_id, false, resp.from_ttl)
                .await;
            Ok(PermissionPromptResponse::Deny {
                message: reason,
                interrupt: false,
            })
        }
        Err(e) => anyhow::bail!("permission_prompt failed: {e}"),
    }
}

/// Append a `TaskEvent::ToolDenied` to the requesting actor's
/// `events.jsonl` audit trail. Logs a warning on append failure but
/// otherwise swallows the error — the deny still flows back to claude
/// so a missing audit row must not stall the model. The path is
/// `<run_dir>/tasks/<actor_id>/events.jsonl`, which mirrors the
/// approval-request audit shape used by `handle_request_approval`.
async fn record_permission_denied(
    state: &Arc<DispatchState>,
    actor_id: &str,
    tool_name: &str,
    reason_kind: PermissionDenialReason,
    reason_text: &str,
) {
    let event = crate::dispatch::events::TaskEvent::ToolDenied {
        at: chrono::Utc::now(),
        tool_name: tool_name.to_string(),
        actor_id: actor_id.to_string(),
        reason_kind,
        reason: reason_text.to_string(),
    };
    if let Err(e) =
        crate::dispatch::events::append_event(&state.root.run_subdir, actor_id, &event).await
    {
        tracing::warn!(
            actor_id,
            tool_name,
            error = %e,
            "failed to append ToolDenied event; the denial still propagated to claude"
        );
    }
}

/// Symmetric to [`record_permission_denied`] for the typed-profile
/// auto-approve path (#252). Logged so post-hoc audit shows both sides
/// of the profile-driven gate — without it, only rejections would land
/// on `events.jsonl` and an operator reading the file couldn't tell
/// whether the silence on the approve side meant "nothing was attempted"
/// or "every Read/Glob/etc. quietly passed via the profile allowlist."
async fn record_permission_auto_approved(
    state: &Arc<DispatchState>,
    actor_id: &str,
    tool_name: &str,
    actor_type: &str,
) {
    let event = crate::dispatch::events::TaskEvent::ToolAutoApproved {
        at: chrono::Utc::now(),
        tool_name: tool_name.to_string(),
        actor_id: actor_id.to_string(),
        actor_type: actor_type.to_string(),
    };
    if let Err(e) =
        crate::dispatch::events::append_event(&state.root.run_subdir, actor_id, &event).await
    {
        tracing::warn!(
            actor_id,
            tool_name,
            actor_type,
            error = %e,
            "failed to append ToolAutoApproved event; the approval still propagated to claude"
        );
    }
}

/// Sentinel `type_id` returned by [`caller_profile`] when the synthetic-
/// default fallback fires (i.e. an un-typed Worker / Sublead under
/// `untyped_actor_policy = "block"`). Audit consumers reading
/// `events.jsonl` see this string in `tool_denied.actor_type` and
/// `tool_auto_approved.actor_type` and can distinguish synthesized
/// denials from declared-profile denials. Kept as a `<...>` token so
/// it can never collide with a valid manifest profile id (validate
/// rejects `<` in ids).
const SYNTHETIC_PROFILE_ID: &str = "<synthetic>";

/// Resolve a permission_prompt caller to its `(type_id, role_label,
/// profile_tools)` triple, looking up the profile from the manifest.
///
/// Returns `None` for the lead (root lead is never typed), for the
/// pre-#252 un-typed bridge fallback (when
/// `[run].untyped_actor_policy = "bridge"`, which is the default),
/// and when a typed caller's id is unknown to the manifest's profile
/// list (shouldn't happen post-spawn; rejected by `actor_type::resolve_*`
/// at spawn time, but defensive against state corruption).
///
/// When `[run].untyped_actor_policy = "block"`, an un-typed caller
/// receives a SYNTHESIZED profile with [`SYNTHETIC_PROFILE_ID`] and an
/// empty `tools` list. The downstream short-circuit then auto-denies
/// every call (because no tool is in the empty allowlist) — the
/// "manifest is the consent signal" policy enacted at runtime.
///
/// `role_label` is the lowercase string `"worker"` or `"sublead"` used
/// to format the `not in <role>_type '<id>' allowlist` model-facing
/// denial message — matching the plan's convention exactly so a Claude
/// session can pattern-match and adapt.
async fn caller_profile(
    state: &Arc<DispatchState>,
    caller_id: &str,
    meta: Option<&crate::shared_store::tools::MetaField>,
) -> Option<(String, &'static str, Vec<String>)> {
    use crate::manifest::schema::UntypedActorPolicy;
    use crate::shared_store::ActorRole;

    let m = meta?;
    let manifest = &state.root.manifest;
    let block_untyped = matches!(manifest.untyped_actor_policy, UntypedActorPolicy::Block);
    match m.actor_role {
        ActorRole::Lead => None,
        ActorRole::Sublead => {
            let type_id = state
                .sublead_actor_types
                .read()
                .await
                .get(caller_id)
                .cloned();
            match type_id {
                Some(id) => {
                    let profile = manifest.sublead_types.iter().find(|st| st.id == id)?;
                    Some((id, "sublead", profile.tools.clone()))
                }
                None if block_untyped => {
                    Some((SYNTHETIC_PROFILE_ID.to_string(), "sublead", Vec::new()))
                }
                None => None,
            }
        }
        ActorRole::Worker => {
            // Same dispatch as `build_caller_identity`: which sub-tree
            // owns this worker? `None` / `Some(None)` is a root-layer
            // worker; `Some(Some(sublead_id))` routes to the sub-layer.
            let layer_opt = state
                .worker_layer_index
                .read()
                .await
                .get(caller_id)
                .cloned();
            let type_id_opt: Option<String> = match layer_opt {
                None | Some(None) => state
                    .root
                    .workers
                    .actor_types
                    .read()
                    .await
                    .get(caller_id)
                    .cloned(),
                Some(Some(sublead_id)) => {
                    // Clone the sub-layer Arc out before awaiting on its
                    // own RwLock — holding `subs` (a guard on
                    // `state.subleads`) across the second `.await` would
                    // pin the outer lock for the duration of the inner
                    // read.
                    let sub = {
                        let subs = state.subleads.read().await;
                        subs.get(&sublead_id).cloned()
                    };
                    match sub {
                        Some(sub) => {
                            let map = sub.workers.actor_types.read().await;
                            map.get(caller_id).cloned()
                        }
                        None => None,
                    }
                }
            };
            match type_id_opt {
                Some(type_id) => {
                    let profile = manifest.worker_types.iter().find(|wt| wt.id == type_id)?;
                    Some((type_id, "worker", profile.tools.clone()))
                }
                None if block_untyped => {
                    Some((SYNTHETIC_PROFILE_ID.to_string(), "worker", Vec::new()))
                }
                None => None,
            }
        }
    }
}
