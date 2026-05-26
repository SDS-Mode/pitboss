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
    // Snapshot any `PITBOSS_RUN_ID` already in our env. If we're running
    // under a parent orchestrator (or as a sub-dispatch the agent
    // triggered from inside a worktree), the inherited value is the parent
    // run id reported on `RunDispatched`. See `notify::parent` for the env
    // contract introduced for issue #133. Our own run_id is propagated to
    // spawned claude subprocesses via per-spawn `Command::env` injection
    // through `apply_pitboss_env_defaults` (closes #328 — pre-fix this was
    // a parent-process `std::env::set_var`, which is UB-leaning under a
    // multi-threaded tokio runtime).
    let parent_run_id = crate::notify::parent::parent_run_id();
    // Honor a pre-minted id from `--background` (issue #133-C);
    // otherwise mint fresh. Background pre-mints in the parent so it
    // can announce the id on stdout before the detached child boots.
    let run_id = pre_minted_run_id.unwrap_or_else(Uuid::now_v7);

    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();

    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), manifest_text).await?;
    if let Ok(b) = serde_json::to_vec_pretty(resolved) {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }

    let started_at = Utc::now();
    // PITBOSS_CONTROL_TCP_PORT is injected by `pitboss container-dispatch`
    // on the host (it allocates a free TCP port and publishes it via
    // `-p 127.0.0.1:<port>:<port>` + `-e PITBOSS_CONTROL_TCP_PORT=<port>`).
    // When present, record the host-side dial address in meta.json so
    // pitboss-web can reach the control bridge on platforms where the
    // in-container AF_UNIX socket isn't visible from the host (#474).
    let control_tcp_addr = std::env::var("PITBOSS_CONTROL_TCP_PORT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|p| format!("127.0.0.1:{p}"));
    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.to_path_buf(),
        pitboss_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at,
        env: Default::default(),
        control_tcp_addr,
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

#[cfg(test)]
mod tests {
    //! Branch coverage for `init_run_state` (#532 / F-TEST-1). The function
    //! is exercised end-to-end via subprocess dispatch in `dogfood_fake_flows.rs`,
    //! but the four input axes (pre-minted vs fresh run-id, `PITBOSS_RUN_ID`
    //! snapshotting from env, `run_dir_override` vs `resolved.run_dir`, and
    //! manifest snapshot file creation) need direct asserts so a regression
    //! in any one branch — e.g. an accidental swap of `pre_minted_run_id` /
    //! `Uuid::now_v7()` ordering — fails fast at the unit level.

    use super::*;
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use pitboss_core::store::JsonFileStore;
    use serial_test::serial;
    use tempfile::TempDir;

    /// Minimal `ResolvedManifest` for unit tests in this module. `run_dir`
    /// points at the caller's `TempDir` so artifacts land in a scoped
    /// location; everything else is zeroed/empty.
    fn mk_manifest(run_dir: PathBuf) -> ResolvedManifest {
        ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(1),
            halt_on_failure: false,
            run_dir,
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            resource_sample_secs: 0,
            claude_setting_sources: None,
            tasks: vec![],
            lead: None,
            max_workers: Some(1),
            budget_usd: None,
            lead_budget_usd: None,
            lead_timeout_secs: None,
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        }
    }

    fn mk_store(dir: &Path) -> Arc<dyn SessionStore> {
        Arc::new(JsonFileStore::new(dir.to_path_buf()))
    }

    #[tokio::test]
    #[serial(env)]
    async fn fresh_run_id_is_minted_when_none_passed() {
        // `PITBOSS_RUN_ID` MUST be unset for this test — otherwise
        // parent_run_id() would see the prior test's value. The
        // `#[serial(env)]` marker serializes against other env-touching
        // tests; we additionally clear here for hygiene.
        std::env::remove_var("PITBOSS_RUN_ID");
        let dir = TempDir::new().unwrap();
        let manifest = mk_manifest(dir.path().to_path_buf());
        let store = mk_store(dir.path());
        let manifest_path = dir.path().join("pitboss.toml");

        let init = init_run_state(
            &manifest,
            "manifest=text",
            &manifest_path,
            None,
            &store,
            None,
            None,
        )
        .await
        .unwrap();

        // UUIDv7's high nibble of byte 6 is `0x7`, so any fresh mint
        // sets the version field to 7. Pinning this rejects regressions
        // that swap minting algorithms (e.g. v4) without anyone noticing.
        assert_eq!(init.run_id.get_version_num(), 7);
        assert!(init.parent_run_id.is_none());
    }

    #[tokio::test]
    #[serial(env)]
    async fn pre_minted_run_id_is_honored() {
        std::env::remove_var("PITBOSS_RUN_ID");
        let dir = TempDir::new().unwrap();
        let manifest = mk_manifest(dir.path().to_path_buf());
        let store = mk_store(dir.path());
        let manifest_path = dir.path().join("pitboss.toml");

        // Fixed UUID from bytes — the workspace only enables uuid's `v7`
        // feature, so `Uuid::new_v4()` would be unavailable. Equality
        // alone proves passthrough: a regression that overwrote our
        // pre-mint with a fresh `now_v7()` would return a different ID.
        let pre_minted = Uuid::from_u128(0x1122_3344_5566_7788_9900_aabb_ccdd_eeff_u128);
        let init = init_run_state(
            &manifest,
            "m",
            &manifest_path,
            None,
            &store,
            Some(pre_minted),
            None,
        )
        .await
        .unwrap();

        assert_eq!(init.run_id, pre_minted);
    }

    #[tokio::test]
    #[serial(env)]
    async fn parent_run_id_snapshotted_from_env() {
        // Set the env var BEFORE calling — init_run_state must capture
        // the parent value into `RunInit.parent_run_id`. The contract
        // documented at notify/parent.rs is the source of truth for
        // what counts as a non-empty parent run id (trim + discard empty).
        std::env::set_var("PITBOSS_RUN_ID", "01234567-89ab-7def-8123-456789abcdef");
        let dir = TempDir::new().unwrap();
        let manifest = mk_manifest(dir.path().to_path_buf());
        let store = mk_store(dir.path());
        let manifest_path = dir.path().join("pitboss.toml");

        let init = init_run_state(&manifest, "m", &manifest_path, None, &store, None, None)
            .await
            .unwrap();

        assert_eq!(
            init.parent_run_id.as_deref(),
            Some("01234567-89ab-7def-8123-456789abcdef")
        );
        std::env::remove_var("PITBOSS_RUN_ID");
    }

    #[tokio::test]
    #[serial(env)]
    async fn run_dir_override_takes_precedence_over_resolved() {
        std::env::remove_var("PITBOSS_RUN_ID");
        let resolved_dir = TempDir::new().unwrap();
        let override_dir = TempDir::new().unwrap();
        let manifest = mk_manifest(resolved_dir.path().to_path_buf());
        let store = mk_store(resolved_dir.path());
        let manifest_path = resolved_dir.path().join("pitboss.toml");

        let init = init_run_state(
            &manifest,
            "m",
            &manifest_path,
            None,
            &store,
            None,
            Some(override_dir.path().to_path_buf()),
        )
        .await
        .unwrap();

        // run_dir picks the override; run_subdir is under it; the
        // resolved manifest's run_dir is NOT consulted in this branch.
        assert_eq!(init.run_dir, override_dir.path());
        assert!(init.run_subdir.starts_with(override_dir.path()));
        assert!(!init.run_subdir.starts_with(resolved_dir.path()));
    }

    #[tokio::test]
    #[serial(env)]
    async fn snapshot_files_written_to_run_subdir() {
        std::env::remove_var("PITBOSS_RUN_ID");
        let dir = TempDir::new().unwrap();
        let manifest = mk_manifest(dir.path().to_path_buf());
        let store = mk_store(dir.path());
        let manifest_path = dir.path().join("pitboss.toml");
        let manifest_text = "name = \"unit-test\"\n";

        let init = init_run_state(
            &manifest,
            manifest_text,
            &manifest_path,
            None,
            &store,
            None,
            None,
        )
        .await
        .unwrap();

        // manifest.snapshot.toml must contain the raw bytes verbatim so
        // resume can re-load + re-substitute env (notifications are
        // deliberately dropped from resolved.json — see resume.rs and
        // dispatch-cli CLAUDE.md).
        let snapshot = tokio::fs::read_to_string(init.run_subdir.join("manifest.snapshot.toml"))
            .await
            .unwrap();
        assert_eq!(snapshot, manifest_text);

        // resolved.json must be valid JSON and re-parse into a
        // ResolvedManifest — `run_dir` is the only field with a
        // non-Default value here so checking it round-trips is the
        // cheapest end-to-end deserialization assertion.
        let resolved_bytes = tokio::fs::read(init.run_subdir.join("resolved.json"))
            .await
            .unwrap();
        let round_trip: ResolvedManifest = serde_json::from_slice(&resolved_bytes).unwrap();
        assert_eq!(round_trip.run_dir, dir.path());
    }
}
