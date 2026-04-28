//! Kill+resume subprocess loop used by both the root lead
//! (`run_hierarchical`) and sub-leads (`spawn_sublead_session`).
//!
//! Each iteration spawns a Claude subprocess via `SessionHandle`, then
//! when the subprocess exits checks whether a synthetic reprompt is
//! waiting in the reprompt channel. If so, and a `claude_session_id`
//! was captured during the iteration, the next iteration spawns under
//! `claude --resume <session_id> -p <new_prompt>` (built via the
//! caller-supplied `build_resume_cmd` closure). Otherwise the loop
//! breaks with the most recent `SessionOutcome`.
//!
//! Before this module existed, the same loop was inlined twice — once
//! in `dispatch/hierarchical.rs` for the root lead and once in
//! `dispatch/sublead.rs` for sub-leads. The two copies drifted (see the
//! `hierarchical.rs:261` audit doc-comment that explicitly said
//! "identical in structure to spawn_sublead_session"), and the audit
//! flagged the duplication as a footgun. Centralizing the protocol in
//! one place keeps the loop's behavior consistent across actor types.
//!
//! # What the helper takes vs. what the call site keeps
//!
//! * The helper takes the `LayerState` so it can read `cancel`,
//!   `spawner`, and update the `workers` map — these touch shared run
//!   state and would be tedious to thread through more arguments.
//! * The helper takes log paths and the per-iteration timeout
//!   explicitly because they are caller-resolved (the root lead and
//!   sub-leads compute them differently).
//! * The helper takes a `build_resume_cmd` closure so each call site
//!   keeps ownership of its own resume-args / env / cwd resolution
//!   (the rules differ — see `lead_resume_spawn_args` vs
//!   `sublead_spawn_args`).
//! * Cost accumulation is **not** done inside the helper. Sub-leads
//!   apply the cost from `result.total_token_usage` once after the
//!   loop returns; the timing change vs. per-iteration accumulation
//!   is benign because no worker spawn into the sub-tree can occur
//!   between iterations (the sub-lead's MCP session is closed while
//!   its subprocess is dead).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc::UnboundedReceiver;

use pitboss_core::parser::TokenUsage;
use pitboss_core::process::SpawnCmd;
use pitboss_core::session::{CancelToken, SessionHandle, SessionOutcome};

use crate::dispatch::layer::LayerState;
use crate::dispatch::state::WorkerState;

/// Per-iteration inputs that don't live on `LayerState`.
pub struct KillResumeArgs {
    /// Actor identity used as the `workers` map key and in tracing
    /// breadcrumbs (`lead.id` for the root lead, `sublead_id` for a
    /// sub-lead).
    pub actor_id: String,
    /// Spawn command for the first iteration. Subsequent iterations
    /// use whatever `build_resume_cmd` returns.
    pub initial_cmd: SpawnCmd,
    /// Per-iteration subprocess timeout (passed straight through to
    /// `SessionHandle::run_to_completion`).
    pub timeout: Duration,
    /// Stdout log path passed to every iteration's `SessionHandle`.
    pub log_path: PathBuf,
    /// Stderr log path passed to every iteration's `SessionHandle`.
    pub stderr_path: PathBuf,
}

/// Aggregated result across all iterations.
pub struct KillResumeResult {
    /// The outcome of the iteration that broke the loop (terminated
    /// without a follow-up reprompt, OR terminated with a reprompt
    /// waiting but no captured `session_id`).
    pub final_outcome: SessionOutcome,
    /// Token usage summed across every iteration. The caller uses this
    /// to compute the actor's compound `TaskRecord` cost.
    pub total_token_usage: TokenUsage,
    /// Number of synthetic reprompts that triggered a kill+resume.
    /// Always equal to `iterations - 1`.
    pub reprompt_count: u32,
    /// Most recently captured `claude_session_id`, if any.
    pub last_session_id: Option<String>,
    /// Wall-clock time of the very first iteration's start. Used as
    /// the `started_at` of the compound `TaskRecord`.
    pub overall_started_at: DateTime<Utc>,
}

/// Run a Claude actor (root lead or sub-lead) under a kill+resume
/// loop. The loop terminates when an iteration exits without a pending
/// synthetic reprompt, or when a reprompt arrives but no
/// `claude_session_id` was captured in the prior iteration (so
/// `--resume` is impossible).
///
/// Side-effects on `layer.workers`:
///
/// * Initially inserts `Running { started_at: now, session_id: None }`.
/// * Per iteration, after a `session_id` is captured, updates to
///   `Running { started_at: overall_started_at, session_id: Some(sid) }`.
/// * Per resume, resets to
///   `Running { started_at: overall_started_at, session_id: None }`
///   so the workers-map view reflects the in-flight subprocess that
///   has not yet emitted its `init` event.
pub async fn run_kill_resume_loop(
    layer: Arc<LayerState>,
    args: KillResumeArgs,
    mut reprompt_rx: UnboundedReceiver<String>,
    mut build_resume_cmd: impl FnMut(&str, &str) -> SpawnCmd,
) -> KillResumeResult {
    let actor_id = args.actor_id;
    let mut current_cmd = args.initial_cmd;

    let overall_started_at = Utc::now();
    layer.workers.write().await.insert(
        actor_id.clone(),
        WorkerState::Running {
            started_at: overall_started_at,
            session_id: None,
        },
    );

    let mut last_session_id: Option<String> = None;
    let mut total_token_usage = TokenUsage::default();
    let mut reprompt_count: u32 = 0;

    let final_outcome = loop {
        let (session_id_tx, mut session_id_rx) = tokio::sync::mpsc::channel::<String>(1);

        // Per-iteration cancel token: forwards tree-level terminate to
        // the subprocess. Lets operator Ctrl-C / cascade kills still
        // reach the subprocess while allowing the reprompt path to
        // kill+restart this iteration without terminating the whole
        // tree.
        let proc_cancel = CancelToken::new();
        {
            let tree_cancel = layer.cancel.clone();
            let proc = proc_cancel.clone();
            tokio::spawn(async move {
                tree_cancel.await_terminate().await;
                proc.terminate();
            });
        }

        let outcome = SessionHandle::new(
            actor_id.clone(),
            Arc::clone(&layer.spawner),
            current_cmd.clone(),
        )
        .with_log_path(args.log_path.clone())
        .with_stderr_log_path(args.stderr_path.clone())
        .with_session_id_tx(session_id_tx)
        .run_to_completion(proc_cancel, args.timeout)
        .await;

        // Capture session_id from the per-iteration channel (preferred,
        // fires on the `system{subtype:"init"}` event so it's available
        // mid-run) or from the final result event.
        if let Ok(sid) = session_id_rx.try_recv() {
            layer.workers.write().await.insert(
                actor_id.clone(),
                WorkerState::Running {
                    started_at: overall_started_at,
                    session_id: Some(sid.clone()),
                },
            );
            last_session_id = Some(sid);
        } else if let Some(ref sid) = outcome.claude_session_id {
            last_session_id = Some(sid.clone());
        }

        total_token_usage.add(&outcome.token_usage);

        let pending_reprompt = reprompt_rx.try_recv().ok();
        if let Some(new_prompt) = pending_reprompt {
            if let Some(ref sid) = last_session_id {
                tracing::info!(
                    actor_id = %actor_id,
                    session_id = %sid,
                    "kill+resume: synthetic reprompt received; resuming subprocess"
                );
                reprompt_count += 1;
                current_cmd = build_resume_cmd(sid, &new_prompt);
                layer.workers.write().await.insert(
                    actor_id.clone(),
                    WorkerState::Running {
                        started_at: overall_started_at,
                        session_id: None,
                    },
                );
                continue;
            }
            tracing::warn!(
                actor_id = %actor_id,
                "kill+resume: reprompt arrived but no session_id captured; \
                 treating as normal termination"
            );
            break outcome;
        }

        break outcome;
    };

    KillResumeResult {
        final_outcome,
        total_token_usage,
        reprompt_count,
        last_session_id,
        overall_started_at,
    }
}
