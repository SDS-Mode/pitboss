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
        let bridge = {
            let tree_cancel = layer.cancel.clone();
            let proc = proc_cancel.clone();
            tokio::spawn(async move {
                tree_cancel.await_terminate().await;
                proc.terminate();
            })
        };

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

        // The bridge task awaits `tree_cancel.await_terminate()` which
        // does not fire until the run ends. Without this abort, every
        // iteration leaks a task holding a CancelToken clone — N
        // resumes accumulates N orphans bounded only by run lifetime.
        // The session has already returned, so the bridge has nothing
        // useful left to do regardless of cancel state.
        bridge.abort();

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

#[cfg(test)]
mod tests {
    //! Regression tests for the kill+resume control loop.
    //!
    //! ISSUE-dispatch-prior-10: a multi-iteration kill+resume cycle must
    //! sum the per-subprocess `TokenUsage` into `total_token_usage` —
    //! double-count, reset-on-restart, or missed-final-iteration would
    //! silently mis-report cost in `meta.json`/`summary.json`. Pin the
    //! accumulation contract here so a future refactor that moves the
    //! `total_token_usage.add(...)` call out of the loop body fails
    //! loudly.
    use super::*;
    use crate::dispatch::layer::LayerState;
    use crate::dispatch::state::ApprovalPolicy;
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::{ChildProcess, ProcessSpawner};
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// Cycles through an ordered list of `FakeScript`s — one per
    /// `spawn` call. Mirrors the `CyclingFake` pattern used in
    /// `runner.rs:1581-1599` so kill+resume tests can give each
    /// iteration a different scripted result.
    struct CyclingFake(Vec<FakeScript>, StdMutex<usize>);

    #[async_trait::async_trait]
    impl ProcessSpawner for CyclingFake {
        async fn spawn(
            &self,
            cmd: SpawnCmd,
        ) -> Result<Box<dyn ChildProcess>, pitboss_core::error::SpawnError> {
            let i = {
                let mut lock = self.1.lock().unwrap();
                let i = *lock;
                *lock += 1;
                i
            };
            let script = self.0[i % self.0.len()].clone();
            FakeSpawner::new(script).spawn(cmd).await
        }
    }

    fn empty_manifest(run_dir: PathBuf) -> ResolvedManifest {
        ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir,
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
            communication: Default::default(),
            lifecycle: None,
        }
    }

    fn stub_cmd() -> SpawnCmd {
        SpawnCmd {
            program: PathBuf::from("claude"),
            args: vec![],
            cwd: PathBuf::from("/"),
            env: HashMap::new(),
        }
    }

    /// One synthetic reprompt → exactly two iterations → `total_token_usage`
    /// is the sum of both per-iteration `TokenUsage` values, and
    /// `reprompt_count == iterations - 1`.
    #[tokio::test]
    async fn kill_resume_accumulates_token_usage_across_iterations() {
        let dir = TempDir::new().unwrap();
        let manifest = empty_manifest(dir.path().to_path_buf());
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));

        // Iteration 1: 10 input + 20 output. Iteration 2: 5 input + 7 output.
        // Both emit `system{init,session_id}` so the reprompt path captures
        // a session_id (required for `--resume`); without that the loop
        // breaks early on the warn-and-exit branch at kill_resume.rs:201.
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"system","subtype":"init","session_id":"sess-1"}"#)
                    .stdout_line(
                        r#"{"type":"result","session_id":"sess-1","usage":{"input_tokens":10,"output_tokens":20}}"#,
                    )
                    .exit_code(0),
                FakeScript::new()
                    .stdout_line(r#"{"type":"system","subtype":"init","session_id":"sess-1"}"#)
                    .stdout_line(
                        r#"{"type":"result","session_id":"sess-1","usage":{"input_tokens":5,"output_tokens":7}}"#,
                    )
                    .exit_code(0),
            ],
            StdMutex::new(0),
        ));

        let layer = Arc::new(LayerState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            ApprovalPolicy::Block,
            None,
            Arc::new(crate::shared_store::SharedStore::new()),
            None,
        ));

        // Pre-queue one reprompt so iteration 1's `try_recv` finds it and
        // the loop continues into iteration 2. A second `try_recv` after
        // iteration 2 returns `Empty`, breaking the loop.
        let (reprompt_tx, reprompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        reprompt_tx.send("[SYSTEM] reconsider".into()).unwrap();
        // Drop the sender so the channel won't accept another send during
        // the test — defensive against a future regression that would
        // mis-trigger an extra iteration.
        drop(reprompt_tx);

        let args = KillResumeArgs {
            actor_id: "lead".into(),
            initial_cmd: stub_cmd(),
            timeout: Duration::from_secs(30),
            log_path: dir.path().join("stdout.log"),
            stderr_path: dir.path().join("stderr.log"),
        };

        let result =
            run_kill_resume_loop(Arc::clone(&layer), args, reprompt_rx, |_sid, _prompt| {
                stub_cmd()
            })
            .await;

        assert_eq!(
            result.reprompt_count, 1,
            "exactly one reprompt → exactly two iterations"
        );
        assert_eq!(
            result.total_token_usage.input, 15,
            "input_tokens must be 10 (iter 1) + 5 (iter 2) = 15; got {}",
            result.total_token_usage.input
        );
        assert_eq!(
            result.total_token_usage.output, 27,
            "output_tokens must be 20 (iter 1) + 7 (iter 2) = 27; got {}",
            result.total_token_usage.output
        );
        assert_eq!(
            result.last_session_id.as_deref(),
            Some("sess-1"),
            "last_session_id must be the most recent iteration's id"
        );
    }

    /// Zero reprompts → exactly one iteration → `total_token_usage`
    /// equals the single iteration's usage. Companion case to the
    /// kill+resume scenario above; pins the no-reprompt path so a
    /// regression that double-adds the per-iteration usage (e.g. by
    /// also accumulating from `final_outcome`) fails here too.
    #[tokio::test]
    async fn kill_resume_with_no_reprompt_returns_single_iteration_usage() {
        let dir = TempDir::new().unwrap();
        let manifest = empty_manifest(dir.path().to_path_buf());
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(FakeSpawner::new(
            FakeScript::new()
                .stdout_line(r#"{"type":"system","subtype":"init","session_id":"sess-only"}"#)
                .stdout_line(
                    r#"{"type":"result","session_id":"sess-only","usage":{"input_tokens":3,"output_tokens":4}}"#,
                )
                .exit_code(0),
        ));

        let layer = Arc::new(LayerState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            ApprovalPolicy::Block,
            None,
            Arc::new(crate::shared_store::SharedStore::new()),
            None,
        ));

        let (reprompt_tx, reprompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        drop(reprompt_tx); // no reprompts queued

        let args = KillResumeArgs {
            actor_id: "lead".into(),
            initial_cmd: stub_cmd(),
            timeout: Duration::from_secs(30),
            log_path: dir.path().join("stdout.log"),
            stderr_path: dir.path().join("stderr.log"),
        };

        let result =
            run_kill_resume_loop(Arc::clone(&layer), args, reprompt_rx, |_sid, _prompt| {
                stub_cmd()
            })
            .await;

        assert_eq!(result.reprompt_count, 0);
        assert_eq!(result.total_token_usage.input, 3);
        assert_eq!(result.total_token_usage.output, 4);
    }
}
