use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use pitboss_core::session::CancelToken;

use crate::dispatch::state::DispatchState;

const SECOND_SIGINT_WINDOW: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------
// SIGSTOP / SIGCONT for freeze-pause
// ---------------------------------------------------------------------
//
// `pause_worker` supports two modes: the classic `cancel` (which
// terminates the claude subprocess and snapshots the session so
// `continue_worker` can re-spawn via `claude --resume`) and the newer
// `freeze` (which SIGSTOP's the process in place and SIGCONT's it
// back). Freeze preserves in-flight state (no token replay, no session
// re-init) but risks Anthropic dropping the HTTP session if the pause
// runs past their server-side idle window. Use cancel for long pauses,
// freeze for quick ones.
//
// We use raw libc::kill rather than the `nix` crate — libc is already a
// transitive dep and this is a two-function use case. Pitboss is
// Linux/macOS only so POSIX signal semantics are available
// unconditionally.
//
// Workers are spawned with `process_group(0)` (see
// `pitboss_core::process::tokio_impl`), so `pid` is also the PGID. We
// signal `-pgid` so freeze/resume reach the entire claude subtree
// (Bash subshells, sub-agents, MCP servers) — otherwise SIGSTOP on the
// parent alone leaves grandchildren running and the freeze illusion
// breaks down (the parent can't service its children's stdio while
// stopped, but the children themselves keep burning CPU and tokens).

/// Suspend the worker process group rooted at `pid` using SIGSTOP.
/// Returns an error for `pid == 0` (slot not yet populated) or if the
/// syscall fails (group already gone, permission denied, etc).
pub fn freeze(pid: u32) -> Result<()> {
    send_group_signal(pid, libc::SIGSTOP, "SIGSTOP")
}

/// Resume a previously-frozen worker process group with SIGCONT.
pub fn resume_stopped(pid: u32) -> Result<()> {
    send_group_signal(pid, libc::SIGCONT, "SIGCONT")
}

fn send_group_signal(pid: u32, sig: libc::c_int, name: &'static str) -> Result<()> {
    if pid == 0 {
        bail!("{name}: pid is 0 (worker not yet spawned?)");
    }
    #[allow(clippy::cast_possible_wrap)]
    let pgid = pid as libc::pid_t;
    // SAFETY: `kill(2)` with a negative pid signals the process group;
    // pgid was published by the spawner after `process_group(0)`, so it
    // is a PGID we own. No memory is dereferenced.
    let rc = unsafe { libc::kill(-pgid, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    bail!("{name} to pgid {pid} failed: {err}")
}

/// Spawn a task that watches for Ctrl-C in two phases:
///   1st SIGINT within window → drain
///   2nd SIGINT within window → terminate
/// After the window, re-armed: a single later SIGINT is treated as a fresh first.
///
/// **Idempotent.** Calling more than once in the same process is a no-op
/// after the first install — pitboss has two callsites today (flat-mode
/// and hierarchical entry points) and a future third callsite would
/// otherwise spawn a duplicate watcher per Ctrl-C. (#150 L15)
pub fn install_ctrl_c_watcher(cancel: CancelToken) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        // Watcher already running. Don't spawn a duplicate — it would
        // doubly drain/terminate on each SIGINT, race the original on
        // re-arm, and turn the documented "second SIGINT within 5s →
        // terminate" contract into "first SIGINT terminates" (because
        // each watcher consumes one keystroke).
        tracing::debug!("install_ctrl_c_watcher: already installed; skipping duplicate");
        return;
    }
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            cancel.drain();
            tracing::warn!("received Ctrl-C — draining; send another within 5s to terminate");
            match tokio::time::timeout(SECOND_SIGINT_WINDOW, tokio::signal::ctrl_c()).await {
                Ok(Ok(_)) => {
                    cancel.terminate();
                    tracing::warn!("received second Ctrl-C — terminating subprocesses");
                    return;
                }
                _ => {
                    tracing::info!("drain window expired; continuing in drain mode");
                    // Loop again: if another Ctrl-C arrives later, start a new window.
                }
            }
        }
    });
}

// ── Kill-with-reason cascade (Task 4.5) ─────────────────────────────────────

/// Cancel any actor in the tree (worker or sub-lead) and optionally deliver
/// a corrective reason to the actor's direct parent lead as a synthetic
/// `[SYSTEM]` reprompt.
///
/// Routing:
/// - Kill a worker in a sub-tree → that sub-lead receives the reason
/// - Kill a root-layer worker → root lead receives the reason
/// - Kill a sub-lead → root lead receives the reason
/// - Kill root / unknown → no reprompt (no parent); reason is logged
///
/// Backward compatible: callers that omit `reason` behave identically to
/// the pre-4.5 cancel path.
pub async fn cancel_actor_with_reason(
    state: &Arc<DispatchState>,
    target: &str,
    reason: Option<String>,
) -> Result<()> {
    // Walk the tree ONCE under held locks: identify the actor, cancel it,
    // and capture its parent layer atomically. A two-pass lookup (find
    // parent, drop lock, re-acquire, cancel) can race with the reaper —
    // if the actor finishes between passes, we'd either bail with "unknown
    // actor" (dropping the reprompt) or identify a parent that has
    // already torn down. Folding both phases under a single tree
    // traversal closes that window.
    let parent_lead_layer = cancel_and_find_parent(state, target).await?;

    if let (Some(reason_text), Some(layer)) = (reason, parent_lead_layer) {
        let synthetic_message = format!(
            "[SYSTEM] Actor {target} was killed by operator.\nReason: {reason_text}\nAdjust your plan accordingly."
        );
        // `send_synthetic_reprompt` itself logs when the layer's delivery
        // channel is absent (layer already terminated), so callers
        // receive a trace rather than a silent drop.
        layer.send_synthetic_reprompt(&synthetic_message).await;
    }

    Ok(())
}

/// Trip the CancelToken for `target` and return the parent layer that
/// should receive a synthetic reprompt. Holds the `subleads` read lock
/// across the whole traversal so cancel + parent-identification are
/// consistent against concurrent mutation.
///
/// Returns `Ok(None)` when the cancel succeeded but the actor has no
/// parent to reprompt (e.g. target is a sub-lead whose parent is the
/// root lead — in that case the function still returns `Some(root)`;
/// `None` is reserved for the root itself, which cannot be cancel-with-
/// reason'd because there is no parent to receive the reason).
async fn cancel_and_find_parent(
    state: &Arc<DispatchState>,
    target: &str,
) -> Result<Option<Arc<crate::dispatch::layer::LayerState>>> {
    // Root-layer workers: parent = root lead.
    {
        let cancels = state.root.worker_cancels.read().await;
        if let Some(tok) = cancels.get(target) {
            tok.terminate();
            return Ok(Some(state.root.clone()));
        }
    }

    // Sub-leads + sub-tree workers share the `subleads` read lock.
    let subleads = state.subleads.read().await;

    // Sub-leads themselves: parent = root lead.
    if let Some(sub_layer) = subleads.get(target) {
        sub_layer.cancel.terminate();
        return Ok(Some(state.root.clone()));
    }

    // Workers in a sub-tree: parent = that sub-tree's LayerState.
    for (_sublead_id, sub_layer) in subleads.iter() {
        let cancels = sub_layer.worker_cancels.read().await;
        if let Some(tok) = cancels.get(target) {
            tok.terminate();
            return Ok(Some(sub_layer.clone()));
        }
    }

    anyhow::bail!("cancel_actor_with_reason: unknown actor id: {target}")
}

/// Install a per-sub-tree cancel-cascade watcher on `sub_layer`.
///
/// Spawns one background task that:
///
/// 1. Waits for the first cancel signal (drain or terminate, whichever
///    arrives first) on `sub_layer.cancel`.
/// 2. Calls `cascade_to_workers` to fan the current cancel state out
///    to every worker registered on this layer at that moment.
/// 3. If the first signal was drain, waits for the (eventual) terminate
///    and fans out again — the second cascade upgrades drained workers
///    to terminated. If the first signal was terminate, exits.
///
/// Funnelling through `LayerState::cascade_to_workers` keeps the
/// terminate-dominates-drain rule in exactly one place
/// (`CancelToken::cascade_to`).
///
/// **Pre- vs. post-registration coverage:** the watcher only fans out
/// to actors registered *before* the signal arrives. Workers registered
/// *after* the signal are covered by the eager cascade in
/// `LayerState::register_worker_cancel` — these two paths together form
/// the full timeline. Pinned by `tests/cancel_cascade_flows.rs`.
///
/// Call exactly once at sub-lead spawn time
/// (see `DispatchState::register_sublead`).
pub fn install_sublead_cancel_watcher(sub_layer: Arc<crate::dispatch::layer::LayerState>) {
    tokio::spawn(async move {
        tokio::select! {
            () = sub_layer.cancel.await_drain() => {}
            () = sub_layer.cancel.await_terminate() => {}
        }
        sub_layer.cascade_to_workers().await;
        if !sub_layer.cancel.is_terminated() {
            sub_layer.cancel.await_terminate().await;
            sub_layer.cascade_to_workers().await;
        }
    });
}

/// Spawn a background task that listens for root cancellation and
/// cascades the signal into every registered sub-tree's `LayerState`.
/// Each sub-tree's per-layer watcher (installed via
/// `install_sublead_cancel_watcher` at spawn time) then cascades to the
/// sub-tree's workers — `DispatchState` never reaches into
/// `sub_layer.worker_cancels` directly.
///
/// Same shape as `install_sublead_cancel_watcher` (one task,
/// drain-then-terminate-aware) — see that function's doc-comment for
/// the lifecycle and pre/post-registration semantics.
///
/// **Idempotency:** Call exactly once per dispatch run. Subsequent calls
/// spawn additional watcher tasks; the result is benign (drain/terminate
/// are idempotent on the watch::Sender) but creates duplicate tracing
/// output.
pub fn install_cascade_cancel_watcher(state: Arc<DispatchState>) {
    tokio::spawn(async move {
        tokio::select! {
            () = state.root.cancel.await_drain() => {}
            () = state.root.cancel.await_terminate() => {}
        }
        state.cascade_to_subleads().await;
        if !state.root.cancel.is_terminated() {
            state.root.cancel.await_terminate().await;
            state.cascade_to_subleads().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_rejects_pid_zero() {
        let err = freeze(0).unwrap_err();
        assert!(err.to_string().contains("pid is 0"));
    }

    #[test]
    fn resume_rejects_pid_zero() {
        let err = resume_stopped(0).unwrap_err();
        assert!(err.to_string().contains("pid is 0"));
    }

    /// End-to-end: spawn a sleeping child, SIGSTOP it, confirm
    /// /proc reports stopped state, SIGCONT, confirm runnable,
    /// then clean up. Linux-only because /proc isn't available on
    /// macOS CI.
    ///
    /// Polls `/proc/<pid>/status` instead of a fixed sleep: the
    /// kernel can briefly report `D` (uninterruptible disk sleep)
    /// or `R` (running) between signal delivery and the process
    /// actually being descheduled, especially on slow CI runners.
    /// We only care that the state *eventually* reaches the
    /// expected one within a generous window.
    #[cfg(target_os = "linux")]
    #[test]
    fn freeze_then_resume_flips_proc_state() {
        use std::os::unix::process::CommandExt;
        use std::process::Command;

        // Spawn the test child in its own process group (matches what
        // TokioSpawner does in production). freeze()/resume_stopped()
        // signal `-pgid` so without this the test would deliver SIGSTOP
        // to the cargo-test process group itself.
        let mut cmd = Command::new("sleep");
        cmd.arg("30").process_group(0);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();

        freeze(pid).unwrap();
        assert!(
            wait_for_state(pid, &['T', 't'], Duration::from_secs(2)),
            "expected stopped state within 2s, final state: {:?}",
            read_proc_state(pid)
        );

        resume_stopped(pid).unwrap();
        assert!(
            wait_for_state(pid, &['S', 'R'], Duration::from_secs(2)),
            "expected sleeping/running state within 2s, final state: {:?}",
            read_proc_state(pid)
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    /// `install_sublead_cancel_watcher` cascades drain to registered workers.
    #[tokio::test]
    async fn sublead_watcher_cascades_drain_to_workers() {
        use crate::dispatch::layer::LayerState;
        use crate::dispatch::state::ApprovalPolicy;
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 1,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(1),
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
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let shared = Arc::new(crate::shared_store::SharedStore::new());

        let sub_layer = Arc::new(LayerState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "sublead-1".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            ApprovalPolicy::AutoApprove,
            None,
            shared,
            None,
        ));

        // Register two worker cancel tokens.
        let w1 = CancelToken::new();
        let w2 = CancelToken::new();
        {
            let mut cancels = sub_layer.worker_cancels.write().await;
            cancels.insert("w1".into(), w1.clone());
            cancels.insert("w2".into(), w2.clone());
        }

        install_sublead_cancel_watcher(sub_layer.clone());

        // Trip the sublead cancel token.
        sub_layer.cancel.drain();

        // Workers should be drained promptly (give the spawned task time to run).
        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if w1.is_draining() && w2.is_draining() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("worker drain should have cascaded within 200ms");
    }

    /// `install_sublead_cancel_watcher` cascades terminate to registered workers.
    #[tokio::test]
    async fn sublead_watcher_cascades_terminate_to_workers() {
        use crate::dispatch::layer::LayerState;
        use crate::dispatch::state::ApprovalPolicy;
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 1,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(1),
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
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let shared = Arc::new(crate::shared_store::SharedStore::new());

        let sub_layer = Arc::new(LayerState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "sublead-1".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            ApprovalPolicy::AutoApprove,
            None,
            shared,
            None,
        ));

        let w1 = CancelToken::new();
        {
            let mut cancels = sub_layer.worker_cancels.write().await;
            cancels.insert("w1".into(), w1.clone());
        }

        install_sublead_cancel_watcher(sub_layer.clone());

        // Terminate the sublead cancel (skipping drain).
        sub_layer.cancel.terminate();

        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if w1.is_terminated() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("worker terminate should have cascaded within 200ms");
    }

    /// `install_sublead_cancel_watcher` upgrades drained workers to
    /// terminated when terminate fires after drain. Pins the
    /// drain-then-terminate path through the unified watcher task
    /// (drain wakes the select, cascade applies drain, watcher then
    /// awaits terminate, second cascade upgrades the worker).
    #[tokio::test]
    async fn sublead_watcher_upgrades_drained_workers_to_terminated() {
        use crate::dispatch::layer::LayerState;
        use crate::dispatch::state::ApprovalPolicy;
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;
        use tempfile::TempDir;
        use uuid::Uuid;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: 1,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(1),
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
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let shared = Arc::new(crate::shared_store::SharedStore::new());

        let sub_layer = Arc::new(LayerState::new(
            Uuid::now_v7(),
            manifest,
            store,
            CancelToken::new(),
            "sublead-1".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            dir.path().to_path_buf(),
            ApprovalPolicy::AutoApprove,
            None,
            shared,
            None,
        ));

        let w1 = CancelToken::new();
        {
            let mut cancels = sub_layer.worker_cancels.write().await;
            cancels.insert("w1".into(), w1.clone());
        }

        install_sublead_cancel_watcher(sub_layer.clone());

        // First, drain. Watcher should fan out drain to the worker.
        sub_layer.cancel.drain();
        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if w1.is_draining() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("drain cascade should reach worker within 200ms");

        // Now terminate. The watcher's second await should fire and
        // upgrade the worker from drained to terminated.
        sub_layer.cancel.terminate();
        tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if w1.is_terminated() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("terminate cascade should reach drained worker within 200ms");
    }

    #[cfg(target_os = "linux")]
    fn read_proc_state(pid: u32) -> Option<char> {
        let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("State:") {
                return rest.trim().chars().next();
            }
        }
        None
    }

    /// Poll `/proc/<pid>/status` every 10ms until the State field is
    /// one of `expected`, or `timeout` elapses. Returns true on match.
    #[cfg(target_os = "linux")]
    fn wait_for_state(pid: u32, expected: &[char], timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(c) = read_proc_state(pid) {
                if expected.contains(&c) {
                    return true;
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
