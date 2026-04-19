//! The six MCP tool handlers exposed to the lead. Real implementations
//! land in Tasks 10-16; this file establishes the types + signatures.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpawnWorkerArgs {
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
        } = u;
        Self {
            input,
            output,
            cache_read,
            cache_creation,
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
        } = s;
        Self {
            input,
            output,
            cache_read,
            cache_creation,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ApprovalToolResponse {
    pub approved: bool,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub edited_summary: Option<String>,
}

use std::sync::Arc;

use anyhow::{bail, Result};
use pitboss_core::store::TaskRecord;
use tokio::time::Duration;
use uuid::Uuid;

use crate::dispatch::state::{DispatchState, WorkerState};

pub async fn handle_spawn_worker(
    state: &Arc<DispatchState>,
    args: SpawnWorkerArgs,
) -> Result<SpawnWorkerResult> {
    // Guard 1: draining
    if state.cancel.is_draining() || state.cancel.is_terminated() {
        bail!("run is draining: no new workers accepted");
    }

    // Guard 2: worker cap
    if let Some(cap) = state.manifest.max_workers {
        let active = state.active_worker_count().await;
        if active >= cap as usize {
            bail!("worker cap reached: {} active (max {})", active, cap);
        }
    }

    // Resolve the worker's model up-front so the budget guard can price it.
    let lead = state.manifest.lead.as_ref();
    let worker_model = args
        .model
        .clone()
        .or_else(|| lead.map(|l| l.model.clone()))
        .unwrap_or_else(|| "claude-haiku-4-5".to_string());

    let task_id = format!("worker-{}", Uuid::now_v7());

    // Guard 3: budget (reservation-aware + model-aware). On pass, reserve the
    // estimated cost so a burst of parallel spawns can't all slip through
    // before the first completion updates `spent_usd`. The reservation is
    // released on completion (both success and spawn-fail paths).
    if let Some(budget) = state.manifest.budget_usd {
        let spent = *state.spent_usd.lock().await;
        let reserved = *state.reserved_usd.lock().await;
        // Estimate this worker's cost using its intended model, as the median
        // of prior workers priced at their actual models (or a model-specific
        // fallback if no worker has completed yet).
        let estimate = estimate_new_worker_cost(state, &worker_model).await;
        if spent + reserved + estimate > budget {
            if let Some(router) = state.notification_router.clone() {
                let envelope = crate::notify::NotificationEnvelope::new(
                    &state.run_id.to_string(),
                    crate::notify::Severity::Error,
                    crate::notify::PitbossEvent::BudgetExceeded {
                        run_id: state.run_id.to_string(),
                        spent_usd: spent,
                        budget_usd: budget,
                    },
                    chrono::Utc::now(),
                );
                let _ = router.dispatch(envelope).await;
            }
            bail!(
                "budget exceeded: ${:.2} spent + ${:.2} reserved + ${:.2} estimated > ${:.2} budget",
                spent, reserved, estimate, budget
            );
        }
        // Reserve.
        *state.reserved_usd.lock().await += estimate;
        state
            .worker_reservations
            .write()
            .await
            .insert(task_id.clone(), estimate);
    }

    {
        let mut workers = state.workers.write().await;
        workers.insert(task_id.clone(), WorkerState::Pending);
    }

    let worker_cancel = pitboss_core::session::CancelToken::new();
    state
        .worker_cancels
        .write()
        .await
        .insert(task_id.clone(), worker_cancel);

    // Record the prompt preview before spawning the background task.
    let prompt_preview: String = args.prompt.chars().take(80).collect();
    state
        .worker_prompts
        .write()
        .await
        .insert(task_id.clone(), prompt_preview);

    // Track the worker's resolved model so `estimate_new_worker_cost` can
    // price completed workers at the correct rate.
    state
        .worker_models
        .write()
        .await
        .insert(task_id.clone(), worker_model.clone());

    // Resolve the worker's directory: args override -> lead.directory fallback.
    let worker_dir: std::path::PathBuf = args
        .directory
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            state
                .manifest
                .lead
                .as_ref()
                .map(|l| l.directory.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        });

    // Resolve tools, timeout: per-args override -> lead defaults -> fallback.
    // (worker_model was resolved above for the budget guard.)
    let worker_tools = args
        .tools
        .clone()
        .or_else(|| lead.map(|l| l.tools.clone()))
        .unwrap_or_default();
    let worker_timeout_secs = args
        .timeout_secs
        .or_else(|| lead.map(|l| l.timeout_secs))
        .unwrap_or(3600);
    let worker_branch = args.branch.clone();
    let worker_use_worktree = lead.is_none_or(|l| l.use_worktree);

    // Retrieve the per-worker cancel token we inserted above.
    let worker_cancel_bg = state
        .worker_cancels
        .read()
        .await
        .get(&task_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("internal: worker_cancel missing after insert"))?;

    let state_bg = Arc::clone(state);
    let task_id_bg = task_id.clone();
    let lead_id_bg = state.lead_id.clone();
    let prompt_bg = args.prompt.clone();

    tokio::spawn(async move {
        run_worker(
            state_bg,
            task_id_bg,
            lead_id_bg,
            prompt_bg,
            worker_dir,
            worker_branch,
            worker_model,
            worker_tools,
            worker_timeout_secs,
            worker_use_worktree,
            worker_cancel_bg,
        )
        .await;
    });

    Ok(SpawnWorkerResult {
        task_id,
        // worktree_path is set later inside Done(rec); callers needing it
        // should go through worker_status / wait_for_worker.
        worktree_path: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    state: Arc<DispatchState>,
    task_id: String,
    lead_id: String,
    prompt: String,
    directory: std::path::PathBuf,
    branch: Option<String>,
    model: String,
    tools: Vec<String>,
    timeout_secs: u64,
    use_worktree: bool,
    cancel: pitboss_core::session::CancelToken,
) {
    use chrono::Utc;
    use pitboss_core::process::SpawnCmd;
    use pitboss_core::session::SessionHandle;
    use pitboss_core::store::TaskStatus;
    use std::time::Duration;

    let task_dir = state.run_subdir.join("tasks").join(&task_id);
    let _ = tokio::fs::create_dir_all(&task_dir).await;
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");

    // Optional worktree prep.
    let mut worktree_handle: Option<pitboss_core::worktree::Worktree> = None;
    let cwd = if use_worktree {
        let name = format!("pitboss-worker-{}-{}", task_id, state.run_id);
        match state.wt_mgr.prepare(&directory, &name, branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                // Persist the worktree path so the TUI's Detail view can run
                // `git diff --shortstat` against it mid-flight, not just after
                // the TaskRecord lands. TaskRecord.worktree_path is only set
                // on settle; writing this sidecar file closes the gap.
                let _ = tokio::fs::write(
                    task_dir.join("worktree.path"),
                    p.to_string_lossy().as_bytes(),
                )
                .await;
                worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                // Release the spawn-time reservation (SpawnFailed path).
                release_reservation(&state, &task_id).await;
                // Record a SpawnFailed TaskRecord and broadcast done.
                let now = Utc::now();
                let rec = TaskRecord {
                    task_id: task_id.clone(),
                    status: TaskStatus::SpawnFailed,
                    exit_code: None,
                    started_at: now,
                    ended_at: now,
                    duration_ms: 0,
                    worktree_path: None,
                    log_path: log_path.clone(),
                    token_usage: Default::default(),
                    claude_session_id: None,
                    final_message_preview: Some(format!("worktree error: {e}")),
                    parent_task_id: Some(lead_id),
                    pause_count: 0,
                    reprompt_count: 0,
                    approvals_requested: 0,
                    approvals_approved: 0,
                    approvals_rejected: 0,
                    model: Some(model.clone()),
                };
                let _ = state.store.append_record(state.run_id, &rec).await;
                state
                    .workers
                    .write()
                    .await
                    .insert(task_id.clone(), WorkerState::Done(rec));
                let _ = state.done_tx.send(task_id);
                return;
            }
        }
    } else {
        directory.clone()
    };

    // Transition Pending → Running.
    state.workers.write().await.insert(
        task_id.clone(),
        WorkerState::Running {
            started_at: Utc::now(),
            session_id: None,
        },
    );

    // Generate worker-scoped mcp-config.json so the worker can reach
    // the shared store via the bridge-injected identity.
    let worker_task_dir = state.run_subdir.join("tasks").join(&task_id);
    tokio::fs::create_dir_all(&worker_task_dir).await.ok();
    let worker_mcp_config = worker_task_dir.join("mcp-config.json");
    let socket_path =
        crate::mcp::server::socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let mcp_config_arg = match crate::dispatch::hierarchical::write_worker_mcp_config(
        &worker_mcp_config,
        &socket_path,
        &task_id,
    )
    .await
    {
        Ok(()) => Some(worker_mcp_config),
        Err(e) => {
            tracing::warn!("write worker mcp-config for {task_id}: {e}; proceeding without");
            None
        }
    };

    let cmd = SpawnCmd {
        program: state.claude_binary.clone(),
        args: worker_spawn_args(&prompt, &model, &tools, mcp_config_arg.as_deref()),
        cwd: cwd.clone(),
        env: Default::default(),
    };

    let outcome = {
        let (session_id_tx, mut session_id_rx) = tokio::sync::mpsc::channel::<String>(1);
        let session_state = Arc::clone(&state);
        let task_id_for_rx = task_id.clone();
        let promote_task = tokio::spawn(async move {
            if let Some(sid) = session_id_rx.recv().await {
                let mut workers = session_state.workers.write().await;
                if let Some(WorkerState::Running { started_at, .. }) =
                    workers.get(&task_id_for_rx).cloned()
                {
                    workers.insert(
                        task_id_for_rx,
                        WorkerState::Running {
                            started_at,
                            session_id: Some(sid),
                        },
                    );
                }
            }
        });
        // Register a pid slot so the SIGSTOP freeze-pause path can
        // signal this worker directly. Populated inside
        // `run_to_completion` right after the spawn succeeds.
        let pid_slot = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        state
            .worker_pids
            .write()
            .await
            .insert(task_id.clone(), pid_slot.clone());
        let outcome = SessionHandle::new(task_id.clone(), Arc::clone(&state.spawner), cmd)
            .with_log_path(log_path.clone())
            .with_stderr_log_path(stderr_path)
            .with_session_id_tx(session_id_tx)
            .with_pid_slot(pid_slot)
            .run_to_completion(cancel, Duration::from_secs(timeout_secs))
            .await;
        promote_task.abort();
        // Clean up the pid slot — the worker is done, the pid is stale.
        state.worker_pids.write().await.remove(&task_id);
        outcome
    };

    let status = match outcome.final_state {
        pitboss_core::session::SessionState::Completed => TaskStatus::Success,
        pitboss_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
        pitboss_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
        pitboss_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
        pitboss_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
        _ => TaskStatus::Failed,
    };

    // Cleanup worktree per policy.
    if let Some(wt) = worktree_handle {
        let succeeded = matches!(status, TaskStatus::Success);
        let _ = state.wt_mgr.cleanup(wt, state.cleanup_policy, succeeded);
    }

    let worktree_path = if use_worktree { Some(cwd) } else { None };
    let counters = state
        .worker_counters
        .read()
        .await
        .get(&task_id)
        .cloned()
        .unwrap_or_default();
    let rec = TaskRecord {
        task_id: task_id.clone(),
        status,
        exit_code: outcome.exit_code,
        started_at: outcome.started_at,
        ended_at: outcome.ended_at,
        duration_ms: outcome.duration_ms(),
        worktree_path,
        log_path,
        token_usage: outcome.token_usage,
        claude_session_id: outcome.claude_session_id,
        final_message_preview: outcome.final_message_preview,
        parent_task_id: Some(lead_id),
        pause_count: counters.pause_count,
        reprompt_count: counters.reprompt_count,
        approvals_requested: counters.approvals_requested,
        approvals_approved: counters.approvals_approved,
        approvals_rejected: counters.approvals_rejected,
        model: Some(model.clone()),
    };

    // Persist record.
    let _ = state.store.append_record(state.run_id, &rec).await;

    // Release the spawn-time reservation before accumulating actual cost.
    release_reservation(&state, &task_id).await;

    // Accumulate cost into spent_usd.
    if let Some(cost) = pitboss_core::prices::cost_usd(&model, &rec.token_usage) {
        *state.spent_usd.lock().await += cost;
    }

    // Transition to Done + broadcast.
    state
        .workers
        .write()
        .await
        .insert(task_id.clone(), WorkerState::Done(rec));
    let _ = state.done_tx.send(task_id);
}

/// Remove `task_id`'s spawn-time reservation from `reserved_usd`. Safe to
/// call even if no reservation was placed (returns 0 from the map). Clamped
/// at 0.0 to avoid f64 arithmetic drift going negative.
async fn release_reservation(state: &Arc<DispatchState>, task_id: &str) {
    let reserved_amount = state
        .worker_reservations
        .write()
        .await
        .remove(task_id)
        .unwrap_or(0.0);
    if reserved_amount > 0.0 {
        let mut r = state.reserved_usd.lock().await;
        *r = (*r - reserved_amount).max(0.0);
    }
}

/// MCP tool names workers need permission to call. Narrower than the lead's
/// `PITBOSS_MCP_TOOLS` — workers only get the shared-store surface, never
/// the orchestration tools (spawn_worker / cancel_worker / request_approval
/// / etc.). Pre-approved via `--allowedTools` so claude doesn't stall at
/// the interactive permission prompt.
pub const PITBOSS_WORKER_MCP_TOOLS: &[&str] = &[
    "mcp__pitboss__kv_get",
    "mcp__pitboss__kv_set",
    "mcp__pitboss__kv_cas",
    "mcp__pitboss__kv_list",
    "mcp__pitboss__kv_wait",
    "mcp__pitboss__lease_acquire",
    "mcp__pitboss__lease_release",
];

fn worker_spawn_args(
    prompt: &str,
    model: &str,
    tools: &[String],
    mcp_config: Option<&std::path::Path>,
) -> Vec<String> {
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    // Workers always get the shared-store MCP tools in their allowlist when
    // an mcp-config is supplied, alongside their user-declared tools. Without
    // this, kv_set / lease_acquire / etc. hit the permission prompt which
    // can't be answered in non-interactive mode.
    let mut allowed: Vec<String> = tools.to_vec();
    if mcp_config.is_some() {
        for t in PITBOSS_WORKER_MCP_TOOLS {
            allowed.push((*t).to_string());
        }
    }
    if !allowed.is_empty() {
        args.push("--allowedTools".into());
        args.push(allowed.join(","));
    }
    args.push("--model".into());
    args.push(model.to_string());
    if let Some(path) = mcp_config {
        args.push("--mcp-config".into());
        args.push(path.display().to_string());
    }
    args.push("-p".into());
    args.push(prompt.to_string());
    args
}

/// Spawn a resume-subprocess for `task_id`, replacing the worker's current
/// SessionHandle. Used by `pause_worker` → `continue_worker` and by
/// `reprompt_worker`. Returns immediately after setting state to Running; the
/// background task drives `run_to_completion` and the terminal TaskRecord.
pub async fn spawn_resume_worker(
    state: &Arc<DispatchState>,
    task_id: String,
    prompt: String,
    session_id: String,
) -> anyhow::Result<()> {
    use chrono::Utc;
    let model = state
        .worker_models
        .read()
        .await
        .get(&task_id)
        .cloned()
        .unwrap_or_else(|| "claude-haiku-4-5".to_string());
    let tools: Vec<String> = state
        .manifest
        .lead
        .as_ref()
        .map(|l| l.tools.clone())
        .unwrap_or_default();
    let timeout_secs = state
        .manifest
        .lead
        .as_ref()
        .map(|l| l.timeout_secs)
        .unwrap_or(3600);
    let cwd = state
        .manifest
        .lead
        .as_ref()
        .map(|l| l.directory.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let worker_cancel = pitboss_core::session::CancelToken::new();
    state
        .worker_cancels
        .write()
        .await
        .insert(task_id.clone(), worker_cancel.clone());
    state.workers.write().await.insert(
        task_id.clone(),
        WorkerState::Running {
            started_at: Utc::now(),
            session_id: Some(session_id.clone()),
        },
    );
    let state_bg = Arc::clone(state);
    let task_id_bg = task_id.clone();
    let lead_id_bg = state.lead_id.clone();

    // Generate (or reuse) worker-scoped mcp-config.json for the resumed
    // subprocess. write_worker_mcp_config is idempotent so calling it again
    // on an existing file is safe.
    let worker_task_dir = state.run_subdir.join("tasks").join(&task_id);
    tokio::fs::create_dir_all(&worker_task_dir).await.ok();
    let worker_mcp_config_path = worker_task_dir.join("mcp-config.json");
    let socket_path =
        crate::mcp::server::socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let mcp_config_arg = match crate::dispatch::hierarchical::write_worker_mcp_config(
        &worker_mcp_config_path,
        &socket_path,
        &task_id,
    )
    .await
    {
        Ok(()) => Some(worker_mcp_config_path),
        Err(e) => {
            tracing::warn!(
                "write worker mcp-config for {task_id} (resume): {e}; proceeding without"
            );
            None
        }
    };

    // Build spawn args with --resume.
    let mut spawn_args_v = worker_spawn_args(&prompt, &model, &tools, mcp_config_arg.as_deref());
    spawn_args_v.insert(0, "--resume".into());
    spawn_args_v.insert(1, session_id);

    let cmd = pitboss_core::process::SpawnCmd {
        program: state.claude_binary.clone(),
        args: spawn_args_v,
        cwd,
        env: Default::default(),
    };
    let task_dir = state.run_subdir.join("tasks").join(&task_id);
    let _ = tokio::fs::create_dir_all(&task_dir).await;
    let log_path = task_dir.join("stdout.log");
    let stderr_path = task_dir.join("stderr.log");
    let resume_model = model.clone();
    // Register a pid slot for the resumed subprocess too, so
    // freeze-pause works across continue_worker boundaries.
    let resume_pid_slot = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    state
        .worker_pids
        .write()
        .await
        .insert(task_id.clone(), resume_pid_slot.clone());

    tokio::spawn(async move {
        use pitboss_core::store::{TaskRecord, TaskStatus};
        let outcome = pitboss_core::session::SessionHandle::new(
            task_id_bg.clone(),
            Arc::clone(&state_bg.spawner),
            cmd,
        )
        .with_log_path(log_path.clone())
        .with_stderr_log_path(stderr_path)
        .with_pid_slot(resume_pid_slot)
        .run_to_completion(worker_cancel, std::time::Duration::from_secs(timeout_secs))
        .await;
        // Clean up the pid slot when the resumed subprocess exits.
        state_bg.worker_pids.write().await.remove(&task_id_bg);
        let status = match outcome.final_state {
            pitboss_core::session::SessionState::Completed => TaskStatus::Success,
            pitboss_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
            pitboss_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
            pitboss_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
            pitboss_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
            _ => TaskStatus::Failed,
        };
        let counters = state_bg
            .worker_counters
            .read()
            .await
            .get(&task_id_bg)
            .cloned()
            .unwrap_or_default();
        let rec = TaskRecord {
            task_id: task_id_bg.clone(),
            status,
            exit_code: outcome.exit_code,
            started_at: outcome.started_at,
            ended_at: outcome.ended_at,
            duration_ms: outcome.duration_ms(),
            worktree_path: None,
            log_path,
            token_usage: outcome.token_usage,
            claude_session_id: outcome.claude_session_id,
            final_message_preview: outcome.final_message_preview,
            parent_task_id: Some(lead_id_bg),
            pause_count: counters.pause_count,
            reprompt_count: counters.reprompt_count,
            approvals_requested: counters.approvals_requested,
            approvals_approved: counters.approvals_approved,
            approvals_rejected: counters.approvals_rejected,
            model: Some(resume_model),
        };
        let _ = state_bg.store.append_record(state_bg.run_id, &rec).await;
        state_bg
            .workers
            .write()
            .await
            .insert(task_id_bg.clone(), WorkerState::Done(rec));
        let _ = state_bg.done_tx.send(task_id_bg);
    });

    Ok(())
}

async fn estimate_new_worker_cost(state: &Arc<DispatchState>, intended_model: &str) -> f64 {
    use pitboss_core::prices::cost_usd;
    let workers = state.workers.read().await;
    let models = state.worker_models.read().await;
    let mut costs: Vec<f64> = Vec::new();
    for (id, w) in workers.iter() {
        if let WorkerState::Done(rec) = w {
            // Price using THIS worker's actual model, fall back to intended.
            let m = models.get(id).map(String::as_str).unwrap_or(intended_model);
            if let Some(c) = cost_usd(m, &rec.token_usage) {
                costs.push(c);
            }
        }
    }
    if costs.is_empty() {
        // Model-specific fallback (Haiku $0.10, Sonnet ~$0.50, Opus ~$2.00).
        return initial_estimate_for(intended_model);
    }
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    costs[costs.len() / 2]
}

/// Initial per-worker cost estimate before any worker has completed. Used as
/// the fallback inside `estimate_new_worker_cost` and as a model-aware
/// replacement for the old `INITIAL_WORKER_COST_EST = 0.10` constant which
/// undercounted Sonnet (~5x) and Opus (~20x) workers.
///
/// Normalizes dated model suffixes (e.g. `claude-haiku-4-5-20251001`) the
/// same way `pitboss_core::prices::rates_for` does.
pub(crate) fn initial_estimate_for(model: &str) -> f64 {
    let base = model.split('-').take(4).collect::<Vec<_>>().join("-");
    match base.as_str() {
        "claude-opus-4-7" => 2.00,
        "claude-sonnet-4-6" => 0.50,
        _ => 0.10, // haiku or unknown
    }
}

pub async fn handle_list_workers(state: &Arc<DispatchState>) -> Vec<WorkerSummary> {
    let workers = state.workers.read().await;
    let prompts = state.worker_prompts.read().await;
    workers
        .iter()
        .filter(|(id, _)| *id != &state.lead_id)
        .map(|(id, w)| {
            let (state_str, started_at) = match w {
                WorkerState::Pending => ("Pending".to_string(), None),
                WorkerState::Running { started_at, .. } => {
                    ("Running".to_string(), Some(started_at.to_rfc3339()))
                }
                WorkerState::Paused { paused_at, .. } => {
                    ("Paused".to_string(), Some(paused_at.to_rfc3339()))
                }
                WorkerState::Frozen { started_at, .. } => {
                    ("Frozen".to_string(), Some(started_at.to_rfc3339()))
                }
                WorkerState::Done(rec) => (
                    match rec.status {
                        pitboss_core::store::TaskStatus::Success => "Completed",
                        pitboss_core::store::TaskStatus::Failed => "Failed",
                        pitboss_core::store::TaskStatus::TimedOut => "TimedOut",
                        pitboss_core::store::TaskStatus::Cancelled => "Cancelled",
                        pitboss_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
                    }
                    .to_string(),
                    Some(rec.started_at.to_rfc3339()),
                ),
            };
            let prompt_preview = prompts.get(id).cloned().unwrap_or_default();
            WorkerSummary {
                task_id: id.clone(),
                state: state_str,
                prompt_preview,
                started_at,
            }
        })
        .collect()
}

pub async fn handle_worker_status(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<WorkerStatus> {
    let workers = state.workers.read().await;
    let w = workers
        .get(task_id)
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let (state_str, started_at, partial_usage, last_text_preview) = match w {
        WorkerState::Pending => (
            "Pending".to_string(),
            None,
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Running { started_at, .. } => (
            "Running".to_string(),
            Some(started_at.to_rfc3339()),
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Paused {
            paused_at,
            prior_token_usage,
            ..
        } => (
            "Paused".to_string(),
            Some(paused_at.to_rfc3339()),
            *prior_token_usage,
            None,
        ),
        WorkerState::Frozen { started_at, .. } => (
            "Frozen".to_string(),
            Some(started_at.to_rfc3339()),
            // The child is still alive and its counters haven't been
            // snapshotted at freeze time (partial_usage is populated by
            // Done records). Report zeros rather than inventing a value.
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Done(rec) => (
            match rec.status {
                pitboss_core::store::TaskStatus::Success => "Completed",
                pitboss_core::store::TaskStatus::Failed => "Failed",
                pitboss_core::store::TaskStatus::TimedOut => "TimedOut",
                pitboss_core::store::TaskStatus::Cancelled => "Cancelled",
                pitboss_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
            }
            .to_string(),
            Some(rec.started_at.to_rfc3339()),
            rec.token_usage,
            rec.final_message_preview.clone(),
        ),
    };
    let prompt_preview = state
        .worker_prompts
        .read()
        .await
        .get(task_id)
        .cloned()
        .unwrap_or_default();
    Ok(WorkerStatus {
        state: state_str,
        started_at,
        partial_usage,
        last_text_preview,
        prompt_preview,
    })
}

pub async fn handle_cancel_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<CancelResult> {
    let cancels = state.worker_cancels.read().await;
    let Some(token) = cancels.get(task_id) else {
        anyhow::bail!("unknown task_id: {task_id}");
    };
    token.terminate();
    Ok(CancelResult { ok: true })
}

pub async fn handle_pause_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
    mode: PauseMode,
) -> Result<CancelResult> {
    let mut workers = state.workers.write().await;
    let Some(entry) = workers.get(task_id).cloned() else {
        anyhow::bail!("unknown task_id: {task_id}");
    };
    match entry {
        WorkerState::Running {
            started_at,
            session_id: Some(sid),
        } => match mode {
            PauseMode::Cancel => {
                let cancels = state.worker_cancels.read().await;
                if let Some(tok) = cancels.get(task_id) {
                    tok.terminate();
                }
                workers.insert(
                    task_id.to_string(),
                    WorkerState::Paused {
                        session_id: sid,
                        paused_at: chrono::Utc::now(),
                        prior_token_usage: Default::default(),
                    },
                );
                Ok(CancelResult { ok: true })
            }
            PauseMode::Freeze => {
                // Read the pid slot. If 0 (subprocess hasn't spawned
                // yet), fail — freeze is meaningless without a pid.
                let pid = state
                    .worker_pids
                    .read()
                    .await
                    .get(task_id)
                    .map(|slot| slot.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(0);
                if pid == 0 {
                    anyhow::bail!("cannot freeze {task_id}: worker pid unknown (race with spawn?)");
                }
                crate::dispatch::signals::freeze(pid)?;
                workers.insert(
                    task_id.to_string(),
                    WorkerState::Frozen {
                        session_id: sid,
                        frozen_at: chrono::Utc::now(),
                        started_at,
                    },
                );
                Ok(CancelResult { ok: true })
            }
        },
        WorkerState::Running {
            session_id: None, ..
        } => anyhow::bail!("worker not yet initialized (no session_id)"),
        WorkerState::Paused { .. } => anyhow::bail!("worker already paused"),
        WorkerState::Frozen { .. } => anyhow::bail!("worker already frozen"),
        _ => anyhow::bail!("worker not in a pausable state"),
    }
}

pub async fn handle_continue_worker(
    state: &Arc<DispatchState>,
    args: ContinueWorkerArgs,
) -> Result<CancelResult> {
    let current = state.workers.read().await.get(&args.task_id).cloned();
    match current {
        Some(WorkerState::Paused { session_id, .. }) => {
            let prompt = args.prompt.unwrap_or_else(|| "continue".into());
            spawn_resume_worker(state, args.task_id, prompt, session_id).await?;
            Ok(CancelResult { ok: true })
        }
        Some(WorkerState::Frozen {
            session_id,
            started_at,
            ..
        }) => {
            // SIGCONT the process in place — no respawn, no session
            // replay. The subprocess picks up exactly where it left
            // off. `prompt` is silently ignored in freeze mode (it's
            // a resume-only concept); clients that want to inject a
            // new prompt should thaw + reprompt as two steps.
            let pid = state
                .worker_pids
                .read()
                .await
                .get(&args.task_id)
                .map(|slot| slot.load(std::sync::atomic::Ordering::Relaxed))
                .unwrap_or(0);
            if pid == 0 {
                anyhow::bail!(
                    "cannot thaw {}: pid slot empty (race with exit?)",
                    args.task_id
                );
            }
            crate::dispatch::signals::resume_stopped(pid)?;
            // Transition back to Running, preserving the ORIGINAL
            // started_at so wall-clock duration stays accurate.
            state.workers.write().await.insert(
                args.task_id.clone(),
                WorkerState::Running {
                    started_at,
                    session_id: Some(session_id),
                },
            );
            Ok(CancelResult { ok: true })
        }
        Some(_) => anyhow::bail!("worker not paused"),
        None => anyhow::bail!("unknown task_id: {}", args.task_id),
    }
}

pub async fn handle_reprompt_worker(
    state: &Arc<DispatchState>,
    args: RepromptWorkerArgs,
) -> Result<CancelResult> {
    let current = state.workers.read().await.get(&args.task_id).cloned();
    let session_id = match current {
        Some(WorkerState::Running {
            session_id: Some(sid),
            ..
        }) => {
            let cancels = state.worker_cancels.read().await;
            if let Some(tok) = cancels.get(&args.task_id) {
                tok.terminate();
            }
            // Brief grace so the prior subprocess exits before spawn_resume
            // starts the new one. Matches the control-socket op.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            sid
        }
        Some(WorkerState::Paused { session_id, .. }) => session_id,
        Some(WorkerState::Frozen { .. }) => {
            anyhow::bail!("worker is frozen; continue_worker (SIGCONT) it first before reprompting")
        }
        Some(WorkerState::Running {
            session_id: None, ..
        }) => anyhow::bail!("worker not yet initialized (no session_id)"),
        Some(WorkerState::Pending) => anyhow::bail!("worker is still pending"),
        Some(WorkerState::Done(_)) => anyhow::bail!("worker already completed"),
        None => anyhow::bail!("unknown task_id: {}", args.task_id),
    };

    // Unconditionally record the reprompt attempt — audit trail even if
    // the subsequent spawn fails.
    let _ = crate::dispatch::events::append_event(
        &state.run_subdir,
        &args.task_id,
        &crate::dispatch::events::TaskEvent::Reprompt {
            at: chrono::Utc::now(),
            prompt_preview: args.prompt.chars().take(80).collect(),
            prior_session_id: session_id.clone(),
        },
    )
    .await;

    spawn_resume_worker(state, args.task_id.clone(), args.prompt, session_id).await?;

    // Counter bump is conditional on spawn success so a failed spawn
    // doesn't falsely inflate the reprompt count.
    state
        .worker_counters
        .write()
        .await
        .entry(args.task_id)
        .or_default()
        .reprompt_count += 1;

    Ok(CancelResult { ok: true })
}

pub async fn handle_request_approval(
    state: &Arc<DispatchState>,
    args: RequestApprovalArgs,
) -> Result<ApprovalToolResponse> {
    let timeout = Duration::from_secs(
        args.timeout_secs
            .or(state.manifest.lead_timeout_secs)
            .unwrap_or(3600),
    );
    let bridge = crate::mcp::approval::ApprovalBridge::new(Arc::clone(state));
    match bridge
        .request(
            state.lead_id.clone(),
            args.summary,
            args.plan,
            crate::control::protocol::ApprovalKind::Action,
            timeout,
        )
        .await
    {
        Ok(resp) => Ok(ApprovalToolResponse {
            approved: resp.approved,
            comment: resp.comment,
            edited_summary: resp.edited_summary,
        }),
        Err(e) => anyhow::bail!("approval failed: {e}"),
    }
}

pub async fn handle_wait_for_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
    timeout_secs: Option<u64>,
) -> Result<TaskRecord> {
    // Fast path: already Done.
    {
        let workers = state.workers.read().await;
        if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
            return Ok(rec.clone());
        }
        if !workers.contains_key(task_id) {
            bail!("unknown task_id: {task_id}");
        }
    }

    // Subscribe to done events and wait.
    let mut rx = state.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_worker timed out for {task_id}"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                if completed_id == task_id {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
                        return Ok(rec.clone());
                    }
                    bail!("internal: task_id marked done but record not present");
                }
                // Defensive: our target may actually be Done now; re-check.
                let workers = state.workers.read().await;
                if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
                    return Ok(rec.clone());
                }
                // Not our task and target not yet done — keep waiting.
            }
        }
    }
}

pub async fn handle_wait_for_any(
    state: &Arc<DispatchState>,
    task_ids: &[String],
    timeout_secs: Option<u64>,
) -> Result<(String, TaskRecord)> {
    if task_ids.is_empty() {
        bail!("wait_for_any: task_ids is empty");
    }

    // Fast path: any already Done?
    {
        let workers = state.workers.read().await;
        for id in task_ids {
            if let Some(WorkerState::Done(rec)) = workers.get(id) {
                return Ok((id.clone(), rec.clone()));
            }
        }
    }

    let mut rx = state.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_any timed out"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                // Primary path: our target completed.
                if task_ids.iter().any(|id| id == &completed_id) {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(&completed_id) {
                        return Ok((completed_id, rec.clone()));
                    }
                }
                // Defensive re-scan: a prior broadcast we missed, or a write-ordering race,
                // might mean one of our targets is actually Done now even though the recv'd
                // id isn't in our set. Cheap to check; returns only if found.
                let workers = state.workers.read().await;
                for id in task_ids {
                    if let Some(WorkerState::Done(rec)) = workers.get(id) {
                        return Ok((id.clone(), rec.clone()));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::{ApprovalPolicy, DispatchState, WorkerState};
    use std::sync::Arc;

    async fn test_state() -> Arc<DispatchState> {
        test_state_with_budget(5.0).await
    }

    async fn test_state_with_budget(budget: f64) -> Arc<DispatchState> {
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use pitboss_core::process::fake::{FakeScript, FakeSpawner};
        use pitboss_core::process::ProcessSpawner;
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        // Minimal lead that turns off worktree prep so the background worker
        // spawn path doesn't require a real git repo to run against.
        let lead = ResolvedLead {
            id: "lead".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "lead prompt".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 3600,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(budget),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        // Use a FakeSpawner that holds its children open until terminated.
        // This keeps spawned workers in the Running state throughout the test
        // (rather than transitioning to Done quickly as TokioSpawner + /bin/true
        // would), which keeps the `active_worker_count()` guard deterministic.
        let script = FakeScript::new().hold_until_signal();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        // Leak the TempDir — the state holds paths into it and the test
        // may spawn background workers that write logs inside it.
        let dir_path = dir.path().to_path_buf();
        std::mem::forget(dir);
        let _ = dir_path;
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ))
    }

    #[tokio::test]
    async fn list_workers_empty_when_no_spawns() {
        let state = test_state().await;
        let result = handle_list_workers(&state).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn list_workers_shows_pending_and_running() {
        let state = test_state().await;
        {
            let mut w = state.workers.write().await;
            w.insert("w-1".into(), WorkerState::Pending);
            w.insert(
                "w-2".into(),
                WorkerState::Running {
                    started_at: chrono::Utc::now(),
                    session_id: None,
                },
            );
        }
        let mut result = handle_list_workers(&state).await;
        result.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].task_id, "w-1");
        assert_eq!(result[0].state, "Pending");
        assert_eq!(result[1].task_id, "w-2");
        assert_eq!(result[1].state, "Running");
    }

    #[tokio::test]
    async fn spawn_worker_adds_entry_to_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "investigate issue #1".into(),
            directory: Some("/tmp".into()),
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let result = handle_spawn_worker(&state, args).await.unwrap();
        assert!(result.task_id.starts_with("worker-"));

        // The background task may have already transitioned the worker to
        // Running or Done by the time we read, so we just assert the key
        // exists and is in a valid state (Pending / Running / Done).
        let workers = state.workers.read().await;
        assert_eq!(workers.len(), 1);
        let entry = workers.get(&result.task_id).unwrap();
        assert!(matches!(
            entry,
            WorkerState::Pending | WorkerState::Running { .. } | WorkerState::Done(_)
        ));

        // Verify prompt_preview was recorded.
        let prompts = state.worker_prompts.read().await;
        assert_eq!(
            prompts.get(&result.task_id).unwrap(),
            "investigate issue #1"
        );
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_max_workers_reached() {
        let state = test_state().await; // max_workers = 4
                                        // Fill up to cap
        for i in 0..4 {
            let args = SpawnWorkerArgs {
                prompt: format!("w{}", i),
                directory: None,
                branch: None,
                tools: None,
                timeout_secs: None,
                model: None,
            };
            handle_spawn_worker(&state, args).await.unwrap();
        }
        // 5th call must fail
        let args = SpawnWorkerArgs {
            prompt: "overflow".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("worker cap reached"), "err: {err}");
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_budget_exceeded() {
        let state = test_state().await; // budget_usd = 5.0
        *state.spent_usd.lock().await = 5.0; // at cap
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("budget exceeded"), "err: {err}");
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_draining() {
        let state = test_state().await;
        state.cancel.drain();
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("draining"), "err: {err}");
    }

    #[tokio::test]
    async fn worker_status_reads_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "investigate bug".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();
        let status = handle_worker_status(&state, &spawn.task_id).await.unwrap();
        // The background task may have already transitioned the worker to
        // Running; we accept either state here. Done is not expected because
        // the test FakeSpawner holds its children open until signalled.
        assert!(
            matches!(status.state.as_str(), "Pending" | "Running"),
            "unexpected state: {}",
            status.state
        );
        // prompt_preview is populated synchronously before the background task.
        assert_eq!(status.prompt_preview, "investigate bug");
    }

    #[tokio::test]
    async fn worker_status_unknown_id_errors() {
        let state = test_state().await;
        let err = handle_worker_status(&state, "nope-123").await.unwrap_err();
        assert!(err.to_string().contains("unknown task_id"));
    }

    #[tokio::test]
    async fn cancel_worker_sets_cancelled_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "p".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();

        let result = handle_cancel_worker(&state, &spawn.task_id).await.unwrap();
        assert!(result.ok);

        // Note: in real wiring, CancelToken signals the SessionHandle to terminate
        // and the subsequent Done(...) entry in state.workers carries status=Cancelled.
        // For v0.3 Task 14 (unit-level), we just verify the cancel call succeeded
        // and didn't panic. Full flow is tested in integration tests (Phase 6).
    }

    #[tokio::test]
    async fn wait_for_worker_returns_outcome_on_completion() {
        use pitboss_core::store::{TaskRecord, TaskStatus};
        use std::time::Duration;

        let state = test_state().await;
        let task_id = "worker-test-1".to_string();
        {
            let mut w = state.workers.write().await;
            w.insert(task_id.clone(), WorkerState::Pending);
        }

        // Spawn a task that marks the worker Done after 50 ms.
        let state_clone = state.clone();
        let task_id_clone = task_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let rec = TaskRecord {
                task_id: task_id_clone.clone(),
                status: TaskStatus::Success,
                exit_code: Some(0),
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                duration_ms: 42,
                worktree_path: None,
                log_path: std::path::PathBuf::new(),
                token_usage: Default::default(),
                claude_session_id: None,
                final_message_preview: Some("ok".into()),
                parent_task_id: Some("lead".into()),
                pause_count: 0,
                reprompt_count: 0,
                approvals_requested: 0,
                approvals_approved: 0,
                approvals_rejected: 0,
                model: None,
            };
            let mut w = state_clone.workers.write().await;
            w.insert(task_id_clone.clone(), WorkerState::Done(rec));
            let _ = state_clone.done_tx.send(task_id_clone);
        });

        let outcome = handle_wait_for_worker(&state, &task_id, Some(5))
            .await
            .unwrap();
        assert!(matches!(outcome.status, TaskStatus::Success));
    }

    #[tokio::test]
    async fn wait_for_worker_times_out() {
        let state = test_state().await;
        let task_id = "worker-stuck".to_string();
        {
            let mut w = state.workers.write().await;
            w.insert(task_id.clone(), WorkerState::Pending);
        }
        let err = handle_wait_for_worker(&state, &task_id, Some(0))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "err: {err}");
    }

    #[tokio::test]
    async fn cancel_worker_unknown_id_errors() {
        let state = test_state().await;
        let err = handle_cancel_worker(&state, "never-existed")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown task_id"));
    }

    #[tokio::test]
    async fn wait_for_any_returns_first_completed() {
        use pitboss_core::store::{TaskRecord, TaskStatus};
        use std::time::Duration;

        let state = test_state().await;
        let ids = vec!["w-a".to_string(), "w-b".to_string(), "w-c".to_string()];
        {
            let mut w = state.workers.write().await;
            for id in &ids {
                w.insert(id.clone(), WorkerState::Pending);
            }
        }

        // Race: w-b finishes first at 30ms, w-a at 100ms.
        let state_clone = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let rec = TaskRecord {
                task_id: "w-b".into(),
                status: TaskStatus::Success,
                exit_code: Some(0),
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                duration_ms: 30,
                worktree_path: None,
                log_path: std::path::PathBuf::new(),
                token_usage: Default::default(),
                claude_session_id: None,
                final_message_preview: None,
                parent_task_id: Some("lead".into()),
                pause_count: 0,
                reprompt_count: 0,
                approvals_requested: 0,
                approvals_approved: 0,
                approvals_rejected: 0,
                model: None,
            };
            let mut w = state_clone.workers.write().await;
            w.insert("w-b".into(), WorkerState::Done(rec));
            let _ = state_clone.done_tx.send("w-b".into());
        });

        let (winner_id, _rec) = handle_wait_for_any(&state, &ids, Some(5)).await.unwrap();
        assert_eq!(winner_id, "w-b");
    }

    /// Build a test_state whose FakeSpawner produces a completed session
    /// (with a result event carrying a known token_usage), so the
    /// backgrounded worker actually transitions through the full spawn path.
    async fn completing_test_state() -> Arc<DispatchState> {
        completing_test_state_with_budget(None).await
    }

    async fn completing_test_state_with_budget(budget: Option<f64>) -> Arc<DispatchState> {
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use pitboss_core::process::fake::{FakeScript, FakeSpawner};
        use pitboss_core::process::ProcessSpawner;
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let lead = ResolvedLead {
            id: "lead".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "lead prompt".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: budget,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        // Emit a single result event with known token usage, then exit 0.
        let script = FakeScript::new()
            .stdout_line(r#"{"type":"system","subtype":"init"}"#)
            .stdout_line(
                r#"{"type":"result","session_id":"sess_ok","usage":{"input_tokens":1000,"output_tokens":2000}}"#,
            )
            .exit_code(0);
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        std::mem::forget(dir);
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ))
    }

    #[tokio::test]
    async fn spawn_worker_completes_and_updates_spent_usd_and_parent_task_id() {
        use pitboss_core::store::TaskStatus;
        use std::time::Duration;

        let state = completing_test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "analyze bug #42".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None, // falls back to lead model (claude-haiku-4-5)
        };

        // Subscribe to done events BEFORE spawning.
        let mut rx = state.done_tx.subscribe();
        let spawn = handle_spawn_worker(&state, args).await.unwrap();

        // Wait for the broadcast.
        let id = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("broadcast arrives in time")
            .expect("broadcast channel open");
        assert_eq!(id, spawn.task_id, "broadcast id matches spawn id");

        // Verify Done state + Success + parent_task_id.
        let workers = state.workers.read().await;
        let entry = workers.get(&spawn.task_id).expect("worker recorded");
        match entry {
            WorkerState::Done(rec) => {
                assert!(
                    matches!(rec.status, TaskStatus::Success),
                    "status is Success"
                );
                assert_eq!(rec.parent_task_id.as_deref(), Some("lead"));
                assert_eq!(rec.token_usage.input, 1000);
                assert_eq!(rec.token_usage.output, 2000);
                assert_eq!(rec.claude_session_id.as_deref(), Some("sess_ok"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
        drop(workers);

        // Verify cost accumulation. claude-haiku-4-5: input $0.80/1M, output $4.00/1M.
        // 1000 input = $0.0008; 2000 output = $0.008; total = $0.0088.
        let spent = *state.spent_usd.lock().await;
        assert!(
            (spent - 0.0088).abs() < 1e-6,
            "expected spent_usd ≈ 0.0088, got {spent}"
        );

        // Verify prompt_preview is present.
        let preview = state
            .worker_prompts
            .read()
            .await
            .get(&spawn.task_id)
            .cloned()
            .unwrap_or_default();
        assert_eq!(preview, "analyze bug #42");
    }

    #[tokio::test]
    async fn burst_spawn_is_budget_capped_via_reservation() {
        // Budget = $0.25. With a per-worker haiku estimate of $0.10 (the
        // fallback for haiku when no workers have completed), only 2 workers
        // should pass the guard in a burst:
        //   spawn 1: spent 0 + reserved 0 + est 0.10 = 0.10 ≤ 0.25 → OK, reserved becomes 0.10
        //   spawn 2: spent 0 + reserved 0.10 + est 0.10 = 0.20 ≤ 0.25 → OK, reserved becomes 0.20
        //   spawn 3: spent 0 + reserved 0.20 + est 0.10 = 0.30 > 0.25 → REJECT
        let state = test_state_with_budget(0.25).await;
        // Lead model defaults to "claude-haiku-4-5" in test_state.

        let args = |prompt: &str| SpawnWorkerArgs {
            prompt: prompt.into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };

        let r1 = handle_spawn_worker(&state, args("w1")).await;
        assert!(r1.is_ok(), "first spawn should pass: {r1:?}");

        let r2 = handle_spawn_worker(&state, args("w2")).await;
        assert!(r2.is_ok(), "second spawn should pass: {r2:?}");

        let r3 = handle_spawn_worker(&state, args("w3")).await;
        assert!(r3.is_err(), "third spawn should be rejected by reservation");
        let msg = r3.unwrap_err().to_string();
        assert!(
            msg.contains("budget exceeded"),
            "expected budget-exceeded message, got: {msg}"
        );

        // Sanity: the reservation should now reflect the two passing spawns.
        let reserved_now = *state.reserved_usd.lock().await;
        assert!(
            (reserved_now - 0.20).abs() < 1e-9,
            "expected reserved ≈ 0.20, got {reserved_now}"
        );
    }

    #[tokio::test]
    async fn reservation_released_on_worker_completion() {
        // Spawn one worker, wait for completion, verify reserved_usd returns to 0.
        use std::time::Duration;

        let state = completing_test_state_with_budget(Some(1.00)).await;

        // Subscribe to done events BEFORE spawning — the completion path is
        // fast (FakeScript exits immediately after emitting the result line).
        let mut rx = state.done_tx.subscribe();

        let spawn = handle_spawn_worker(
            &state,
            SpawnWorkerArgs {
                prompt: "p".into(),
                directory: None,
                branch: None,
                tools: None,
                timeout_secs: None,
                model: None,
            },
        )
        .await
        .unwrap();

        // Reservation should be > 0 at some point between spawn and completion;
        // under a very fast FakeSpawner the worker can complete before this
        // read, so we only assert "reservation was initialized to >0". That's
        // checked indirectly via the `worker_reservations` map having an entry
        // (or having had one — it's removed on release).
        // The primary assertion is post-completion.

        let completed_id = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("broadcast arrives in time")
            .expect("broadcast channel open");
        assert_eq!(completed_id, spawn.task_id);

        let reserved_after = *state.reserved_usd.lock().await;
        assert!(
            reserved_after.abs() < 1e-9,
            "reservation should be released after completion, got {reserved_after}"
        );
        let reservations = state.worker_reservations.read().await;
        assert!(
            !reservations.contains_key(&spawn.task_id),
            "reservation entry should be removed on completion"
        );
    }

    #[test]
    fn initial_estimate_is_model_aware() {
        assert!((initial_estimate_for("claude-haiku-4-5") - 0.10).abs() < 1e-9);
        assert!((initial_estimate_for("claude-sonnet-4-6") - 0.50).abs() < 1e-9);
        assert!((initial_estimate_for("claude-opus-4-7") - 2.00).abs() < 1e-9);
        // Unknown model falls back to Haiku's rate.
        assert!((initial_estimate_for("claude-unknown-x-y") - 0.10).abs() < 1e-9);
        // Dated suffix is normalized (matches `rates_for` in pitboss-core::prices).
        assert!((initial_estimate_for("claude-haiku-4-5-20251001") - 0.10).abs() < 1e-9);
        assert!((initial_estimate_for("claude-sonnet-4-6-20251001") - 0.50).abs() < 1e-9);
        assert!((initial_estimate_for("claude-opus-4-7-20251001") - 2.00).abs() < 1e-9);
    }

    #[tokio::test]
    async fn running_worker_state_gets_session_id_after_init() {
        use std::time::Duration;

        let state = completing_test_state().await;
        let mut rx = state.done_tx.subscribe();
        let args = SpawnWorkerArgs {
            prompt: "analyze".into(),
            directory: None,
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("broadcast arrives")
            .expect("broadcast open");

        // Post-completion, the worker is in Done state. The session_id is
        // preserved on TaskRecord via SessionOutcome. Assert it.
        let workers = state.workers.read().await;
        match workers.get(&spawn.task_id).unwrap() {
            WorkerState::Done(rec) => {
                assert_eq!(rec.claude_session_id.as_deref(), Some("sess_ok"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn continue_worker_args_roundtrip() {
        let a = ContinueWorkerArgs {
            task_id: "w".into(),
            prompt: Some("next step".into()),
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: ContinueWorkerArgs = serde_json::from_str(&s).unwrap();
        assert_eq!(back.task_id, "w");
        assert_eq!(back.prompt.as_deref(), Some("next step"));
    }

    #[test]
    fn reprompt_worker_args_roundtrip() {
        let a = RepromptWorkerArgs {
            task_id: "w-1".into(),
            prompt: "new plan".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: RepromptWorkerArgs = serde_json::from_str(&s).unwrap();
        assert_eq!(back.task_id, "w-1");
        assert_eq!(back.prompt, "new plan");
    }

    #[test]
    fn request_approval_args_roundtrip() {
        // Bare form — no plan.
        let a = RequestApprovalArgs {
            summary: "spawn 3 workers".into(),
            timeout_secs: Some(60),
            plan: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: RequestApprovalArgs = serde_json::from_str(&s).unwrap();
        assert_eq!(back.summary, "spawn 3 workers");
        assert_eq!(back.timeout_secs, Some(60));
        assert!(back.plan.is_none());

        // Typed form.
        let b = RequestApprovalArgs {
            summary: "drop staging index".into(),
            timeout_secs: None,
            plan: Some(ApprovalPlan {
                summary: "drop staging index".into(),
                rationale: Some("obsolete since v2".into()),
                resources: vec!["db/idx_foo".into()],
                risks: vec!["slow reads if live".into()],
                rollback: Some("restore from snapshot".into()),
            }),
        };
        let s = serde_json::to_string(&b).unwrap();
        let back: RequestApprovalArgs = serde_json::from_str(&s).unwrap();
        let plan = back.plan.unwrap();
        assert_eq!(plan.rationale.as_deref(), Some("obsolete since v2"));
        assert_eq!(plan.resources, vec!["db/idx_foo".to_string()]);
    }

    #[tokio::test]
    async fn handle_pause_worker_pauses_running_worker() {
        let state = test_state().await;
        let worker_token = pitboss_core::session::CancelToken::new();
        state
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state.workers.write().await.insert(
            "w-1".into(),
            WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess".into()),
            },
        );
        let res = handle_pause_worker(&state, "w-1", PauseMode::Cancel)
            .await
            .unwrap();
        assert!(res.ok);
        assert!(worker_token.is_terminated());
        let workers = state.workers.read().await;
        assert!(matches!(
            workers.get("w-1").unwrap(),
            WorkerState::Paused { .. }
        ));
    }

    /// End-to-end freeze: spawn a real sleeping child, register its pid
    /// slot + a Running WorkerState, call handle_pause_worker(Freeze),
    /// verify Frozen state + that /proc (on Linux) sees the process as
    /// stopped. Then handle_continue_worker to thaw.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn freeze_and_thaw_transition_via_handler() {
        use std::process::Command;

        let state = test_state().await;

        // Spawn a real long-sleep child we can safely SIGSTOP/SIGCONT.
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();

        // Register the pid + Running state.
        let slot = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(pid));
        state
            .worker_pids
            .write()
            .await
            .insert("w-freeze".into(), slot);
        state
            .worker_cancels
            .write()
            .await
            .insert("w-freeze".into(), pitboss_core::session::CancelToken::new());
        state.workers.write().await.insert(
            "w-freeze".into(),
            WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess-freeze".into()),
            },
        );

        // Freeze.
        let res = handle_pause_worker(&state, "w-freeze", PauseMode::Freeze)
            .await
            .unwrap();
        assert!(res.ok);
        assert!(matches!(
            state.workers.read().await.get("w-freeze").unwrap(),
            WorkerState::Frozen { .. }
        ));

        // /proc should show 'T' (stopped).
        std::thread::sleep(std::time::Duration::from_millis(50));
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let state_line = status
            .lines()
            .find(|l| l.starts_with("State:"))
            .unwrap_or("State: ?");
        assert!(
            state_line.contains('T'),
            "expected stopped state, got {state_line}"
        );

        // Thaw via continue_worker (no prompt — freeze path ignores it).
        let cres = handle_continue_worker(
            &state,
            ContinueWorkerArgs {
                task_id: "w-freeze".into(),
                prompt: None,
            },
        )
        .await
        .unwrap();
        assert!(cres.ok);
        assert!(matches!(
            state.workers.read().await.get("w-freeze").unwrap(),
            WorkerState::Running { .. }
        ));

        // Cleanup.
        let mut owned = child;
        let _ = owned.kill();
        let _ = owned.wait();
    }

    #[tokio::test]
    async fn handle_continue_worker_resumes_paused() {
        let state = test_state().await;
        state.workers.write().await.insert(
            "w-1".into(),
            WorkerState::Paused {
                session_id: "sess".into(),
                paused_at: chrono::Utc::now(),
                prior_token_usage: Default::default(),
            },
        );
        state
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "hi".into());
        state
            .worker_models
            .write()
            .await
            .insert("w-1".into(), "claude-haiku-4-5".into());
        let res = handle_continue_worker(
            &state,
            ContinueWorkerArgs {
                task_id: "w-1".into(),
                prompt: Some("resume please".into()),
            },
        )
        .await
        .unwrap();
        assert!(res.ok);
        let workers = state.workers.read().await;
        assert!(matches!(
            workers.get("w-1").unwrap(),
            WorkerState::Running { .. }
        ));
    }

    #[tokio::test]
    async fn handle_reprompt_worker_from_running() {
        let state = test_state().await;
        let worker_token = pitboss_core::session::CancelToken::new();
        state
            .worker_cancels
            .write()
            .await
            .insert("w-1".into(), worker_token.clone());
        state.workers.write().await.insert(
            "w-1".into(),
            WorkerState::Running {
                started_at: chrono::Utc::now(),
                session_id: Some("sess-abc".into()),
            },
        );
        state
            .worker_prompts
            .write()
            .await
            .insert("w-1".into(), "original".into());
        state
            .worker_models
            .write()
            .await
            .insert("w-1".into(), "claude-haiku-4-5".into());

        let res = handle_reprompt_worker(
            &state,
            RepromptWorkerArgs {
                task_id: "w-1".into(),
                prompt: "new plan".into(),
            },
        )
        .await
        .unwrap();

        assert!(res.ok);
        // Counter bumps on success.
        let counters = state
            .worker_counters
            .read()
            .await
            .get("w-1")
            .cloned()
            .unwrap_or_default();
        assert_eq!(counters.reprompt_count, 1);
        // events.jsonl records the reprompt.
        let events_path = state
            .run_subdir
            .join("tasks")
            .join("w-1")
            .join("events.jsonl");
        let events = tokio::fs::read_to_string(&events_path).await.unwrap();
        assert!(
            events.contains("\"kind\":\"reprompt\""),
            "events.jsonl missing reprompt: {events}"
        );
        // Worker transitioned back to Running via spawn_resume_worker.
        let workers = state.workers.read().await;
        assert!(matches!(
            workers.get("w-1").unwrap(),
            WorkerState::Running { .. }
        ));
    }

    #[tokio::test]
    async fn handle_reprompt_worker_from_done_errors() {
        let state = test_state().await;
        // Insert a Done worker — terminal state, no reprompt allowed.
        let rec = pitboss_core::store::TaskRecord {
            task_id: "w-done".into(),
            status: pitboss_core::store::TaskStatus::Success,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 0,
            worktree_path: None,
            log_path: std::path::PathBuf::from("/tmp/x"),
            token_usage: Default::default(),
            claude_session_id: Some("sess-done".into()),
            final_message_preview: None,
            parent_task_id: Some("lead".into()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
        };
        state
            .workers
            .write()
            .await
            .insert("w-done".into(), WorkerState::Done(rec));

        let err = handle_reprompt_worker(
            &state,
            RepromptWorkerArgs {
                task_id: "w-done".into(),
                prompt: "retry".into(),
            },
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("already completed"),
            "expected 'already completed' in error, got: {err}"
        );
    }

    #[tokio::test]
    async fn handle_request_approval_auto_approves() {
        use crate::dispatch::state::ApprovalPolicy;
        // Rebuild a state with AutoApprove.
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use pitboss_core::process::fake::{FakeScript, FakeSpawner};
        use pitboss_core::process::ProcessSpawner;
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let lead = ResolvedLead {
            id: "lead".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_timeout_secs: None,
            approval_policy: Some(ApprovalPolicy::AutoApprove),
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let script = FakeScript::new().hold_until_signal();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(script));
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_id = Uuid::now_v7();
        let run_subdir = dir.path().join(run_id.to_string());
        std::mem::forget(dir);
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::AutoApprove,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ));
        let resp = handle_request_approval(
            &state,
            RequestApprovalArgs {
                summary: "spawn 3".into(),
                timeout_secs: Some(2),
                plan: None,
            },
        )
        .await
        .unwrap();
        assert!(resp.approved);
    }
}
