//! Hierarchical dispatch path — one lead subprocess plus dynamically-spawned
//! workers. Reuses most of the flat dispatch plumbing from runner.rs and
//! adds the MCP server lifecycle + lead spawn wiring on top.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, RunSummary, SessionStore};
use uuid::Uuid;

use crate::control::{control_socket_path, server::start_control_server};
use crate::dispatch::state::DispatchState;
use crate::manifest::resolve::ResolvedManifest;
use crate::mcp::{socket_path_for_run, McpServer};

/// Read `summary.jsonl` and return `(records, ids)` where `records` is the
/// list of every successfully-parsed [`TaskRecord`] in append order and
/// `ids` is the set of task_ids present.
///
/// `summary.jsonl` is the source of truth for the run's actor lifecycle:
/// the lead, every sub-lead, and every Done worker (root or sub-tree)
/// have appended a `TaskRecord` here by the time finalize runs. The
/// finalize phase reads this file rather than walking
/// `state.root.workers` (which only sees the root layer — pre-#221 bug)
/// to assemble the canonical `summary.json` aggregate.
///
/// Unparseable lines are logged and skipped (mid-write truncation in
/// in-progress reads, or future format skew). Returns an io error if
/// the file is missing — finalize callers always have the file because
/// the lead's record was appended a few hundred lines above.
async fn read_summary_jsonl_records(
    path: &std::path::Path,
) -> Result<(
    Vec<pitboss_core::store::TaskRecord>,
    std::collections::HashSet<String>,
)> {
    let bytes = tokio::fs::read(path).await.with_context(|| {
        format!(
            "read summary.jsonl for finalize aggregation: {}",
            path.display()
        )
    })?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| anyhow::anyhow!("summary.jsonl is not utf-8: {e}"))?;
    let mut records: Vec<pitboss_core::store::TaskRecord> = Vec::new();
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<pitboss_core::store::TaskRecord>(trimmed) {
            Ok(rec) => {
                ids.insert(rec.task_id.clone());
                records.push(rec);
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    line = trimmed,
                    "summary.jsonl: skipping unparseable line in finalize aggregation"
                );
            }
        }
    }
    Ok((records, ids))
}

// `sublead_sessions`: prior sub-lead session IDs from `subleads.jsonl`,
// read by `build_resume_hierarchical`. Empty for fresh dispatches. When
// non-empty, seeded into the root shared store at `/resume/subleads` so the
// root lead can pass `resume_session_id` to `spawn_sublead`.
#[allow(clippy::too_many_arguments)]
pub async fn run_hierarchical(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
    sublead_sessions: std::collections::HashMap<String, String>,
    pre_minted_run_id: Option<Uuid>,
) -> Result<i32> {
    // Surface headless approval-gate mis-configurations early, before any
    // claude subprocess launches. Runs silently if stdout is a TTY (the
    // operator has the TUI to approve things).
    crate::dispatch::runner::print_headless_warnings_if_applicable(&resolved);

    // Validate the manifest has a `[[lead]]` BEFORE writing any on-disk
    // artifacts. Pre-#150-M9 this check happened after manifest snapshot
    // writes, so a no-lead bail left orphan files in `run_subdir`. Now
    // we fail fast with no side effects.
    let lead = resolved
        .lead
        .as_ref()
        .context("hierarchical mode requires a [[lead]]")?
        .clone();

    if dry_run {
        println!("DRY-RUN lead: {}", lead.id);
        println!(
            "DRY-RUN command: {} --verbose (mcp socket TBD)",
            claude_binary.display()
        );
        return Ok(0);
    }

    // Build the store first since `init_run_state` needs it. Hierarchical
    // resolves its own run_dir from the override (or `resolved.run_dir`)
    // before constructing the JsonFileStore so the store points at the
    // right root.
    let resolved_run_dir = run_dir_override
        .clone()
        .unwrap_or_else(|| resolved.run_dir.clone());
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(resolved_run_dir.clone()));

    // Phase 1 (shared with flat mode via dispatch::entrypoint): mint /
    // honor run_id, snapshot parent run id, write manifest snapshots,
    // initialise RunMeta. The lead validation above already fired —
    // a no-lead manifest never reaches this point and never persists
    // an orphan RunMeta.
    let init = crate::dispatch::entrypoint::init_run_state(
        &resolved,
        &manifest_text,
        &manifest_path,
        claude_version.clone(),
        &store,
        pre_minted_run_id,
        run_dir_override,
    )
    .await?;
    // Bind locals from init so the rest of this function (which references
    // these by short name) doesn't need ~20 inline `init.foo` rewrites.
    // The `meta` snapshot below is reconstructed from init for the
    // `RunSummary` build at finalize time — `init.started_at` is the
    // authoritative dispatch start.
    let run_id = init.run_id;
    let run_subdir = init.run_subdir.clone();
    let run_dir = init.run_dir.clone();

    let cancel = CancelToken::new();
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());

    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());

    // Moved up from later: WorktreeManager and cleanup policy are now needed
    // by DispatchState so the MCP spawn_worker handler can prepare worktrees.
    let wt_mgr = Arc::new(pitboss_core::worktree::WorktreeManager::new());
    let cleanup_policy = match resolved.worktree_cleanup {
        crate::manifest::schema::WorktreeCleanup::Always => {
            pitboss_core::worktree::CleanupPolicy::Always
        }
        crate::manifest::schema::WorktreeCleanup::OnSuccess => {
            pitboss_core::worktree::CleanupPolicy::OnSuccess
        }
        crate::manifest::schema::WorktreeCleanup::Never => {
            pitboss_core::worktree::CleanupPolicy::Never
        }
    };

    // Phase 2 (shared with flat mode): build notification router and fire
    // `RunDispatched` with mode = "hierarchical". The shared helper enforces
    // a single `set_run_subdir` binding + emit-once invariant — past drift
    // between flat and hierarchical here is what #150 M9 was about.
    let notification_router =
        crate::dispatch::entrypoint::build_notification_router_and_emit_dispatched(
            &resolved,
            &manifest_path,
            &init,
            "hierarchical",
        )
        .await?;

    // 1. Start the MCP server.
    let socket = socket_path_for_run(run_id, &run_dir);
    let state = Arc::new(DispatchState::new(
        run_id,
        resolved.clone(),
        store.clone(),
        cancel.clone(),
        lead.id.clone(),
        spawner.clone(),
        claude_binary.clone(),
        wt_mgr.clone(),
        cleanup_policy,
        run_subdir.clone(),
        resolved.default_approval_policy.unwrap_or_default(),
        notification_router.clone(),
        {
            let s = std::sync::Arc::new(crate::shared_store::SharedStore::new());
            s.start_lease_pruner();
            s
        },
    ));
    let mcp = McpServer::start(socket.clone(), state.clone()).await?;

    // Seed /resume/subleads with prior sub-lead session IDs so the root lead
    // can read them on a resume run and pass resume_session_id to spawn_sublead.
    // Skipped on fresh dispatches (sublead_sessions is empty).
    if !sublead_sessions.is_empty() {
        let value = serde_json::to_vec(&sublead_sessions).unwrap_or_default();
        if let Err(e) = state
            .root
            .shared_store
            .set("/resume/subleads", value, "pitboss")
            .await
        {
            tracing::warn!(error = %e, "failed to seed /resume/subleads in shared store");
        }
    }

    // Load declarative approval policy from the manifest into the root layer.
    // Empty approval_rules → no PolicyMatcher is installed (legacy path unchanged).
    if !resolved.approval_rules.is_empty() {
        let matcher = crate::mcp::policy::PolicyMatcher::new(resolved.approval_rules.clone());
        state.root.set_policy_matcher(matcher).await;
    }

    // Install cascade cancel watcher: when root drains, all registered
    // sub-trees and their workers are drained automatically.
    crate::dispatch::signals::install_cascade_cancel_watcher(state.clone());

    // Install approval TTL watcher: scan the approval queue every 250ms
    // and apply fallback actions (auto_reject, auto_approve) to expired entries.
    crate::dispatch::runner::install_approval_ttl_watcher(state.clone());

    // Bind the control socket for TUI ↔ dispatcher ops.
    let control_sock = control_socket_path(run_id, &run_dir);
    let _control = start_control_server(
        control_sock,
        env!("CARGO_PKG_VERSION").to_string(),
        run_id.to_string(),
        "hierarchical".into(),
        state.clone(),
    )
    .await
    .context("start control server")?;

    // 2. Build the --mcp-config file for the lead.
    //
    // Mint a per-actor token for the root lead and embed it in the
    // bridge args. The server uses the token to bind connection identity
    // — defending against direct (non-bridge) socket connections that
    // try to forge `_meta.actor_role: root_lead`. Issue #145.
    let lead_token = state.mint_token(&lead.id, "lead").await;
    let mcp_config_path = run_subdir.join("lead-mcp-config.json");
    write_mcp_config(
        &mcp_config_path,
        &socket,
        &lead.id,
        "lead",
        Some(&lead_token),
        &resolved.mcp_servers,
    )
    .await?;

    // 3. Prepare lead worktree + spawn.
    let mut lead_worktree_handle: Option<pitboss_core::worktree::Worktree> = None;
    let lead_cwd = if lead.use_worktree {
        let name = format!("pitboss-lead-{}-{}", lead.id, run_id);
        match wt_mgr.prepare(&lead.directory, &name, lead.branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                lead_worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                anyhow::bail!("lead worktree prepare failed: {e}");
            }
        }
    } else {
        lead.directory.clone()
    };

    let lead_task_dir = run_subdir.join("tasks").join(&lead.id);
    tokio::fs::create_dir_all(&lead_task_dir).await.ok();
    let lead_log_path = lead_task_dir.join("stdout.log");
    let lead_stderr_path = lead_task_dir.join("stderr.log");

    // Persist the lead's worktree path so the TUI can run mid-flight
    // git-diff against it (same pattern as workers — see mcp/tools.rs).
    if lead.use_worktree {
        let _ = tokio::fs::write(
            lead_task_dir.join("worktree.path"),
            lead_cwd.to_string_lossy().as_bytes(),
        )
        .await;
    }

    // 3b. Wire the reprompt delivery channel for the root lead BEFORE spawning.
    //     `send_synthetic_reprompt` will find this channel when a worker is
    //     killed with reason and the root layer is the target. The receiving
    //     end is consumed by the kill+resume loop below.
    let (reprompt_tx, reprompt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    state.root.set_reprompt_tx(reprompt_tx).await;

    // 3c. Kill+resume loop — shared with spawn_sublead_session via
    //     `crate::dispatch::kill_resume::run_kill_resume_loop`. The closure
    //     captures the lead's env / cwd / spawn-args resolution because root
    //     lead and sub-lead use different builders (`lead_resume_spawn_args`
    //     vs. `sublead_spawn_args`).
    let mut lead_env = lead.env.clone();
    crate::dispatch::runner::apply_pitboss_env_defaults(&mut lead_env, lead.permission_routing);
    let initial_cmd = pitboss_core::process::SpawnCmd {
        program: claude_binary.clone(),
        args: crate::dispatch::runner::lead_spawn_args(&lead, &mcp_config_path),
        cwd: lead_cwd.clone(),
        env: lead_env,
    };

    let kr_result = crate::dispatch::kill_resume::run_kill_resume_loop(
        state.root.clone(),
        crate::dispatch::kill_resume::KillResumeArgs {
            actor_id: lead.id.clone(),
            initial_cmd,
            timeout: std::time::Duration::from_secs(lead.timeout_secs),
            log_path: lead_log_path.clone(),
            stderr_path: lead_stderr_path.clone(),
        },
        reprompt_rx,
        |sid, new_prompt| {
            let mut resume_env = lead.env.clone();
            crate::dispatch::runner::apply_pitboss_env_defaults(
                &mut resume_env,
                lead.permission_routing,
            );
            pitboss_core::process::SpawnCmd {
                program: claude_binary.clone(),
                args: crate::dispatch::runner::lead_resume_spawn_args(
                    &lead,
                    &mcp_config_path,
                    sid,
                    new_prompt,
                ),
                cwd: lead_cwd.clone(),
                env: resume_env,
            }
        },
    )
    .await;

    let final_outcome = kr_result.final_outcome;
    let overall_started_at = kr_result.overall_started_at;
    let total_token_usage = kr_result.total_token_usage;
    let reprompt_count = kr_result.reprompt_count;

    // Close the reprompt channel so further sends fail fast.
    state.root.clear_reprompt_tx().await;

    // Build lead TaskRecord using the accumulated data from all iterations.
    let lead_counters = state
        .root
        .worker_counters
        .read()
        .await
        .get(&state.root.lead_id)
        .cloned()
        .unwrap_or_default();
    // Merge the loop's own reprompt_count into the counter-based one.
    let total_reprompt_count = lead_counters.reprompt_count + reprompt_count;
    // Compute initial status from session state, then reclassify if the
    // root lead exited shortly after a rejected approval (lessons-learned:
    // require_plan_approval = true + auto_reject by policy → lead exits 0
    // → would otherwise be Success). See `mcp::tools::run_worker` for the
    // same pattern.
    let mut lead_status = match final_outcome.final_state {
        pitboss_core::session::SessionState::Completed => pitboss_core::store::TaskStatus::Success,
        pitboss_core::session::SessionState::Failed { .. } => {
            pitboss_core::store::TaskStatus::Failed
        }
        pitboss_core::session::SessionState::TimedOut => pitboss_core::store::TaskStatus::TimedOut,
        pitboss_core::session::SessionState::Cancelled => {
            pitboss_core::store::TaskStatus::Cancelled
        }
        pitboss_core::session::SessionState::SpawnFailed { .. } => {
            pitboss_core::store::TaskStatus::SpawnFailed
        }
        _ => pitboss_core::store::TaskStatus::Failed,
    };
    if matches!(lead_status, pitboss_core::store::TaskStatus::Success) {
        if let Some(crate::dispatch::state::ApprovalTerminationKind::Rejected) =
            state.approval_driven_termination(&lead.id).await
        {
            lead_status = pitboss_core::store::TaskStatus::ApprovalRejected;
        }
    }
    let lead_record = pitboss_core::store::TaskRecord {
        task_id: lead.id.clone(),
        status: lead_status,
        exit_code: final_outcome.exit_code,
        started_at: overall_started_at,
        ended_at: final_outcome.ended_at,
        duration_ms: (final_outcome.ended_at - overall_started_at)
            .num_milliseconds()
            .max(0),
        worktree_path: if lead.use_worktree {
            Some(lead_cwd)
        } else {
            None
        },
        log_path: lead_log_path.clone(),
        token_usage: total_token_usage,
        claude_session_id: final_outcome.claude_session_id,
        final_message_preview: final_outcome.final_message_preview,
        final_message: final_outcome.final_message,
        parent_task_id: None, // lead has no parent
        pause_count: lead_counters.pause_count,
        reprompt_count: total_reprompt_count,
        approvals_requested: lead_counters.approvals_requested,
        approvals_approved: lead_counters.approvals_approved,
        approvals_rejected: lead_counters.approvals_rejected,
        model: Some(lead.model.clone()),
        failure_reason: crate::dispatch::failure_detection::detect_failure_reason(
            final_outcome.exit_code,
            Some(&lead_log_path),
            Some(&lead_stderr_path),
        )
        .map(|r| {
            // #184: when --resume was used and we got an unhelpful
            // Unknown classification, surface a hint about the session
            // id potentially being invalid. Specific markers
            // (RateLimit / AuthFailure / etc.) pass through unchanged.
            if let Some(sid) = lead.resume_session_id.as_deref() {
                pitboss_core::failure_classify::enrich_with_resume_hint(r, sid)
            } else {
                r
            }
        }),
    };

    // Cleanup worktree per policy. Surface the result via tracing
    // instead of swallowing it with `let _` — pre-fix, a worktree that
    // failed to clean up (e.g. a still-running worker subprocess holding
    // an open file inside the lead's tree) left zero diagnostic and the
    // operator only noticed the leak via `pitboss prune`. (#150 L11)
    if let Some(wt) = lead_worktree_handle {
        let succeeded = matches!(lead_record.status, pitboss_core::store::TaskStatus::Success);
        if let Err(e) = wt_mgr.cleanup(wt, cleanup_policy, succeeded) {
            tracing::warn!(
                error = %e,
                "lead worktree cleanup failed — run `pitboss prune` to clean up \
                 leftover .worktrees entries"
            );
        }
    }

    // Persist lead record
    store.append_record(run_id, &lead_record).await?;
    // Broadcast classified failure so the TUI and any attached client see
    // why the root lead died (rate-limit / network / auth / ...).
    // Root-lead dying generally ends the run, but still record into
    // api_health for completeness — any subleads spawned after this point
    // (rare) will see the gate.
    if let Some(reason) = lead_record.failure_reason.clone() {
        state.api_health.record(&reason).await;
        crate::dispatch::failure_detection::broadcast_worker_failed(
            &state.root,
            lead.id.clone(),
            None,
            reason,
            &["root", lead.id.as_str()],
        )
        .await;
    }
    state.root.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Done(lead_record.clone()),
    );
    let _ = state.root.done_tx.send(lead.id.clone());

    // 4. Finalize.
    // Capture the ORIGINAL cancel state BEFORE we call terminate() below.
    // This preserves the distinction between user Ctrl-C (real interruption)
    // and our internal cleanup termination signal to drain workers.
    let was_interrupted = cancel.is_draining() || cancel.is_terminated();

    // Drain in-flight workers across all layers (#150 M6+M7).
    //
    // Pre-fix this block was three lines: terminate root workers, terminate
    // the run cancel, sleep TERMINATE_GRACE. Two problems:
    //
    // 1. (M7) The run-cancel terminate fans out to sub-trees via the
    //    fire-and-forget cascade watchers (`install_cascade_cancel_watcher`,
    //    `install_sublead_cancel_watcher`). Their `tokio::spawn` is not
    //    awaited; under load, scheduling latency can extend past the
    //    sleep deadline and leave sub-tree workers un-signaled while we
    //    classify them.
    // 2. (M6) The fixed `tokio::time::sleep(TERMINATE_GRACE)` ran the
    //    whole window even when workers drained in milliseconds, AND ran
    //    out at the same instant as each `SessionHandle`'s own
    //    SIGTERM-grace timeout — so a worker mid-SIGKILL escalation
    //    could be classified Cancelled while still alive on the host.
    //
    // The new shape:
    //
    // - **Subscribe first.** `done_rx` is taken before we snapshot or
    //   trip cancels. Workers send their task_id to `state.root.done_tx`
    //   on every exit (root layer or sub-tree, see `mcp/tools.rs:704`).
    //   Subscribing first guarantees we don't miss a fast-completing
    //   worker that exits between the snapshot and the wait.
    //
    // - **Snapshot in_flight set.** Every non-Done worker across the
    //   root layer and every sub-tree at this instant. Workers that
    //   transition Done after the snapshot still send a done event we
    //   simply discard (not in our set).
    //
    // - **Synchronous cascade.** `cancel.terminate()` plus the manual
    //   walks through `cascade_to_subleads` / each sub-layer's
    //   `cascade_to_workers` / root's `cascade_to_workers` propagate
    //   the signal through the whole tree without yielding to the
    //   spawned cascade watchers. By the time we start the wait, every
    //   in-flight worker's CancelToken has been tripped.
    //
    // - **Join-with-timeout.** `await_workers_drained` consumes
    //   `done_rx` until either every snapshot id signals or the
    //   deadline fires. Deadline is `TERMINATE_GRACE + 2s` so each
    //   worker's own SessionHandle SIGTERM→SIGKILL escalation has
    //   headroom to complete before we move on; workers that don't
    //   signal within that window are still classified Cancelled below
    //   but those are now the cases where the underlying child was
    //   genuinely stuck (the audit's pre-fix concern was real).
    let mut done_rx = state.root.done_tx.subscribe();

    let mut in_flight: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let workers = state.root.workers.read().await;
        for (id, w) in workers.iter() {
            if id != &lead.id && !matches!(w, crate::dispatch::state::WorkerState::Done(_)) {
                in_flight.insert(id.clone());
            }
        }
    }
    {
        let subleads = state.subleads.read().await;
        for sub in subleads.values() {
            let workers = sub.workers.read().await;
            for (id, w) in workers.iter() {
                if !matches!(w, crate::dispatch::state::WorkerState::Done(_)) {
                    in_flight.insert(id.clone());
                }
            }
        }
    }

    cancel.terminate();
    state.cascade_to_subleads().await;
    {
        let subleads = state.subleads.read().await;
        for sub in subleads.values() {
            sub.cascade_to_workers().await;
        }
    }
    state.root.cascade_to_workers().await;

    let drain_deadline = pitboss_core::session::TERMINATE_GRACE + std::time::Duration::from_secs(2);
    await_workers_drained(&state, in_flight, &mut done_rx, drain_deadline).await;

    // Aggregate the run's TaskRecords from `summary.jsonl`, which is the
    // source of truth: it carries the lead (appended above), every
    // sub-lead's `TaskRecord` (appended at sub-lead exit by
    // `dispatch::sublead`), and every Done worker's record (appended at
    // worker exit by `mcp::tools::spawn::run_worker`). Pre-#221, finalize
    // built `summary.json` from `state.root.workers` only — sub-tree
    // workers live in their sub-lead's `LayerState`, not root, so a
    // 5-actor smoke run produced `tasks_total: 1` in the operational
    // console. Reading the JSONL captures every layer for free.
    let (mut all_records, mut existing_ids) =
        read_summary_jsonl_records(&run_subdir.join("summary.jsonl")).await?;

    // Synthesise Cancelled `TaskRecord`s for every still-in-flight worker
    // across **every layer** (root + each sub-tree). A worker that
    // hasn't appended its own Done record by drain-deadline time gets a
    // synthetic Cancelled so the aggregate has an entry for every actor
    // we know spawned. Filter on `existing_ids` so we don't clobber a
    // worker that DID complete cleanly between the drain wait and our
    // re-read (the JSONL append in `run_worker` happens before the
    // workers-map transitions to `Done`, so a small race window exists
    // where the file has the real Done record but our in-memory scan
    // would still see the worker as Running).
    let now = Utc::now();
    let mut cancelled_records: Vec<pitboss_core::store::TaskRecord> = Vec::new();
    let make_cancelled =
        |id: &str, parent_id: &str, model: Option<String>| pitboss_core::store::TaskRecord {
            task_id: id.to_string(),
            status: pitboss_core::store::TaskStatus::Cancelled,
            exit_code: None,
            started_at: now,
            ended_at: now,
            duration_ms: 0,
            worktree_path: None,
            log_path: run_subdir.join("tasks").join(id).join("stdout.log"),
            token_usage: Default::default(),
            claude_session_id: None,
            final_message_preview: Some("cancelled when lead exited".into()),
            final_message: None,
            parent_task_id: Some(parent_id.to_string()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model,
            failure_reason: None,
        };
    {
        let workers = state.root.workers.read().await;
        let worker_models = state.root.worker_models.read().await;
        for (id, w) in workers.iter() {
            if id == &lead.id || existing_ids.contains(id) {
                continue;
            }
            if !matches!(w, crate::dispatch::state::WorkerState::Done(_)) {
                cancelled_records.push(make_cancelled(
                    id,
                    &lead.id,
                    worker_models.get(id).cloned(),
                ));
            }
        }
    }
    {
        let subleads = state.subleads.read().await;
        for (sublead_id, sub) in subleads.iter() {
            let workers = sub.workers.read().await;
            let worker_models = sub.worker_models.read().await;
            for (id, w) in workers.iter() {
                // Sub-lead layers register the sub-lead itself as a
                // "worker" entry (the claude subprocess). Filter by the
                // layer's lead_id so we don't double-emit a Cancelled
                // record for a sub-lead — its own TaskRecord was
                // already appended at sub-lead exit time.
                if id == &sub.lead_id || existing_ids.contains(id) {
                    continue;
                }
                if !matches!(w, crate::dispatch::state::WorkerState::Done(_)) {
                    cancelled_records.push(make_cancelled(
                        id,
                        sublead_id,
                        worker_models.get(id).cloned(),
                    ));
                }
            }
        }
    }

    for rec in &cancelled_records {
        store.append_record(run_id, rec).await?;
        existing_ids.insert(rec.task_id.clone());
        all_records.push(rec.clone());
    }

    // Workers cancelled because the lead finished cleanly are not failures.
    let lead_succeeded = matches!(lead_record.status, pitboss_core::store::TaskStatus::Success);
    let tasks_failed = all_records
        .iter()
        .filter(|r| {
            if r.task_id == lead.id {
                return !matches!(r.status, pitboss_core::store::TaskStatus::Success);
            }
            if lead_succeeded && matches!(r.status, pitboss_core::store::TaskStatus::Cancelled) {
                false
            } else {
                !matches!(r.status, pitboss_core::store::TaskStatus::Success)
            }
        })
        .count();

    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path,
        manifest_name: resolved.name.clone(),
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at: init.started_at,
        ended_at,
        total_duration_ms: (ended_at - init.started_at).num_milliseconds(),
        tasks_total: all_records.len(),
        tasks_failed,
        was_interrupted,
        tasks: all_records,
    };
    store.finalize_run(&summary).await?;

    // Optional post-mortem dump of shared-store contents.
    if resolved.dump_shared_store {
        let dump_path = run_subdir.join("shared-store.json");
        if let Err(e) = state.root.shared_store.dump_to_path(&dump_path).await {
            tracing::warn!(?e, "shared-store dump failed");
        }
    }

    // Emit RunFinished. Hierarchical mode used to build the notification
    // router and never fire this event — consumers only saw the run via
    // RunDispatched (now) and the per-tool approval traffic. Cost intent
    // (spent_usd) is left at 0.0 here; the lead-spend accounting work
    // (separate roadmap item) will populate it.
    if let Some(router) = &notification_router {
        let env = crate::notify::NotificationEnvelope::new(
            &run_id.to_string(),
            crate::notify::Severity::Info,
            crate::notify::PitbossEvent::RunFinished {
                run_id: run_id.to_string(),
                tasks_total: summary.tasks_total,
                tasks_failed,
                duration_ms: u64::try_from(summary.total_duration_ms).unwrap_or(0),
                spent_usd: 0.0,
            },
            Utc::now(),
        );
        let _ = router.dispatch(env).await;
    }

    // Exit code same as flat dispatch
    let rc = if was_interrupted {
        130
    } else if tasks_failed > 0 {
        1
    } else {
        0
    };

    // #151 M2: deterministic MCP teardown. Awaits per-connection
    // cleanup (lease release, identity slot drain) before the
    // dispatcher returns, so callers observe a fully-quiesced run.
    // Pre-fix this happened in synchronous Drop with no await on
    // `tracker.wait()`, which left detached cleanup tasks racing
    // against the dispatcher's exit.
    mcp.shutdown().await;

    Ok(rc)
}

/// Emit a `--mcp-config` file that tells claude to launch our own pitboss
/// binary as a stdio MCP server, passing the socket path as an argument.
/// `pitboss mcp-bridge --actor-id <id> --actor-role <role> <socket>` proxies
/// bytes between claude's stdio pair and the pitboss MCP server's unix socket,
/// stamping every inbound tool call with the caller's identity.
///
/// This avoids relying on a non-standard `transport: { type: "unix", ... }`
/// Wait for `in_flight` worker task-ids to all signal completion via
/// `state.root.done_tx`, or until `timeout` elapses (#150 M6).
///
/// `done_rx` MUST be subscribed to BEFORE the caller takes the
/// `in_flight` snapshot and trips any cancel signals — otherwise a
/// fast-completing worker that exits between snapshot and subscription
/// would emit its done event into the void and we'd wait the full
/// timeout. Caller's order: subscribe → snapshot → cancel → call this.
///
/// Best-effort. Returns whether all in-flight ids drained within the
/// budget (for tracing); the caller's classification pass below still
/// walks the workers map and marks any non-Done worker as Cancelled
/// regardless. The timeout protects against a worker whose subprocess
/// is genuinely stuck; in that case we accept that the operator-visible
/// summary will list it Cancelled while the host process may still be
/// reaped a few seconds after pitboss exits.
async fn await_workers_drained(
    state: &Arc<DispatchState>,
    mut in_flight: std::collections::HashSet<String>,
    done_rx: &mut tokio::sync::broadcast::Receiver<String>,
    timeout: std::time::Duration,
) -> bool {
    if in_flight.is_empty() {
        return true;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    while !in_flight.is_empty() {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            tracing::warn!(
                remaining = in_flight.len(),
                "worker drain timeout: {} worker(s) did not signal done within {:?}",
                in_flight.len(),
                timeout,
            );
            return false;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, done_rx.recv()).await {
            Err(_) => return false, // outer timeout
            Ok(Ok(id)) => {
                in_flight.remove(&id);
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                // Channel closed (shouldn't normally happen — done_tx is
                // owned by LayerState which outlives this wait). Fall back
                // to a poll: rescan workers map then break.
                refresh_in_flight_from_maps(state, &mut in_flight).await;
                break;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                // Buffer overran — rescan maps to recover the truth.
                refresh_in_flight_from_maps(state, &mut in_flight).await;
            }
        }
    }
    in_flight.is_empty()
}

/// Walk the root workers map plus every sub-tree's workers map and
/// remove any id that has reached `WorkerState::Done` from `in_flight`.
/// Used as a recovery path inside `await_workers_drained` when the
/// `done_tx` broadcast lags or closes — the workers map is the
/// authoritative source so the wait can pick up where the channel
/// left off without missing a transition.
async fn refresh_in_flight_from_maps(
    state: &Arc<DispatchState>,
    in_flight: &mut std::collections::HashSet<String>,
) {
    let mut done_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let workers = state.root.workers.read().await;
        for (id, w) in workers.iter() {
            if matches!(w, crate::dispatch::state::WorkerState::Done(_)) {
                done_ids.insert(id.clone());
            }
        }
    }
    {
        let subleads = state.subleads.read().await;
        for sub in subleads.values() {
            let workers = sub.workers.read().await;
            for (id, w) in workers.iter() {
                if matches!(w, crate::dispatch::state::WorkerState::Done(_)) {
                    done_ids.insert(id.clone());
                }
            }
        }
    }
    in_flight.retain(|id| !done_ids.contains(id));
}

/// field that claude's MCP client may not honor. The generated config uses
/// only the documented `command` + `args` (stdio transport) shape.
/// Build the `mcpServers` JSON object with the pitboss bridge entry plus any
/// operator-declared `[[mcp_server]]` entries from the manifest.
///
/// `token` is the actor's per-connection auth token (minted by
/// `DispatchState::mint_token`). When `Some`, it is appended as
/// `--token <hex>` to the bridge args; the server then validates it and
/// binds the connection identity. Closes #145.
fn build_mcp_servers_json(
    pitboss_exe: &std::path::Path,
    socket: &std::path::Path,
    actor_id: &str,
    actor_role: &str,
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> serde_json::Map<String, serde_json::Value> {
    let mut servers = serde_json::Map::new();
    let mut args: Vec<serde_json::Value> = vec![
        serde_json::Value::String("mcp-bridge".into()),
        serde_json::Value::String("--actor-id".into()),
        serde_json::Value::String(actor_id.to_string()),
        serde_json::Value::String("--actor-role".into()),
        serde_json::Value::String(actor_role.to_string()),
    ];
    if let Some(t) = token {
        args.push(serde_json::Value::String("--token".into()));
        args.push(serde_json::Value::String(t.to_string()));
    }
    args.push(serde_json::Value::String(socket.to_string_lossy().into()));
    servers.insert(
        "pitboss".into(),
        serde_json::json!({
            "command": pitboss_exe.to_string_lossy(),
            "args": args,
        }),
    );
    for s in extra_servers {
        let mut entry = serde_json::Map::new();
        entry.insert("command".into(), s.command.clone().into());
        entry.insert(
            "args".into(),
            serde_json::Value::Array(
                s.args
                    .iter()
                    .map(|a| serde_json::Value::String(a.clone()))
                    .collect(),
            ),
        );
        if !s.env.is_empty() {
            entry.insert("env".into(), serde_json::json!(s.env));
        }
        servers.insert(s.id.clone(), serde_json::Value::Object(entry));
    }
    servers
}

async fn write_mcp_config(
    path: &std::path::Path,
    socket: &std::path::Path,
    actor_id: &str,
    actor_role: &str, // "lead" or "worker"
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> Result<()> {
    // Find the pitboss binary path (the one running us now) so the lead can
    // re-exec the same build for the bridge subcommand.
    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;
    let mcp_servers = build_mcp_servers_json(
        &pitboss_exe,
        socket,
        actor_id,
        actor_role,
        token,
        extra_servers,
    );
    let cfg = serde_json::json!({ "mcpServers": mcp_servers });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    // mcp-config.json carries the actor's auth token (#145). Restrict to the
    // running user so a same-UID-but-different-process attacker can't read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(())
}

/// Emit a worker-scoped `--mcp-config` file. Lists only the 7 shared-store
/// tools — NOT spawn_worker / cancel_worker / wait_for_worker / etc.
/// The bridge command includes the worker's actor_id + actor_role=worker
/// so the dispatcher can identify the caller and enforce namespace authz.
///
/// `token` is the worker's auth token (issue #145). When `Some`, it is
/// embedded so the bridge can prove the connection's identity to the
/// server; when `None`, no token is written (server falls back to
/// rejecting calls without bound identity).
pub async fn write_worker_mcp_config(
    path: &std::path::Path,
    socket: &std::path::Path,
    worker_id: &str,
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> Result<()> {
    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;
    let mcp_servers = build_mcp_servers_json(
        &pitboss_exe,
        socket,
        worker_id,
        "worker",
        token,
        extra_servers,
    );
    let cfg = serde_json::json!({
        "mcpServers": mcp_servers,
        "allowedTools": [
            "mcp__pitboss__kv_get",
            "mcp__pitboss__kv_set",
            "mcp__pitboss__kv_cas",
            "mcp__pitboss__kv_list",
            "mcp__pitboss__kv_wait",
            "mcp__pitboss__lease_acquire",
            "mcp__pitboss__lease_release"
        ]
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    // Same 0o600 hardening as write_mcp_config: keeps the embedded token
    // unreadable to other local users (issue #145).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(())
}

/// Emit a sublead-scoped `--mcp-config` file. Lists only the SUBLEAD_MCP_TOOLS
/// (no spawn_sublead, no wait_for_sublead — depth-2 cap enforced).
/// The bridge command includes the sublead's actor_id + actor_role=sublead
/// so the dispatcher can identify the caller and enforce namespace authz.
///
/// `token` is the sublead's auth token (issue #145). When `Some`, it is
/// embedded into the bridge args.
pub async fn build_sublead_mcp_config(
    sublead_id: &str,
    socket: &std::path::Path,
    run_subdir: &std::path::Path,
    token: Option<&str>,
    extra_servers: &[crate::manifest::schema::McpServerSpec],
) -> Result<PathBuf> {
    use crate::dispatch::runner::SUBLEAD_MCP_TOOLS;

    let pitboss_exe =
        std::env::current_exe().context("resolve current exe for mcp-bridge subcommand")?;
    let mcp_servers = build_mcp_servers_json(
        &pitboss_exe,
        socket,
        sublead_id,
        "sublead",
        token,
        extra_servers,
    );
    let cfg = serde_json::json!({
        "mcpServers": mcp_servers,
        "allowedTools": SUBLEAD_MCP_TOOLS.iter().collect::<Vec<_>>()
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;

    // Ensure run_subdir exists. In production runner.rs creates it before any
    // sublead spawns, but test harnesses build DispatchState without creating
    // the directory, so this create_dir_all is a defensive no-op in production
    // and a required step in tests.
    tokio::fs::create_dir_all(run_subdir).await?;
    let mcp_config_path = run_subdir.join(format!("sublead-{sublead_id}-mcp-config.json"));
    tokio::fs::write(&mcp_config_path, bytes).await?;
    // Same 0o600 hardening as the lead/worker config writers (#145).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            tokio::fs::set_permissions(&mcp_config_path, std::fs::Permissions::from_mode(0o600))
                .await;
    }
    Ok(mcp_config_path)
}

#[cfg(test)]
mod await_drained_tests {
    use super::*;
    use crate::dispatch::state::{ApprovalPolicy, WorkerState};
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use pitboss_core::process::TokioSpawner;
    use pitboss_core::store::JsonFileStore;
    use pitboss_core::worktree::CleanupPolicy;
    use std::collections::HashSet;
    use std::time::Duration;
    use tempfile::TempDir;

    fn mk_state() -> (TempDir, Arc<DispatchState>) {
        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: None,
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
        let wt_mgr = Arc::new(pitboss_core::worktree::WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead-1".into(),
            spawner,
            PathBuf::from("/bin/false"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::AutoApprove,
            None,
            Arc::new(crate::shared_store::SharedStore::new()),
        ));
        (dir, state)
    }

    /// Empty `in_flight` set returns true immediately without waiting.
    #[tokio::test]
    async fn drain_returns_true_immediately_for_empty_set() {
        let (_dir, state) = mk_state();
        let mut rx = state.root.done_tx.subscribe();
        let in_flight: HashSet<String> = HashSet::new();
        let start = tokio::time::Instant::now();
        let drained =
            await_workers_drained(&state, in_flight, &mut rx, Duration::from_secs(5)).await;
        assert!(drained);
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    /// All ids signaling done before deadline returns true and does NOT
    /// run the full timeout — proves the wait is event-driven, not a
    /// fixed sleep.
    #[tokio::test]
    async fn drain_returns_true_when_all_workers_signal_before_deadline() {
        let (_dir, state) = mk_state();
        let mut rx = state.root.done_tx.subscribe();
        let in_flight: HashSet<String> = ["w1", "w2", "w3"].into_iter().map(String::from).collect();

        // Spawn a producer that emits done events for each worker.
        let tx = state.root.done_tx.clone();
        tokio::spawn(async move {
            for id in ["w1", "w2", "w3"] {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let _ = tx.send(id.to_string());
            }
        });

        let start = tokio::time::Instant::now();
        let drained =
            await_workers_drained(&state, in_flight, &mut rx, Duration::from_secs(10)).await;
        let elapsed = start.elapsed();
        assert!(drained, "all workers should have drained");
        assert!(
            elapsed < Duration::from_millis(500),
            "wait should return promptly after the last done event, took {:?}",
            elapsed,
        );
    }

    /// A worker that never signals done causes the wait to time out
    /// and return false. Pre-fix this case was a fixed
    /// `tokio::time::sleep(TERMINATE_GRACE)` that classified the
    /// stuck worker Cancelled regardless; the new contract returns
    /// `false` so the caller can log the survivors before classifying.
    #[tokio::test]
    async fn drain_returns_false_when_worker_never_signals() {
        let (_dir, state) = mk_state();
        let mut rx = state.root.done_tx.subscribe();
        let in_flight: HashSet<String> = ["never-drains".to_string()].into_iter().collect();
        let start = tokio::time::Instant::now();
        let drained =
            await_workers_drained(&state, in_flight, &mut rx, Duration::from_millis(150)).await;
        let elapsed = start.elapsed();
        assert!(!drained, "stuck worker should produce false");
        assert!(
            elapsed >= Duration::from_millis(140) && elapsed < Duration::from_millis(400),
            "wait should fire close to the deadline, took {:?}",
            elapsed,
        );
    }

    /// Done events for ids not in the in_flight set are harmless —
    /// they're discarded. This pins the "subscribe before snapshot"
    /// race tolerance: a worker that completes between subscribe and
    /// snapshot is in the channel buffer but not in the set; the
    /// loop just drops the event and keeps waiting for the right ids.
    #[tokio::test]
    async fn drain_ignores_done_events_for_unknown_ids() {
        let (_dir, state) = mk_state();
        let mut rx = state.root.done_tx.subscribe();
        let in_flight: HashSet<String> = ["target-1".to_string(), "target-2".to_string()]
            .into_iter()
            .collect();

        let tx = state.root.done_tx.clone();
        tokio::spawn(async move {
            // Emit some unrelated events first.
            let _ = tx.send("noise-a".to_string());
            let _ = tx.send("noise-b".to_string());
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = tx.send("target-1".to_string());
            let _ = tx.send("target-2".to_string());
        });

        let drained =
            await_workers_drained(&state, in_flight, &mut rx, Duration::from_secs(2)).await;
        assert!(drained);
    }

    /// `refresh_in_flight_from_maps` removes ids whose `WorkerState`
    /// is `Done` in either the root layer or any sub-tree. This is
    /// the recovery path the wait loop falls back to when the
    /// broadcast lags.
    #[tokio::test]
    async fn refresh_in_flight_drops_done_ids() {
        let (_dir, state) = mk_state();
        // Insert a Done worker on root and a Running worker on root.
        {
            let mut workers = state.root.workers.write().await;
            workers.insert(
                "done-root".into(),
                WorkerState::Done(pitboss_core::store::TaskRecord {
                    task_id: "done-root".into(),
                    status: pitboss_core::store::TaskStatus::Success,
                    exit_code: Some(0),
                    started_at: chrono::Utc::now(),
                    ended_at: chrono::Utc::now(),
                    duration_ms: 0,
                    worktree_path: None,
                    log_path: PathBuf::from("/dev/null"),
                    token_usage: Default::default(),
                    claude_session_id: None,
                    final_message_preview: None,
                    final_message: None,
                    parent_task_id: None,
                    pause_count: 0,
                    reprompt_count: 0,
                    approvals_requested: 0,
                    approvals_approved: 0,
                    approvals_rejected: 0,
                    model: None,
                    failure_reason: None,
                }),
            );
            workers.insert("running-root".into(), WorkerState::Pending);
        }
        let mut in_flight: HashSet<String> = ["done-root", "running-root", "ghost"]
            .into_iter()
            .map(String::from)
            .collect();
        refresh_in_flight_from_maps(&state, &mut in_flight).await;
        assert!(
            !in_flight.contains("done-root"),
            "Done id should be removed"
        );
        assert!(
            in_flight.contains("running-root"),
            "Running id should remain"
        );
        assert!(
            in_flight.contains("ghost"),
            "id not in any layer should remain (caller's snapshot)"
        );
    }
}

#[cfg(test)]
mod summary_jsonl_aggregation_tests {
    use super::*;
    use pitboss_core::store::{TaskRecord, TaskStatus};
    use tempfile::TempDir;

    fn rec(task_id: &str, parent: Option<&str>, status: TaskStatus) -> TaskRecord {
        TaskRecord {
            task_id: task_id.into(),
            status,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 1,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: Default::default(),
            claude_session_id: None,
            final_message_preview: None,
            final_message: None,
            parent_task_id: parent.map(String::from),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
        }
    }

    /// #221 regression: a hierarchical run's `summary.jsonl` carries the
    /// lead, every sub-lead, and every Done worker (root or sub-tree)
    /// across all layers. `read_summary_jsonl_records` must aggregate
    /// every parseable line in append order so the finalize phase can
    /// build a `summary.json` with the full hierarchy.
    #[tokio::test]
    async fn read_summary_jsonl_aggregates_lead_subleads_and_workers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("summary.jsonl");
        // Append order mirrors what production writes:
        // worker exits → sub-lead exits → lead exits.
        let lines = [
            rec("worker-1", Some("sublead-A"), TaskStatus::Success),
            rec("sublead-A", Some("smoke-lead"), TaskStatus::Success),
            rec("worker-2", Some("sublead-B"), TaskStatus::Success),
            rec("sublead-B", Some("smoke-lead"), TaskStatus::Success),
            rec("smoke-lead", None, TaskStatus::Success),
        ];
        let payload: String = lines
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
            .collect();
        tokio::fs::write(&path, payload).await.unwrap();

        let (records, ids) = read_summary_jsonl_records(&path).await.unwrap();
        assert_eq!(records.len(), 5, "all 5 actors present");
        assert_eq!(ids.len(), 5);
        for actor in [
            "smoke-lead",
            "sublead-A",
            "sublead-B",
            "worker-1",
            "worker-2",
        ] {
            assert!(ids.contains(actor), "missing actor {actor}");
        }
        // First-seen order is preserved (matters when callers want a
        // chronological / append-order rendering).
        assert_eq!(records[0].task_id, "worker-1");
        assert_eq!(records[4].task_id, "smoke-lead");
    }

    /// Unparseable lines (mid-write truncation, future format skew)
    /// are skipped; the surrounding records still land.
    #[tokio::test]
    async fn read_summary_jsonl_skips_unparseable_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("summary.jsonl");
        let mut payload = String::new();
        payload.push_str(&format!(
            "{}\n",
            serde_json::to_string(&rec("a", None, TaskStatus::Success)).unwrap()
        ));
        payload.push_str("{\"task_id\":\"truncated\",\"sta\n"); // mid-write
        payload.push('\n'); // empty line
        payload.push_str("not even json\n");
        payload.push_str(&format!(
            "{}\n",
            serde_json::to_string(&rec("b", None, TaskStatus::Failed)).unwrap()
        ));
        tokio::fs::write(&path, payload).await.unwrap();

        let (records, ids) = read_summary_jsonl_records(&path).await.unwrap();
        assert_eq!(records.len(), 2, "two valid records survive");
        assert_eq!(records[0].task_id, "a");
        assert_eq!(records[1].task_id, "b");
        assert_eq!(ids.len(), 2);
    }

    /// Empty file (race: read before any record was appended) returns
    /// an empty aggregate without erroring.
    #[tokio::test]
    async fn read_summary_jsonl_empty_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("summary.jsonl");
        tokio::fs::write(&path, "").await.unwrap();
        let (records, ids) = read_summary_jsonl_records(&path).await.unwrap();
        assert!(records.is_empty());
        assert!(ids.is_empty());
    }

    /// Missing file is an error — finalize callers always have the
    /// file because the lead's record was appended a few hundred
    /// lines above the read site.
    #[tokio::test]
    async fn read_summary_jsonl_missing_file_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.jsonl");
        let err = read_summary_jsonl_records(&path).await.unwrap_err();
        assert!(
            err.to_string().contains("read summary.jsonl"),
            "context attached: {err}"
        );
    }
}
