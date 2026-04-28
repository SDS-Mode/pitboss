//! Shared dispatch entry-point primitives used by both flat
//! (`runner::execute`) and hierarchical (`hierarchical::run_hierarchical`)
//! mode. The two paths previously inlined ~50 lines of identical
//! boilerplate each (run-id minting, manifest snapshot writes, RunMeta
//! init, notification-router build, RunDispatched emit). Centralizing
//! them here closes the audit's #150 M9 finding — past drift between
//! the two copies caused subtle differences (e.g. the `mode` string,
//! the `set_run_subdir` binding) that were easy to miss in code review.
//!
//! Hierarchical-only steps (lead validation, MCP server start, sub-lead
//! resume seeding, headless approval-gate warnings) stay in
//! `dispatch/hierarchical.rs`. Flat-only steps (per-task spawn loop,
//! progress table, semaphore, halt_drained tracking) stay in
//! `dispatch/runner.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use pitboss_core::store::{RunMeta, SessionStore};

use crate::manifest::resolve::ResolvedManifest;

/// Identifiers + paths produced by [`init_run_state`] and consumed by
/// the rest of the dispatch path. `run_dir` is the resolved run root
/// (operator override or `resolved.run_dir`); `run_subdir` is the
/// per-dispatch directory underneath it that holds the manifest
/// snapshots, summary.jsonl, etc.
pub struct RunInit {
    pub run_id: Uuid,
    pub run_subdir: PathBuf,
    /// Resolved run root — the operator-supplied `run_dir_override`
    /// when present, otherwise `resolved.run_dir`. Hierarchical mode
    /// uses this for the MCP socket path and the control socket;
    /// flat mode reads it via `resolved.run_dir` directly.
    pub run_dir: PathBuf,
    /// `PITBOSS_RUN_ID` from the inherited env at dispatch start,
    /// snapshotted before we overwrite it. `None` when this dispatch
    /// isn't running under a parent orchestrator. Surfaced on the
    /// `RunDispatched` notification.
    pub parent_run_id: Option<String>,
    /// Wall-clock at the moment of `RunMeta` construction. Used by the
    /// finalize phase to compute `RunSummary.total_duration_ms`
    /// authoritatively (vs. snapshotting again at finalize time, which
    /// would miss any time spent before the first task spawned).
    pub started_at: DateTime<Utc>,
}

/// Mint / honor the run id, snapshot the parent run id from the
/// inherited env BEFORE overwriting it, write per-run subdir +
/// manifest snapshot files, and initialise `RunMeta` in the store.
///
/// Both flat and hierarchical mode call this. The `run_dir_override`
/// parameter is `None` for flat mode (uses `resolved.run_dir`) and
/// `Some(_)` when the operator passed `--run-dir` to hierarchical
/// dispatch. Callers that need to fail-fast on validation (e.g.
/// hierarchical mode rejecting a manifest with no `[[lead]]`) should
/// do that BEFORE calling `init_run_state` so a bailed dispatch
/// leaves no on-disk artifacts and no orphan `RunMeta` entry in the
/// store.
pub async fn init_run_state(
    resolved: &ResolvedManifest,
    manifest_text: &str,
    manifest_path: &Path,
    claude_version: Option<String>,
    store: &Arc<dyn SessionStore>,
    pre_minted_run_id: Option<Uuid>,
    run_dir_override: Option<PathBuf>,
) -> Result<RunInit> {
    // Snapshot any `PITBOSS_RUN_ID` already in our env BEFORE we
    // overwrite it with our own run_id. If we're running under a
    // parent orchestrator (or as a sub-dispatch the agent triggered
    // from inside a worktree), the prior value is the parent run id
    // reported on `RunDispatched`. See the `notify::parent` module for
    // the full env-var contract introduced for issue #133.
    let parent_run_id = crate::notify::parent::parent_run_id();
    // Honor a pre-minted id from `--background` (issue #133-C);
    // otherwise mint fresh. Background pre-mints in the parent so it
    // can announce the id on stdout before the detached child boots.
    let run_id = pre_minted_run_id.unwrap_or_else(Uuid::now_v7);
    crate::notify::parent::set_run_id_env(&run_id.to_string());

    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();

    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), manifest_text).await?;
    if let Ok(b) = serde_json::to_vec_pretty(resolved) {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }

    let started_at = Utc::now();
    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.to_path_buf(),
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at,
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    Ok(RunInit {
        run_id,
        run_subdir,
        run_dir,
        parent_run_id,
        started_at,
    })
}

/// Build a notification router from manifest `[[notification]]` sections
/// AND the optional `PITBOSS_PARENT_NOTIFY_URL` env var, bind it to
/// the run subdir for terminal-emit-failure logging, and fire a
/// `RunDispatched` notification before any tokens spend.
///
/// Returns `Ok(None)` when both notification sources are empty so the
/// no-notify common case stays cost-free. The `mode` parameter is the
/// string carried on `RunDispatched.mode` (`"flat"` or
/// `"hierarchical"`); centralizing the call here prevents the two
/// modes from drifting on this label, which downstream orchestrators
/// match on for routing.
pub async fn build_notification_router_and_emit_dispatched(
    resolved: &ResolvedManifest,
    manifest_path: &Path,
    init: &RunInit,
    mode: &'static str,
) -> Result<Option<Arc<crate::notify::NotificationRouter>>> {
    let http = std::sync::Arc::new(reqwest::Client::new());
    let router = crate::notify::parent::build_router(&resolved.notifications, &http)?;
    if let Some(r) = &router {
        // Bind the run subdir so terminal emit failures land in
        // <run_subdir>/notifications.jsonl as
        // TaskEvent::NotificationFailed. Issue #156 (M4).
        r.set_run_subdir(init.run_subdir.clone());

        // Fire RunDispatched immediately. The orchestrator wants to
        // register the run before any tokens are spent — emitting at
        // finalize-time only (the prior behavior) defeats the point
        // of the hook.
        let env = crate::notify::NotificationEnvelope::new(
            &init.run_id.to_string(),
            crate::notify::Severity::Info,
            crate::notify::PitbossEvent::RunDispatched {
                run_id: init.run_id.to_string(),
                parent_run_id: init.parent_run_id.clone(),
                manifest_path: manifest_path.display().to_string(),
                mode: mode.to_string(),
                survive_parent: resolved
                    .lifecycle
                    .as_ref()
                    .is_some_and(|l| l.survive_parent),
            },
            Utc::now(),
        );
        let _ = r.dispatch(env).await;
    }
    Ok(router)
}
