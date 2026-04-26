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

/// Suspend the process with `pid` using SIGSTOP. Returns an error for
/// `pid == 0` (slot not yet populated) or if the syscall fails
/// (process already exited, permission denied, etc).
pub fn freeze(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGSTOP, "SIGSTOP")
}

/// Resume a previously-frozen process with SIGCONT.
pub fn resume_stopped(pid: u32) -> Result<()> {
    send_signal(pid, libc::SIGCONT, "SIGCONT")
}

fn send_signal(pid: u32, sig: libc::c_int, name: &'static str) -> Result<()> {
    if pid == 0 {
        bail!("{name}: pid is 0 (worker not yet spawned?)");
    }
    // SAFETY: `kill(2)` accepts any pid_t + valid signo; no memory is
    // dereferenced. The cast is libc's expected pid_t shape.
    let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    bail!("{name} to pid {pid} failed: {err}")
}

/// Spawn a task that watches for Ctrl-C in two phases:
///   1st SIGINT within window → drain
///   2nd SIGINT within window → terminate
/// After the window, re-armed: a single later SIGINT is treated as a fresh first.
pub fn install_ctrl_c_watcher(cancel: CancelToken) {
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
/// Spawns two background tasks that mirror the per-layer variant of the root
/// cascade watcher:
///
/// * **drain watcher**: fires when `sub_layer.cancel` is drained → drains
///   every worker cancel token registered on that layer at that moment.
/// * **terminate watcher**: fires when `sub_layer.cancel` is terminated →
///   terminates every worker cancel token registered on that layer.
///
/// Call exactly once at sub-lead spawn time (see `spawn_sublead`).  The root
/// cascade watcher (`install_cascade_cancel_watcher`) only needs to signal
/// `sub_layer.cancel`; this function handles the worker cascade so
/// `DispatchState` never touches `sub_layer.worker_cancels` directly.
pub fn install_sublead_cancel_watcher(sub_layer: Arc<crate::dispatch::layer::LayerState>) {
    let layer_drain = sub_layer.clone();
    tokio::spawn(async move {
        layer_drain.cancel.await_drain().await;
        let worker_cancels = layer_drain.worker_cancels.read().await;
        for (worker_id, tok) in worker_cancels.iter() {
            tracing::debug!(worker_id = %worker_id, "cascading drain to sub-tree worker");
            tok.drain();
        }
    });

    let layer_term = sub_layer;
    tokio::spawn(async move {
        layer_term.cancel.await_terminate().await;
        let worker_cancels = layer_term.worker_cancels.read().await;
        for (worker_id, tok) in worker_cancels.iter() {
            tracing::debug!(worker_id = %worker_id, "cascading terminate to sub-tree worker");
            tok.terminate();
        }
    });
}

/// Spawn a background task that listens for root cancellation and
/// cascades the signal into every registered sub-tree `LayerState` by
/// tripping each sub-tree's own cancel token.  Each sub-tree's per-layer
/// watcher (installed via `install_sublead_cancel_watcher` at spawn time)
/// then cascades to the sub-tree's workers — `DispatchState` never
/// reaches into `sub_layer.worker_cancels` directly.
///
/// **Idempotency:** Call exactly once per dispatch run. Subsequent calls spawn
/// additional watcher tasks; the result is benign (drain is idempotent on the
/// watch::Sender) but creates duplicate tracing output.
///
/// **Post-drain registration:** The watcher self-terminates after one cascade
/// fire — re-installing after the cascade has fired will not catch sub-trees
/// registered post-cascade. For that, see the spawn-time check in `spawn_sublead`.
pub fn install_cascade_cancel_watcher(state: Arc<DispatchState>) {
    let root_cancel_drain = state.root.cancel.clone();
    let state_drain = state.clone();
    tokio::spawn(async move {
        root_cancel_drain.await_drain().await;
        let subleads = state_drain.subleads.read().await;
        for (sublead_id, sub_layer) in subleads.iter() {
            tracing::info!(sublead_id = %sublead_id, "cascading drain to sub-tree");
            sub_layer.cancel.drain();
        }
    });

    let root_cancel_term = state.root.cancel.clone();
    tokio::spawn(async move {
        root_cancel_term.await_terminate().await;
        let subleads = state.subleads.read().await;
        for (sublead_id, sub_layer) in subleads.iter() {
            tracing::info!(sublead_id = %sublead_id, "cascading terminate to sub-tree");
            sub_layer.cancel.terminate();
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
        use std::process::Command;

        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
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
