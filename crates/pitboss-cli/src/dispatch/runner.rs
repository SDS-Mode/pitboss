#![allow(clippy::large_futures, clippy::needless_pass_by_value)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use pitboss_core::process::{ProcessSpawner, SpawnCmd, TokioSpawner};
use pitboss_core::session::{CancelToken, SessionHandle};
use pitboss_core::store::{
    JsonFileStore, RunMeta, RunSummary, SessionStore, TaskRecord, TaskStatus,
};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use crate::manifest::resolve::{ResolvedManifest, ResolvedTask};

/// Seed an env map with pitboss's own defaults for spawned claude subprocesses.
/// Currently: set `CLAUDE_CODE_ENTRYPOINT=sdk-ts` if not already present.
///
/// Rationale: pitboss is the external permission authority (via
/// `approval_policy`, `[[approval_policy]]` rules, and the TUI). Claude's
/// own interactive permission gate is redundant under pitboss orchestration
/// and causes silent stalls in headless dispatch when no operator is
/// present to answer each prompt. The `sdk-ts` value tells claude "you're
/// running under an SDK runtime that manages permissions externally."
///
/// **Companion flag**: every pitboss-spawned claude (lead, sub-lead, worker)
/// also launches with `--dangerously-skip-permissions`. Together the env
/// var and the flag close the full permission surface (MCP tools, file I/O,
/// bash-with-`$VAR`, bash-with-`&&`). See `lead_spawn_args` for the
/// detailed trust-model writeup.
///
/// Operators who want claude's own gate back for a specific actor can
/// override `CLAUDE_CODE_ENTRYPOINT` via `[defaults.env]`, `[lead.env]`,
/// `[[task]].env`, or the `env` field on `spawn_sublead` — but the
/// `--dangerously-skip-permissions` CLI flag is set unconditionally and
/// is not env-overridable. Operators who need the claude gate fully back
/// should not use pitboss's headless dispatch.
///
/// See `docs/superpowers/specs/2026-04-20-path-b-permission-prompt-routing-pin.md`
/// for the deferred alternative (routing claude's gate through pitboss's
/// approval queue rather than bypassing it).
pub fn apply_pitboss_env_defaults(
    env: &mut std::collections::HashMap<String, String>,
    permission_routing: crate::manifest::schema::PermissionRouting,
) {
    use crate::manifest::schema::PermissionRouting;
    match permission_routing {
        PermissionRouting::PathA => {
            // Path A (default): sdk-ts entrypoint bypasses claude's built-in gate.
            env.entry("CLAUDE_CODE_ENTRYPOINT".to_string())
                .or_insert_with(|| "sdk-ts".to_string());
        }
        PermissionRouting::PathB => {
            // Path B: leave the entrypoint unset so claude's gate is active.
            // Pitboss registers `permission_prompt` MCP tool to intercept checks.
            // Do NOT set sdk-ts — that would bypass the gate we want to route.
        }
    }
}

/// Check whether a manifest has approval gates that will block indefinitely
/// in a headless (no-TUI) dispatch. Returns warning strings describing each
/// gate; empty if the manifest can run cleanly headless. Callers typically
/// print the warnings to stderr at dispatch startup.
///
/// Gates checked:
/// - `[run].require_plan_approval = true` → lead's `propose_plan` will hang
///   on the operator queue with no TUI to respond.
/// - `[run].approval_policy = "block"` (or unset — the default) → unmatched
///   `request_approval` calls will sit in the queue.
/// - Any `[[approval_policy]]` rule with `action = "block"` → policy-matched
///   approvals will force into the queue.
///
/// The warning is orthogonal to the Path A env-default fix — Path A stops
/// claude's OWN permission gate from firing, but pitboss's own approval
/// layer is still operator-driven by default. An operator expecting headless
/// dispatch should set `approval_policy = "auto_approve"` (or use
/// `[[approval_policy]]` rules with `ttl_secs` + `fallback`).
pub fn headless_approval_gate_warnings(manifest: &ResolvedManifest) -> Vec<String> {
    let mut warnings = Vec::new();

    if manifest.require_plan_approval {
        warnings.push(
            "`[run].require_plan_approval = true` — lead will hang on `propose_plan` \
             with no TUI to approve. Set `false` or add a `ttl_secs` + `fallback` \
             on the `propose_plan` call for headless use."
                .to_string(),
        );
    }

    // `approval_policy` defaults to `Block` when unset, so both None and
    // Some(Block) trigger the warning.
    let policy_blocks = matches!(
        manifest.approval_policy,
        None | Some(crate::dispatch::state::ApprovalPolicy::Block)
    );
    if policy_blocks {
        warnings.push(
            "`[run].approval_policy` is `block` (or unset — defaults to block) — \
             unmatched `request_approval` / `propose_plan` calls will sit in \
             the operator queue. Set `auto_approve` or `auto_reject` for \
             headless use."
                .to_string(),
        );
    }

    let blocking_rule_count = manifest
        .approval_rules
        .iter()
        .filter(|r| matches!(r.action, crate::mcp::policy::ApprovalAction::Block))
        .count();
    if blocking_rule_count > 0 {
        warnings.push(format!(
            "{} `[[approval_policy]]` rule(s) use `action = \"block\"` — matched \
             approvals will force into the operator queue. Change to \
             `auto_approve` / `auto_reject`, or attach a TUI.",
            blocking_rule_count
        ));
    }

    warnings
}

/// Emit headless-approval-gate warnings to stderr when stdout is not a
/// terminal. Operators running pitboss interactively won't see spurious
/// warnings; headless operators see them prominently before any claude
/// subprocess launches.
pub fn print_headless_warnings_if_applicable(manifest: &ResolvedManifest) {
    if atty::is(atty::Stream::Stdout) {
        return;
    }
    let warnings = headless_approval_gate_warnings(manifest);
    if warnings.is_empty() {
        return;
    }
    eprintln!(
        "pitboss: WARNING — dispatching without a TUI surface but the manifest \
         has approval gates that will block:"
    );
    for w in &warnings {
        eprintln!("  - {}", w);
    }
    eprintln!(
        "See https://sds-mode.github.io/pitboss/operator-guide/approvals.html \
         for the headless approval patterns."
    );
}

fn cleanup_policy_from(w: crate::manifest::schema::WorktreeCleanup) -> CleanupPolicy {
    match w {
        crate::manifest::schema::WorktreeCleanup::Always => CleanupPolicy::Always,
        crate::manifest::schema::WorktreeCleanup::OnSuccess => CleanupPolicy::OnSuccess,
        crate::manifest::schema::WorktreeCleanup::Never => CleanupPolicy::Never,
    }
}

/// Public entry — main.rs calls this. Constructs production spawner + store.
pub async fn run_dispatch_inner(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32> {
    // Flat manifests rarely use `[[approval_policy]]` or `propose_plan`,
    // but if they do, the same headless-gate warnings apply. Silent on TTY.
    print_headless_warnings_if_applicable(&resolved);

    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.clone()));

    execute(
        resolved,
        manifest_text,
        manifest_path,
        claude_binary,
        claude_version,
        spawner,
        store,
        dry_run,
    )
    .await
}

/// Inner workhorse — takes its dependencies injected for testability.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    spawner: Arc<dyn ProcessSpawner>,
    store: Arc<dyn SessionStore>,
    dry_run: bool,
) -> Result<i32> {
    let run_id = Uuid::now_v7();

    let run_dir = resolved.run_dir.clone();
    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), &manifest_text).await?;
    if let Ok(b) = serde_json::to_vec_pretty(&resolved) {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }

    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.clone(),
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at: Utc::now(),
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    if dry_run {
        for t in &resolved.tasks {
            println!(
                "DRY-RUN {}: {} {}",
                t.id,
                claude_binary.display(),
                spawn_args(t).join(" ")
            );
        }
        return Ok(0);
    }

    let is_tty = atty::is(atty::Stream::Stdout);
    let table = Arc::new(Mutex::new(crate::tui_table::ProgressTable::new(is_tty)));
    for t in &resolved.tasks {
        table.lock().await.register(&t.id);
    }

    let semaphore = Arc::new(Semaphore::new(resolved.max_parallel as usize));
    let cancel = CancelToken::new();
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let records: Arc<Mutex<Vec<TaskRecord>>> = Arc::new(Mutex::new(Vec::new()));
    // Tracks whether the cancel drain was triggered by halt_on_failure logic
    // (not by a user signal), so was_interrupted is not set in that case.
    let halt_drained: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // Build notification router if manifest has any [[notification]] sections.
    let notification_router = if !resolved.notifications.is_empty() {
        let http = std::sync::Arc::new(reqwest::Client::new());
        let sinks: Vec<_> = resolved
            .notifications
            .iter()
            .enumerate()
            .map(|(idx, cfg)| {
                let sink = crate::notify::sinks::build(cfg, idx, &http)
                    .context("build notification sink")?;
                let filter = crate::notify::SinkFilter::from(cfg);
                Ok::<_, anyhow::Error>((sink, filter))
            })
            .collect::<Result<_>>()?;
        Some(std::sync::Arc::new(crate::notify::NotificationRouter::new(
            sinks,
        )))
    } else {
        None
    };

    // Build a minimal DispatchState so the control server has something to bind
    // against. Flat mode has no lead and no spawn_worker path, but cancel and
    // list_workers still apply.
    let notification_router_for_emit = notification_router.clone();
    let shared_store = Arc::new(crate::shared_store::SharedStore::new());
    shared_store.start_lease_pruner();
    let flat_state = Arc::new(crate::dispatch::state::DispatchState::new(
        run_id,
        resolved.clone(),
        store.clone(),
        cancel.clone(),
        "".into(),
        spawner.clone(),
        claude_binary.clone(),
        wt_mgr.clone(),
        cleanup_policy_from(resolved.worktree_cleanup),
        run_subdir.clone(),
        resolved.approval_policy.unwrap_or_default(),
        notification_router,
        shared_store.clone(),
    ));
    let control_sock = crate::control::control_socket_path(run_id, &run_dir);
    let _control = crate::control::server::start_control_server(
        control_sock,
        env!("CARGO_PKG_VERSION").to_string(),
        run_id.to_string(),
        "flat".into(),
        flat_state,
    )
    .await
    .context("start control server")?;

    let mut handles = Vec::new();

    for task in resolved.tasks.clone() {
        if cancel.is_draining() {
            break;
        }
        let permit = semaphore.clone().acquire_owned().await?;
        // Re-check after potentially blocking on the semaphore.
        if cancel.is_draining() {
            break;
        }
        let spawner = spawner.clone();
        let store = store.clone();
        let cancel = cancel.clone();
        let claude = claude_binary.clone();
        let records = records.clone();
        let wt_mgr = wt_mgr.clone();
        let halt_on_failure = resolved.halt_on_failure;
        let run_dir = resolved.run_dir.clone();
        let cleanup_policy = match resolved.worktree_cleanup {
            crate::manifest::schema::WorktreeCleanup::Always => CleanupPolicy::Always,
            crate::manifest::schema::WorktreeCleanup::OnSuccess => CleanupPolicy::OnSuccess,
            crate::manifest::schema::WorktreeCleanup::Never => CleanupPolicy::Never,
        };
        let table = table.clone();
        let halt_drained = halt_drained.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let record = execute_task(
                &task,
                &claude,
                spawner,
                cancel.clone(),
                wt_mgr,
                cleanup_policy,
                run_id,
                run_dir,
                table.clone(),
            )
            .await;
            let failed = !matches!(record.status, TaskStatus::Success);
            table.lock().await.mark_done(&record);
            // Incrementally append to summary.jsonl so a mid-run kill still
            // leaves the completed tasks on disk (spec §5.3 invariant).
            if let Err(e) = store.append_record(run_id, &record).await {
                tracing::warn!(task_id = %record.task_id, error = %e, "append_record failed");
            }
            records.lock().await.push(record);
            if failed && halt_on_failure {
                halt_drained.store(true, Ordering::Relaxed);
                cancel.drain();
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let records = Arc::try_unwrap(records)
        .map_err(|_| anyhow::anyhow!("records locked"))?
        .into_inner();
    let tasks_failed = records
        .iter()
        .filter(|r| !matches!(r.status, TaskStatus::Success))
        .count();

    let started_at = meta.started_at;
    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path: manifest_path.clone(),
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at,
        ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total: records.len(),
        tasks_failed,
        was_interrupted: (cancel.is_draining() || cancel.is_terminated())
            && !halt_drained.load(Ordering::Relaxed),
        tasks: records,
    };
    store.finalize_run(&summary).await?;

    // Optional post-mortem dump of shared-store contents.
    if resolved.dump_shared_store {
        let dump_path = run_subdir.join("shared-store.json");
        if let Err(e) = shared_store.dump_to_path(&dump_path).await {
            tracing::warn!(?e, "shared-store dump failed");
        }
    }

    // Emit RunFinished event if notification router is configured.
    if let Some(router) = notification_router_for_emit {
        let env = crate::notify::NotificationEnvelope::new(
            &run_id.to_string(),
            crate::notify::Severity::Info,
            crate::notify::PitbossEvent::RunFinished {
                run_id: run_id.to_string(),
                tasks_total: summary.tasks_total,
                tasks_failed,
                duration_ms: summary.total_duration_ms as u64,
                spent_usd: 0.0,
            },
            Utc::now(),
        );
        let _ = router.dispatch(env).await;
    }

    let rc = if cancel.is_terminated() {
        130
    } else if tasks_failed > 0 {
        1
    } else {
        0
    };
    Ok(rc)
}

#[allow(clippy::too_many_arguments)]
async fn execute_task(
    task: &ResolvedTask,
    claude: &Path,
    spawner: Arc<dyn ProcessSpawner>,
    cancel: CancelToken,
    wt_mgr: Arc<WorktreeManager>,
    cleanup: CleanupPolicy,
    run_id: Uuid,
    run_dir: PathBuf,
    table: Arc<Mutex<crate::tui_table::ProgressTable>>,
) -> TaskRecord {
    let task_dir = run_dir
        .join(run_id.to_string())
        .join("tasks")
        .join(&task.id);
    tokio::fs::create_dir_all(&task_dir).await.ok();
    let log_path = task_dir.join("stdout.log");
    let stderr_log_path = task_dir.join("stderr.log");

    // Worktree preparation (optional).
    let mut worktree_handle = None;
    let cwd = if task.use_worktree {
        let name = format!("pitboss-{}-{}", task.id, run_id);
        match wt_mgr.prepare(&task.directory, &name, task.branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                return TaskRecord {
                    task_id: task.id.clone(),
                    status: TaskStatus::SpawnFailed,
                    exit_code: None,
                    started_at: Utc::now(),
                    ended_at: Utc::now(),
                    duration_ms: 0,
                    worktree_path: None,
                    log_path,
                    token_usage: Default::default(),
                    claude_session_id: None,
                    final_message_preview: Some(format!("worktree error: {e}")),
                    parent_task_id: None,
                    pause_count: 0,
                    reprompt_count: 0,
                    approvals_requested: 0,
                    approvals_approved: 0,
                    approvals_rejected: 0,
                    model: Some(task.model.clone()),
                    failure_reason: None,
                };
            }
        }
    } else {
        task.directory.clone()
    };

    let mut cmd_env = task.env.clone();
    apply_pitboss_env_defaults(&mut cmd_env, Default::default());
    let cmd = SpawnCmd {
        program: claude.to_path_buf(),
        args: spawn_args(task),
        cwd: cwd.clone(),
        env: cmd_env,
    };

    table.lock().await.mark_running(&task.id);

    let outcome = SessionHandle::new(task.id.clone(), spawner, cmd)
        .with_log_path(log_path.clone())
        .with_stderr_log_path(stderr_log_path.clone())
        .run_to_completion(cancel, Duration::from_secs(task.timeout_secs))
        .await;

    let status = match outcome.final_state {
        pitboss_core::session::SessionState::Completed => TaskStatus::Success,
        pitboss_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
        pitboss_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
        pitboss_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
        pitboss_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
        _ => TaskStatus::Failed,
    };

    // Cleanup worktree.
    if let Some(wt) = worktree_handle {
        let succeeded = matches!(status, TaskStatus::Success);
        let _ = wt_mgr.cleanup(wt, cleanup, succeeded);
    }

    let worktree_path = if task.use_worktree { Some(cwd) } else { None };
    let failure_reason = crate::dispatch::failure_detection::detect_failure_reason(
        outcome.exit_code,
        Some(&log_path),
        Some(&stderr_log_path),
    );
    TaskRecord {
        task_id: task.id.clone(),
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
        parent_task_id: None,
        pause_count: 0,
        reprompt_count: 0,
        approvals_requested: 0,
        approvals_approved: 0,
        approvals_rejected: 0,
        model: Some(task.model.clone()),
        failure_reason,
    }
}

/// Claude CLI flags every pitboss-spawned subprocess gets for plugin /
/// skill isolation. Without these, the operator's `~/.claude/`
/// settings (skills, MCP servers, plugins, agents, hooks registered
/// by the human's interactive claude setup) bleed into pitboss-
/// spawned claude processes — observable during v2.1 dispatch testing
/// as workers invoking `Skill{superpowers:brainstorming}` as their
/// very first tool call, following the skill's "explore and ask
/// questions first" rubric, and exiting without producing any
/// content despite `Success exit=0`.
///
/// - `--strict-mcp-config` — only load MCP servers from the
///   `--mcp-config` file we generate, ignore user-level MCP config
/// - `--disable-slash-commands` — disable all skills (slash commands)
///   registered at user level, including operator-installed plugins
///   like `superpowers`. Pitboss MCP tools are unaffected (they come
///   via `--mcp-config`, not the slash-command registry).
///
/// We don't use `--bare` even though it would be more thorough,
/// because `--bare` forces `ANTHROPIC_API_KEY` auth and disables
/// keychain reads — many operators auth via keychain/OAuth and would
/// need explicit key management for pitboss runs.
///
/// Applied uniformly to `spawn_args` (flat tasks), `lead_spawn_args`,
/// `lead_resume_spawn_args`, `sublead_spawn_args`, and
/// `crate::mcp::tools::worker_spawn_args`. Any new spawn-args site
/// MUST include the same two flags; there is a regression test
/// (`every_spawn_variant_has_plugin_isolation_flags`) that asserts
/// this.
fn spawn_args(task: &ResolvedTask) -> Vec<String> {
    // claude CLI requires --verbose when combining -p (print mode) with
    // --output-format stream-json. Without it, claude rejects the invocation
    // with "When using --print, --output-format=stream-json requires --verbose".
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        // Permission authority is pitboss (see lead_spawn_args doc).
        "--dangerously-skip-permissions".into(),
        // Plugin/skill isolation (see lead_spawn_args doc).
        "--strict-mcp-config".into(),
        "--disable-slash-commands".into(),
    ];
    if !task.tools.is_empty() {
        args.push("--allowedTools".into());
        args.push(task.tools.join(","));
    }
    args.push("--model".into());
    args.push(task.model.clone());
    if let Some(sess) = &task.resume_session_id {
        args.push("--resume".into());
        args.push(sess.clone());
    }
    args.push("-p".into());
    args.push(task.prompt.clone());
    args
}

/// MCP tool names the lead needs permission to call. Pre-approved via the
/// lead's `--allowedTools` flag so claude never stalls at the interactive
/// permission prompt (which can't be answered in `-p` non-interactive mode).
/// Format: `mcp__<server-name>__<tool>`, where `pitboss` is the server name
/// we emit in `write_mcp_config`.
pub const PITBOSS_MCP_TOOLS: &[&str] = &[
    // Worker orchestration tools (v0.3+).
    "mcp__pitboss__spawn_worker",
    "mcp__pitboss__worker_status",
    "mcp__pitboss__wait_for_worker",
    "mcp__pitboss__wait_for_any",
    "mcp__pitboss__wait_actor",
    "mcp__pitboss__list_workers",
    "mcp__pitboss__cancel_worker",
    "mcp__pitboss__pause_worker",
    "mcp__pitboss__continue_worker",
    "mcp__pitboss__request_approval",
    "mcp__pitboss__reprompt_worker",
    "mcp__pitboss__propose_plan",
    // Shared-store tools (v0.5+). Leads can read/write the per-run
    // coordination surface alongside workers; without these in the
    // allowlist, claude stalls at the permission prompt the first time
    // the lead tries kv_set / lease_acquire / etc.
    "mcp__pitboss__kv_get",
    "mcp__pitboss__kv_set",
    "mcp__pitboss__kv_cas",
    "mcp__pitboss__kv_list",
    "mcp__pitboss__kv_wait",
    "mcp__pitboss__lease_acquire",
    "mcp__pitboss__lease_release",
    // Run-global leases (v0.6+). For cross-sub-tree resource coordination
    // (e.g., serializing access to an operator-facing filesystem dir).
    "mcp__pitboss__run_lease_acquire",
    "mcp__pitboss__run_lease_release",
];

/// Sub-lead tools: all root-lead tools EXCEPT `spawn_sublead` and
/// `wait_for_sublead`. Depth-2 cap is baked in; sub-leads cannot spawn
/// further subleads. The actor_role=sublead marker in the MCP bridge invocation
/// is enforced server-side via list_tools gating, but the CLI allowlist
/// provides defense-in-depth.
pub const SUBLEAD_MCP_TOOLS: &[&str] = &[
    // Worker orchestration tools (v0.3+).
    "mcp__pitboss__spawn_worker",
    "mcp__pitboss__worker_status",
    "mcp__pitboss__wait_for_worker",
    "mcp__pitboss__wait_for_any",
    "mcp__pitboss__wait_actor",
    "mcp__pitboss__list_workers",
    "mcp__pitboss__cancel_worker",
    "mcp__pitboss__pause_worker",
    "mcp__pitboss__continue_worker",
    "mcp__pitboss__request_approval",
    "mcp__pitboss__reprompt_worker",
    "mcp__pitboss__propose_plan",
    // Shared-store tools (v0.5+).
    "mcp__pitboss__kv_get",
    "mcp__pitboss__kv_set",
    "mcp__pitboss__kv_cas",
    "mcp__pitboss__kv_list",
    "mcp__pitboss__kv_wait",
    "mcp__pitboss__lease_acquire",
    "mcp__pitboss__lease_release",
    // Run-global leases (v0.6+) for cross-sub-tree resource coordination.
    "mcp__pitboss__run_lease_acquire",
    "mcp__pitboss__run_lease_release",
    // NOTE: spawn_sublead is intentionally NOT included (depth-2 cap).
];

/// Builds the argv for spawning the lead subprocess, including the
/// `--mcp-config` pointer to the generated MCP server config file.
///
/// Claude Code gates MCP tool use behind a permission prompt that can't be
/// answered in `-p` (non-interactive) mode, so we always pre-allow the six
/// pitboss MCP tools here. User-specified `tools` (from defaults / per-lead)
/// are merged in alongside the MCP set.
///
/// **Trust model — `--dangerously-skip-permissions`:** every pitboss-spawned
/// claude (lead, sub-lead, worker) launches with this flag. Pitboss is the
/// external permission authority via `[run].approval_policy`,
/// `[[approval_policy]]` rules, and the TUI's approve/reject modal — see
/// `apply_pitboss_env_defaults` for the rationale we already adopted for
/// MCP tools (CLAUDE_CODE_ENTRYPOINT=sdk-ts). The flag extends that
/// "single permission authority" design to the rest of claude's gates
/// (filesystem reads/writes, bash with `$VAR` expansion, bash with `&&`).
/// Without it, headless dispatch silently stalls: under `-p`, claude's own
/// prompts have no UI to answer them, so `echo x >> "$WORK_DIR/file"`
/// returns "Contains simple_expansion" and the orchestration plan
/// collapses with no operator-visible cause. Operators who want claude's
/// own gate back for a specific run should not use pitboss's headless
/// dispatch — they should drive the claude CLI interactively. See
/// CHANGELOG entry under v0.7.1 for the security note.
pub fn lead_spawn_args(
    lead: &crate::manifest::resolve::ResolvedLead,
    mcp_config: &std::path::Path,
) -> Vec<String> {
    use crate::manifest::schema::PermissionRouting;
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    // Path A (default): pitboss is the permission authority; bypass claude's gate.
    // Path B: leave gate active; permission_prompt MCP tool routes each check.
    if lead.permission_routing == PermissionRouting::PathA {
        args.push("--dangerously-skip-permissions".into());
    }
    // Plugin/skill isolation: prevent operator's ~/.claude/ plugins
    // (skills, MCP servers, agents, hooks) from bleeding in.
    args.push("--strict-mcp-config".into());
    args.push("--disable-slash-commands".into());

    // Build the allowed-tools set: user tools + pitboss MCP tools.
    let mut allowed: Vec<String> = lead.tools.clone();
    for t in PITBOSS_MCP_TOOLS {
        allowed.push((*t).to_string());
    }
    // v0.6: when allow_subleads=true, add the depth-2 tools to the allowlist
    // so claude's --allowedTools gate doesn't block them. The MCP server's
    // list_tools already gates visibility; this only widens the CLI allowlist.
    if lead.allow_subleads {
        allowed.push("mcp__pitboss__spawn_sublead".into());
    }
    // Path B: pre-allow permission_prompt so claude can route checks without stalling.
    if lead.permission_routing == PermissionRouting::PathB {
        allowed.push("mcp__pitboss__permission_prompt".into());
    }
    args.push("--allowedTools".into());
    args.push(allowed.join(","));

    args.push("--model".into());
    args.push(lead.model.clone());
    args.push("--mcp-config".into());
    args.push(mcp_config.display().to_string());
    if let Some(sess) = &lead.resume_session_id {
        args.push("--resume".into());
        args.push(sess.clone());
    }
    args.push("-p".into());
    args.push(lead.prompt.clone());
    args
}

/// Build the CLI args for resuming the root lead's claude subprocess with a
/// synthetic reprompt. Used by `run_hierarchical`'s kill+resume loop when a
/// `kill_worker_with_reason` reprompt message arrives.
///
/// Mirrors `lead_spawn_args` exactly except:
/// - `--resume <session_id>` is always emitted (using the captured session_id)
/// - `-p <prompt>` uses the supplied `new_prompt` instead of `lead.prompt`
pub fn lead_resume_spawn_args(
    lead: &crate::manifest::resolve::ResolvedLead,
    mcp_config: &std::path::Path,
    session_id: &str,
    new_prompt: &str,
) -> Vec<String> {
    use crate::manifest::schema::PermissionRouting;
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if lead.permission_routing == PermissionRouting::PathA {
        args.push("--dangerously-skip-permissions".into());
    }
    // Plugin/skill isolation (see lead_spawn_args doc).
    args.push("--strict-mcp-config".into());
    args.push("--disable-slash-commands".into());
    let mut allowed: Vec<String> = lead.tools.clone();
    for t in PITBOSS_MCP_TOOLS {
        allowed.push((*t).to_string());
    }
    if lead.allow_subleads {
        allowed.push("mcp__pitboss__spawn_sublead".into());
    }
    if lead.permission_routing == PermissionRouting::PathB {
        allowed.push("mcp__pitboss__permission_prompt".into());
    }
    args.push("--allowedTools".into());
    args.push(allowed.join(","));

    args.push("--model".into());
    args.push(lead.model.clone());
    args.push("--mcp-config".into());
    args.push(mcp_config.display().to_string());
    args.push("--resume".into());
    args.push(session_id.into());
    args.push("-p".into());
    args.push(new_prompt.into());
    args
}

/// Build the CLI args for spawning a sub-lead's claude subprocess.
/// Mirrors `lead_spawn_args` but enforces depth-2 cap via the toolset:
/// `spawn_sublead` and `wait_for_sublead` are NOT included, regardless of
/// the root lead's `allow_subleads` setting. The mcp-bridge invocation
/// passes `actor_role=sublead` so MCP handlers can route requests to the
/// correct layer and gate tools accordingly.
///
/// The sub-lead is spawned as a worker-of-root in the dispatch tree,
/// so its lifecycle is tracked for cancellation and wait purposes.
///
/// # Arguments
///
/// - `sublead_id`: UUIDv7 assigned to this sub-lead instance
/// - `prompt`: The task prompt for the sub-lead
/// - `model`: The model name (e.g., "claude-opus-4-1")
/// - `mcp_config_path`: Path to the per-sublead mcp-config file
/// - `resume_session_id`: Optional session ID to resume from
/// - `tools_override`: When `Some(&[...])`, the listed tools are added to
///   the allow-list alongside the standard sublead MCP toolset. When
///   `None`, only the MCP toolset is included (preserves v0.6 behavior).
///   De-duplicated to keep the resulting `--allowedTools` flag tidy.
/// - `permission_routing`: controls whether `--dangerously-skip-permissions`
///   is included and whether `permission_prompt` is pre-allowed.
pub fn sublead_spawn_args(
    _sublead_id: &str,
    prompt: &str,
    model: &str,
    mcp_config_path: &std::path::Path,
    resume_session_id: Option<&str>,
    tools_override: Option<&[String]>,
    permission_routing: crate::manifest::schema::PermissionRouting,
) -> Vec<String> {
    use crate::manifest::schema::PermissionRouting;
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if permission_routing == PermissionRouting::PathA {
        args.push("--dangerously-skip-permissions".into());
    }
    // Plugin/skill isolation (see lead_spawn_args doc).
    args.push("--strict-mcp-config".into());
    args.push("--disable-slash-commands".into());

    // Build the allowed-tools set. Operator-supplied tools (if any) are
    // listed first; pitboss MCP tools always appended so the sub-lead can
    // still orchestrate workers regardless of the override.
    let mut allowed: Vec<String> = match tools_override {
        Some(ts) => ts.to_vec(),
        None => Vec::new(),
    };
    for t in SUBLEAD_MCP_TOOLS {
        allowed.push((*t).to_string());
    }
    if permission_routing == PermissionRouting::PathB {
        allowed.push("mcp__pitboss__permission_prompt".into());
    }
    // De-duplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    allowed.retain(|t| seen.insert(t.clone()));
    args.push("--allowedTools".into());
    args.push(allowed.join(","));

    args.push("--model".into());
    args.push(model.into());
    args.push("--mcp-config".into());
    args.push(mcp_config_path.display().to_string());
    if let Some(sess) = resume_session_id {
        args.push("--resume".into());
        args.push(sess.into());
    }
    args.push("-p".into());
    args.push(prompt.into());
    args
}

// ── Task 4.4: TTL watcher for pending approvals ────────────────────────────────

/// Background task that periodically scans the approval queue for
/// expired entries and applies their fallback action. Runs every
/// 250ms; cancels itself when state.root.cancel terminates.
pub fn install_approval_ttl_watcher(state: Arc<crate::dispatch::state::DispatchState>) {
    let token = state.root.cancel.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    expire_approvals(&state).await;
                }
                _ = token.await_terminate() => {
                    break;
                }
            }
        }
    });
}

async fn expire_approvals(state: &Arc<crate::dispatch::state::DispatchState>) {
    // Root layer first.
    expire_layer_approvals(state, &state.root).await;

    // Then every sub-lead layer. Acquiring the outer `subleads` read lock
    // first and each inner `approval_queue` lock in a fresh per-iteration
    // scope matches the project-wide lock ordering rule (see DispatchState
    // docs) and avoids holding both kinds of locks across await points.
    let subleads = state.subleads.read().await;
    for sub_layer in subleads.values() {
        expire_layer_approvals(state, sub_layer).await;
    }
}

async fn expire_layer_approvals(
    state: &Arc<crate::dispatch::state::DispatchState>,
    layer: &Arc<crate::dispatch::layer::LayerState>,
) {
    use crate::dispatch::state::ApprovalResponse;
    use crate::mcp::approval::ApprovalFallback;

    let now = chrono::Utc::now();

    // ── Scan approval_queue ───────────────────────────────────────────────────
    // Entries here have not yet been seen by any TUI.
    let mut queue = layer.approval_queue.lock().await;
    let mut i = 0;
    let mut expired_queue = Vec::new();
    while i < queue.len() {
        let expired_now = match queue[i].ttl_secs {
            Some(ttl_secs) => {
                let age = (now - queue[i].created_at).num_seconds() as u64;
                let fallback = queue[i].fallback.unwrap_or(ApprovalFallback::Block);
                age >= ttl_secs && !matches!(fallback, ApprovalFallback::Block)
            }
            None => false,
        };
        if expired_now {
            expired_queue.push(queue.remove(i).expect("index in range"));
        } else {
            i += 1;
        }
    }
    drop(queue);

    for entry in expired_queue {
        let fallback = entry.fallback.unwrap_or(ApprovalFallback::Block);
        let approved = matches!(fallback, ApprovalFallback::AutoApprove);
        tracing::info!(
            request_id = %entry.request_id,
            task_id = %entry.task_id,
            fallback = ?fallback,
            approved,
            "approval queue TTL expired, applying fallback"
        );
        let response = ApprovalResponse {
            approved,
            comment: Some("TTL expired".to_string()),
            edited_summary: None,
            reason: Some(format!("TTL expired: fallback={fallback:?}")),
            from_ttl: true,
        };
        state
            .record_last_approval_response(&entry.task_id, approved, true)
            .await;
        let _ = entry.responder.send(response);
    }

    // ── Scan approval_bridge ──────────────────────────────────────────────────
    // Entries here were drained to a TUI that may have disconnected before
    // responding. Without this scan they would silently miss their TTL.
    let expired_bridge_ids: Vec<String> = {
        let bridge = layer.approval_bridge.lock().await;
        bridge
            .iter()
            .filter_map(|(id, entry)| {
                let ttl = entry.ttl_secs?;
                let fallback = entry.fallback.unwrap_or(ApprovalFallback::Block);
                if matches!(fallback, ApprovalFallback::Block) {
                    return None;
                }
                let age = (now - entry.created_at).num_seconds() as u64;
                if age >= ttl {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect()
    };

    for request_id in expired_bridge_ids {
        let entry = layer.approval_bridge.lock().await.remove(&request_id);
        if let Some(entry) = entry {
            let fallback = entry.fallback.unwrap_or(ApprovalFallback::Block);
            let approved = matches!(fallback, ApprovalFallback::AutoApprove);
            tracing::info!(
                request_id = %request_id,
                task_id = %entry.task_id,
                fallback = ?fallback,
                approved,
                "approval bridge TTL expired, applying fallback"
            );
            let response = ApprovalResponse {
                approved,
                comment: Some("TTL expired".to_string()),
                edited_summary: None,
                reason: Some(format!("TTL expired: fallback={fallback:?}")),
                from_ttl: true,
            };
            state
                .record_last_approval_response(&entry.task_id, approved, true)
                .await;
            let _ = entry.responder.send(response);
        }
    }
}

// Note: these tests use pitboss-core's FakeSpawner, which is gated by
// pitboss-core's "test-support" feature. That feature is always enabled in
// pitboss-cli's dev-dependencies, so the tests always compile in `cargo test`.
#[cfg(test)]
mod tests {
    use super::*;
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use std::collections::HashMap;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn apply_pitboss_env_defaults_sets_entrypoint_when_absent() {
        let mut env: HashMap<String, String> = HashMap::new();
        apply_pitboss_env_defaults(&mut env, Default::default());
        assert_eq!(
            env.get("CLAUDE_CODE_ENTRYPOINT"),
            Some(&"sdk-ts".to_string()),
            "default should be applied to an empty env"
        );
    }

    #[test]
    fn apply_pitboss_env_defaults_honors_operator_override() {
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("CLAUDE_CODE_ENTRYPOINT".to_string(), "cli".to_string());
        apply_pitboss_env_defaults(&mut env, Default::default());
        assert_eq!(
            env.get("CLAUDE_CODE_ENTRYPOINT"),
            Some(&"cli".to_string()),
            "operator-set value must not be overwritten"
        );
    }

    #[test]
    fn apply_pitboss_env_defaults_preserves_other_keys() {
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("FOO".to_string(), "bar".to_string());
        apply_pitboss_env_defaults(&mut env, Default::default());
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
        assert_eq!(
            env.get("CLAUDE_CODE_ENTRYPOINT"),
            Some(&"sdk-ts".to_string())
        );
    }

    fn minimal_manifest() -> ResolvedManifest {
        ResolvedManifest {
            max_parallel: 1,
            halt_on_failure: false,
            run_dir: PathBuf::from("/tmp/pitboss-test"),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: Some(crate::dispatch::state::ApprovalPolicy::AutoApprove),
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        }
    }

    #[test]
    fn headless_warnings_empty_for_clean_manifest() {
        let manifest = minimal_manifest();
        let warnings = headless_approval_gate_warnings(&manifest);
        assert!(
            warnings.is_empty(),
            "clean manifest should not warn: {:?}",
            warnings
        );
    }

    #[test]
    fn headless_warnings_flag_require_plan_approval() {
        let mut manifest = minimal_manifest();
        manifest.require_plan_approval = true;
        let warnings = headless_approval_gate_warnings(&manifest);
        assert_eq!(warnings.len(), 1, "expected one warning: {:?}", warnings);
        assert!(warnings[0].contains("require_plan_approval"));
    }

    #[test]
    fn headless_warnings_flag_block_default_when_unset() {
        let mut manifest = minimal_manifest();
        manifest.approval_policy = None;
        let warnings = headless_approval_gate_warnings(&manifest);
        assert_eq!(warnings.len(), 1, "expected one warning: {:?}", warnings);
        assert!(warnings[0].contains("approval_policy"));
    }

    #[test]
    fn headless_warnings_flag_block_policy_explicit() {
        let mut manifest = minimal_manifest();
        manifest.approval_policy = Some(crate::dispatch::state::ApprovalPolicy::Block);
        let warnings = headless_approval_gate_warnings(&manifest);
        assert_eq!(warnings.len(), 1, "expected one warning: {:?}", warnings);
        assert!(warnings[0].contains("approval_policy"));
    }

    #[test]
    fn headless_warnings_flag_block_rule() {
        let mut manifest = minimal_manifest();
        manifest.approval_rules = vec![crate::mcp::policy::ApprovalRule {
            r#match: crate::mcp::policy::ApprovalMatch::default(),
            action: crate::mcp::policy::ApprovalAction::Block,
        }];
        let warnings = headless_approval_gate_warnings(&manifest);
        assert_eq!(warnings.len(), 1, "expected one warning: {:?}", warnings);
        assert!(warnings[0].contains("approval_policy") && warnings[0].contains("rule"));
    }

    #[test]
    fn headless_warnings_stack_when_multiple_gates_present() {
        let mut manifest = minimal_manifest();
        manifest.require_plan_approval = true;
        manifest.approval_policy = Some(crate::dispatch::state::ApprovalPolicy::Block);
        let warnings = headless_approval_gate_warnings(&manifest);
        assert_eq!(warnings.len(), 2, "expected two warnings: {:?}", warnings);
    }

    fn init_repo(root: &std::path::Path) {
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t.x"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(root)
            .status()
            .unwrap();
        std::fs::write(root.join("r"), "").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "i"])
            .current_dir(root)
            .status()
            .unwrap();
    }

    #[tokio::test]
    async fn executes_three_tasks_with_mixed_outcomes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 2,
            halt_on_failure: false,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![
                ResolvedTask {
                    id: "ok".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(),
                    branch: None,
                    model: "m".into(),
                    effort: crate::manifest::schema::Effort::High,
                    tools: vec![],
                    timeout_secs: 30,
                    use_worktree: false,
                    env: Default::default(),
                    resume_session_id: None,
                },
                ResolvedTask {
                    id: "bad".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(),
                    branch: None,
                    model: "m".into(),
                    effort: crate::manifest::schema::Effort::High,
                    tools: vec![],
                    timeout_secs: 30,
                    use_worktree: false,
                    env: Default::default(),
                    resume_session_id: None,
                },
            ],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };

        // Script: first call succeeds, second call fails. FakeSpawner is single-shot,
        // so we use a cycling spawner.
        let spawner = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s1","usage":{"input_tokens":1,"output_tokens":2}}"#)
                    .exit_code(0),
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s2","usage":{"input_tokens":1,"output_tokens":2}}"#)
                    .exit_code(5),
            ],
            std::sync::Mutex::new(0),
        ));

        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(
            resolved,
            String::new(),
            PathBuf::new(),
            PathBuf::from("claude"),
            None,
            spawner,
            store.clone(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(rc, 1, "one failure → exit 1");
    }

    struct CyclingFake(Vec<FakeScript>, std::sync::Mutex<usize>);

    #[async_trait::async_trait]
    impl ProcessSpawner for CyclingFake {
        async fn spawn(
            &self,
            cmd: SpawnCmd,
        ) -> Result<Box<dyn pitboss_core::process::ChildProcess>, pitboss_core::error::SpawnError>
        {
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

    #[tokio::test]
    async fn halt_on_failure_drains_after_first_failure() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let make_task = |id: &str| ResolvedTask {
            id: id.into(),
            directory: dir.path().to_path_buf(),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec![],
            timeout_secs: 30,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 1, // serialize so ordering is deterministic
            halt_on_failure: true,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![make_task("a"), make_task("b"), make_task("c")],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };

        let spawner = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":0,"output_tokens":0}}"#)
                    .exit_code(7),   // fails → cascade
                FakeScript::new().exit_code(0),
                FakeScript::new().exit_code(0),
            ],
            std::sync::Mutex::new(0),
        ));
        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(
            resolved,
            String::new(),
            PathBuf::new(),
            PathBuf::from("claude"),
            None,
            spawner,
            store.clone(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(rc, 1);

        // Expect only task "a" recorded; others were skipped by the drain.
        // summary.json should exist with tasks.len() == 1.
        let summary_path = run_dir
            .path()
            .join(store_run_id_dir(run_dir.path()))
            .join("summary.json");
        let bytes = std::fs::read(&summary_path).unwrap();
        let s: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(s.tasks.len(), 1);
    }

    fn store_run_id_dir(root: &std::path::Path) -> String {
        // Finds the single UUID-named subdir just created.
        for entry in std::fs::read_dir(root).unwrap() {
            let e = entry.unwrap();
            if e.path().is_dir() {
                return e.file_name().to_string_lossy().to_string();
            }
        }
        panic!("no run dir")
    }

    /// Regression: the dispatch runner must call `store.append_record` after
    /// each task completes so `summary.jsonl` reflects completed tasks on disk
    /// incrementally. A prior bug left the file empty until finalize_run.
    #[tokio::test]
    async fn summary_jsonl_populated_incrementally() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 1,
            halt_on_failure: false,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![
                ResolvedTask {
                    id: "one".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(),
                    branch: None,
                    model: "m".into(),
                    effort: crate::manifest::schema::Effort::High,
                    tools: vec![],
                    timeout_secs: 30,
                    use_worktree: false,
                    env: Default::default(),
                    resume_session_id: None,
                },
                ResolvedTask {
                    id: "two".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(),
                    branch: None,
                    model: "m".into(),
                    effort: crate::manifest::schema::Effort::High,
                    tools: vec![],
                    timeout_secs: 30,
                    use_worktree: false,
                    env: Default::default(),
                    resume_session_id: None,
                },
            ],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };

        let spawner = Arc::new(CyclingFake(
            vec![FakeScript::new()
                .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":1,"output_tokens":2}}"#)
                .exit_code(0)],
            std::sync::Mutex::new(0),
        ));
        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(
            resolved,
            String::new(),
            PathBuf::new(),
            PathBuf::from("claude"),
            None,
            spawner,
            store.clone(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(rc, 0);

        let jsonl_path = run_dir
            .path()
            .join(store_run_id_dir(run_dir.path()))
            .join("summary.jsonl");
        let contents =
            std::fs::read_to_string(&jsonl_path).expect("summary.jsonl must exist and be readable");
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            2,
            "both tasks should have records appended to summary.jsonl, got: {contents}"
        );
        // Each line must parse as a TaskRecord.
        for l in &lines {
            let _: pitboss_core::store::TaskRecord =
                serde_json::from_str(l).unwrap_or_else(|e| panic!("line does not parse: {e}: {l}"));
        }
    }

    fn make_test_task(id: &str, resume_session_id: Option<String>) -> ResolvedTask {
        ResolvedTask {
            id: id.into(),
            directory: PathBuf::from("/tmp"),
            prompt: "test prompt".into(),
            branch: None,
            model: "claude-test".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec![],
            timeout_secs: 30,
            use_worktree: false,
            env: Default::default(),
            resume_session_id,
        }
    }

    #[tokio::test]
    async fn spawn_args_includes_resume_when_session_id_set() {
        let task = make_test_task("t", Some("sess_abc".to_string()));
        let args = spawn_args(&task);
        assert!(
            args.iter().any(|a| a == "--resume"),
            "expected --resume in args: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "sess_abc"),
            "expected sess_abc in args: {args:?}"
        );
    }

    #[tokio::test]
    async fn spawn_args_omits_resume_when_no_session_id() {
        let task = make_test_task("t", None);
        let args = spawn_args(&task);
        assert!(
            !args.iter().any(|a| a == "--resume"),
            "expected no --resume in args: {args:?}"
        );
    }

    #[test]
    fn every_spawn_variant_has_all_isolation_flags() {
        // Canary for all three hardening flags every pitboss-spawned claude
        // must carry:
        //   - `--dangerously-skip-permissions`: pitboss is the permission
        //     authority; without this, headless dispatch silently stalls on
        //     bash-with-`$VAR`, write-outside-cwd, etc.
        //   - `--strict-mcp-config` + `--disable-slash-commands`: prevents
        //     operator's ~/.claude/ plugins (skills, MCP servers, agents)
        //     from bleeding into spawned subprocesses.
        use crate::manifest::resolve::{ResolvedLead, ResolvedTask};
        use std::path::PathBuf;

        let task = ResolvedTask {
            id: "t".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec!["Read".into()],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
        };
        let lead = ResolvedLead {
            id: "l".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec!["Read".into()],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: true,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_workers_across_tree: None,
            sublead_defaults: None,
        };
        let cfg = PathBuf::from("/tmp/cfg.json");
        let cases: Vec<(&str, Vec<String>)> = vec![
            ("flat task", spawn_args(&task)),
            ("lead", lead_spawn_args(&lead, &cfg)),
            (
                "lead_resume",
                lead_resume_spawn_args(&lead, &cfg, "sess", "new prompt"),
            ),
            (
                "sublead",
                sublead_spawn_args("sl-id", "p", "m", &cfg, None, None, Default::default()),
            ),
            (
                "sublead_resume",
                sublead_spawn_args(
                    "sl-id",
                    "p",
                    "m",
                    &cfg,
                    Some("sess"),
                    None,
                    Default::default(),
                ),
            ),
        ];
        for (name, argv) in cases {
            assert!(
                argv.iter().any(|a| a == "--dangerously-skip-permissions"),
                "{name} spawn args missing --dangerously-skip-permissions: {argv:?}"
            );
            assert!(
                argv.iter().any(|a| a == "--strict-mcp-config"),
                "{name} spawn args missing --strict-mcp-config: {argv:?}"
            );
            assert!(
                argv.iter().any(|a| a == "--disable-slash-commands"),
                "{name} spawn args missing --disable-slash-commands: {argv:?}"
            );
        }
    }

    #[test]
    fn lead_spawn_args_includes_mcp_config_and_verbose() {
        use crate::manifest::resolve::ResolvedLead;
        use std::path::PathBuf;
        let lead = ResolvedLead {
            id: "l".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec!["Read".into()],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: false,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_workers_across_tree: None,
            sublead_defaults: None,
        };
        let args = lead_spawn_args(&lead, &PathBuf::from("/tmp/cfg.json"));
        assert!(args.iter().any(|a| a == "--verbose"));
        assert!(args.iter().any(|a| a == "--mcp-config"));
        assert!(args.iter().any(|a| a == "/tmp/cfg.json"));
        assert!(args.iter().any(|a| a == "-p"));
        assert!(args.iter().any(|a| a == "p"));
    }

    #[test]
    fn lead_spawn_args_auto_allows_pitboss_mcp_tools() {
        use crate::manifest::resolve::ResolvedLead;
        use std::path::PathBuf;
        let lead = ResolvedLead {
            id: "l".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec!["Read".into()],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: false,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_workers_across_tree: None,
            sublead_defaults: None,
        };
        let args = lead_spawn_args(&lead, &PathBuf::from("/tmp/cfg.json"));
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        // User-declared tool preserved
        assert!(list.contains("Read"), "expected user tool, got {list}");
        // All six pitboss MCP tools present under the `mcp__pitboss__` prefix.
        for t in PITBOSS_MCP_TOOLS {
            assert!(
                list.contains(t),
                "expected {t} in allowedTools, got: {list}"
            );
        }
    }

    #[test]
    fn lead_spawn_args_allows_depth2_orchestration_tools() {
        // Regression: prior to this test, the root-lead allowlist omitted
        // wait_actor, propose_plan, and run_lease_*, so claude's own
        // --allowedTools gate denied them at call time ("Claude requested
        // permissions to use mcp__pitboss__wait_actor, but you haven't
        // granted it yet"). These tools MUST be pre-allowed.
        use crate::manifest::resolve::ResolvedLead;
        use std::path::PathBuf;
        let lead = ResolvedLead {
            id: "l".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec!["Read".into()],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: true,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_workers_across_tree: None,
            sublead_defaults: None,
        };
        let args = lead_spawn_args(&lead, &PathBuf::from("/tmp/cfg.json"));
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        for t in [
            "mcp__pitboss__wait_actor",
            "mcp__pitboss__propose_plan",
            "mcp__pitboss__run_lease_acquire",
            "mcp__pitboss__run_lease_release",
            // spawn_sublead only appears when allow_subleads=true (our fixture).
            "mcp__pitboss__spawn_sublead",
        ] {
            assert!(
                list.contains(t),
                "expected {t} in allowedTools, got: {list}"
            );
        }
        // Phantom entry removed: `wait_for_sublead` is not a real server tool.
        // Keeping it in the allowlist masked the `wait_actor` omission during
        // review, and the string is load-bearing noise on the CLI.
        assert!(
            !list.contains("mcp__pitboss__wait_for_sublead"),
            "phantom wait_for_sublead should not be in allowedTools, got: {list}"
        );
    }

    #[test]
    fn sublead_spawn_args_allows_depth2_orchestration_tools() {
        // Subleads wait on their own workers (wait_actor), propose plans
        // when the root has require_plan_approval set, and coordinate across
        // sub-trees via run_lease_*. All must be pre-allowed to avoid the
        // same Claude permission-prompt failure mode the root lead hit.
        let args = sublead_spawn_args(
            "test-sublead-id",
            "do some work",
            "claude-opus-4-1",
            &PathBuf::from("/tmp/sublead-cfg.json"),
            None,
            None,
            Default::default(),
        );
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        for t in [
            "mcp__pitboss__wait_actor",
            "mcp__pitboss__propose_plan",
            "mcp__pitboss__run_lease_acquire",
            "mcp__pitboss__run_lease_release",
        ] {
            assert!(
                list.contains(t),
                "expected {t} in sublead allowedTools, got: {list}"
            );
        }
    }

    #[test]
    fn sublead_spawn_args_excludes_spawn_sublead() {
        let args = sublead_spawn_args(
            "test-sublead-id",
            "do some work",
            "claude-opus-4-1",
            &PathBuf::from("/tmp/sublead-cfg.json"),
            None,
            None,
            Default::default(),
        );
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        // spawn_sublead must NOT be in the allowlist
        assert!(
            !list.contains("mcp__pitboss__spawn_sublead"),
            "spawn_sublead should NOT be in sublead allowedTools, got: {list}"
        );
        // wait_for_sublead must NOT be in the allowlist either
        assert!(
            !list.contains("mcp__pitboss__wait_for_sublead"),
            "wait_for_sublead should NOT be in sublead allowedTools, got: {list}"
        );
    }

    #[test]
    fn sublead_spawn_args_includes_spawn_worker() {
        let args = sublead_spawn_args(
            "test-sublead-id",
            "do some work",
            "claude-opus-4-1",
            &PathBuf::from("/tmp/sublead-cfg.json"),
            None,
            None,
            Default::default(),
        );
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        // spawn_worker MUST be in the allowlist
        assert!(
            list.contains("mcp__pitboss__spawn_worker"),
            "spawn_worker should be in sublead allowedTools, got: {list}"
        );
    }

    #[test]
    fn sublead_spawn_args_passes_correct_actor_role() {
        let args = sublead_spawn_args(
            "test-sublead-id",
            "do some work",
            "claude-opus-4-1",
            &PathBuf::from("/tmp/sublead-cfg.json"),
            Some("resume-session-123"),
            None,
            Default::default(),
        );
        // Verify the basic arg structure is correct
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"claude-opus-4-1".to_string()));
        assert!(args.contains(&"--mcp-config".to_string()));
        assert!(args.contains(&"/tmp/sublead-cfg.json".to_string()));
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"resume-session-123".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"do some work".to_string()));
    }

    #[test]
    fn sublead_spawn_args_honors_tools_override() {
        let custom = ["Read".to_string(), "Bash".to_string()];
        let args = sublead_spawn_args(
            "test-sublead-id",
            "do some work",
            "claude-opus-4-1",
            &PathBuf::from("/tmp/sublead-cfg.json"),
            None,
            Some(&custom),
            Default::default(),
        );
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        // Operator-supplied tools must be present.
        assert!(list.contains("Read"), "Read missing from {list}");
        assert!(list.contains("Bash"), "Bash missing from {list}");
        // pitboss MCP tools must still be present (override doesn't remove them).
        assert!(
            list.contains("mcp__pitboss__"),
            "pitboss MCP tools missing from {list}"
        );
    }

    #[test]
    fn sublead_spawn_args_dedups_when_override_overlaps_mcp_tools() {
        // Operator passes a pitboss MCP tool already in the standard set.
        // Result should not contain duplicates.
        let custom = ["mcp__pitboss__spawn_worker".to_string()];
        let args = sublead_spawn_args(
            "test-sublead-id",
            "do some work",
            "claude-opus-4-1",
            &PathBuf::from("/tmp/sublead-cfg.json"),
            None,
            Some(&custom),
            Default::default(),
        );
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        let count = list.matches("mcp__pitboss__spawn_worker").count();
        assert_eq!(count, 1, "spawn_worker appears {count} times in {list}");
    }

    #[test]
    fn path_b_lead_omits_dangerously_skip_and_includes_permission_prompt() {
        use crate::manifest::resolve::ResolvedLead;
        use crate::manifest::schema::PermissionRouting;
        use std::path::PathBuf;
        let lead = ResolvedLead {
            id: "l".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: PermissionRouting::PathB,
            allow_subleads: false,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_workers_across_tree: None,
            sublead_defaults: None,
        };
        let cfg = PathBuf::from("/tmp/cfg.json");
        let args = lead_spawn_args(&lead, &cfg);
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "Path B lead must NOT have --dangerously-skip-permissions: {args:?}"
        );
        let idx = args.iter().position(|a| a == "--allowedTools").unwrap();
        let list = &args[idx + 1];
        assert!(
            list.contains("mcp__pitboss__permission_prompt"),
            "Path B lead allowedTools must include permission_prompt: {list}"
        );
    }
}
