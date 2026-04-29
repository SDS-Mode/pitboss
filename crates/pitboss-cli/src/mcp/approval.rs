//! Approval bridge: wires an in-dispatcher MCP `request_approval` call to
//! one of three resolution paths, in order:
//!
//!   1. **Operator-declared `[[approval_policy]]` rule match** —
//!      handled by callers (`handle_request_approval` etc.) before
//!      they reach the bridge; rules with `auto_approve` / `auto_reject`
//!      / `block` actions short-circuit unconditionally.
//!   2. **Manifest `default_approval_policy`** (`ApprovalPolicy`) —
//!      `AutoApprove` and `AutoReject` short-circuit unconditionally
//!      from inside `request()` regardless of whether a TUI/web
//!      console is attached. Pre-v0.9.2 these only fired when no TUI
//!      was connected; a connected console silently bypassed the
//!      policy. The current behavior matches what the field name
//!      implies — a *policy*, not a headless fallback.
//!   3. **`Block` policy** (the default) — if a TUI is attached the
//!      request is routed to it via the control socket; if no TUI
//!      is attached the request is queued for the next connect.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::dispatch::state::{ApprovalPolicy, ApprovalResponse, DispatchState, QueuedApproval};

// ── Rich approval-record types (Phase 4) ────────────────────────────────────

/// Re-export of `mcp::tools::ApprovalPlan` so callers can refer to it from
/// this module (keeps `dispatch::state::PendingApproval`'s plan field type
/// in the same module as the other approval types).
pub type ApprovalPlan = crate::mcp::tools::ApprovalPlan;

/// Discriminator for what kind of action an approval covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalCategory {
    ToolUse,
    Plan,
    Cost,
    Other,
}

/// What happens when a `PendingApproval` exceeds its `ttl_secs` with no
/// operator response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalFallback {
    /// Treat as rejected when TTL elapses (safe default).
    #[default]
    AutoReject,
    /// Treat as approved when TTL elapses (permissive — use with care).
    AutoApprove,
    /// Never auto-resolve; block until an operator acts (sticky).
    Block,
}

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("approval request timed out")]
    Timeout,
    #[error("approval request cancelled")]
    Cancelled,
    #[error("control socket disconnected mid-request")]
    ControlDisconnected,
}

pub struct ApprovalBridge {
    state: Arc<DispatchState>,
}

/// Convert the MCP-tool-layer `ApprovalPlan` into the control-protocol
/// wire shape. Field layout is identical; the duplication exists so
/// `control::protocol` stays independent of `mcp::tools`.
pub(crate) fn approval_plan_to_wire(
    p: crate::mcp::tools::ApprovalPlan,
) -> crate::control::protocol::ApprovalPlanWire {
    let crate::mcp::tools::ApprovalPlan {
        summary,
        rationale,
        resources,
        risks,
        rollback,
    } = p;
    crate::control::protocol::ApprovalPlanWire {
        summary,
        rationale,
        resources,
        risks,
        rollback,
    }
}

impl ApprovalBridge {
    pub fn new(state: Arc<DispatchState>) -> Self {
        Self { state }
    }

    /// Request operator approval. Blocks until either (a) the operator
    /// responds, (b) the policy auto-resolves, or (c) `timeout` elapses.
    /// `plan` carries the typed structured fields (rationale, resources,
    /// risks, rollback); pass `None` for simple summary-only approvals.
    /// `kind` distinguishes `request_approval` (in-flight, `Action`) from
    /// `propose_plan` (pre-flight, `Plan`) — passed through to the TUI
    /// modal so the operator can tell them apart.
    #[allow(clippy::too_many_arguments)]
    pub async fn request(
        &self,
        task_id: String,
        summary: String,
        plan: Option<crate::mcp::tools::ApprovalPlan>,
        kind: crate::control::protocol::ApprovalKind,
        timeout: Duration,
        ttl_secs: Option<u64>,
        fallback: Option<ApprovalFallback>,
    ) -> Result<ApprovalResponse, ApprovalError> {
        let request_id = format!("req-{}", Uuid::now_v7());
        // Clone before task_id is moved into bridge/queue/event structures below.
        let task_id_for_counter = task_id.clone();
        let _ = crate::dispatch::events::append_event(
            &self.state.root.run_subdir,
            &self.state.root.lead_id,
            &crate::dispatch::events::TaskEvent::ApprovalRequest {
                at: chrono::Utc::now(),
                request_id: request_id.clone(),
                summary_preview: summary.chars().take(80).collect(),
            },
        )
        .await;

        if let Some(router) = self.state.root.notification_router.clone() {
            let envelope = crate::notify::NotificationEnvelope::new(
                &self.state.root.run_id.to_string(),
                crate::notify::Severity::Warning,
                crate::notify::PitbossEvent::ApprovalRequest {
                    request_id: request_id.clone(),
                    task_id: task_id.clone(),
                    summary: summary.clone(),
                },
                chrono::Utc::now(),
            );
            let _ = router.dispatch(envelope).await;
        }

        let (tx, rx) = oneshot::channel::<ApprovalResponse>();

        // Unconditional policy short-circuit. Pre-fix `default_approval_policy`
        // only applied when no TUI was attached: a connected web console or
        // pitboss-tui silently bypassed `auto_approve` / `auto_reject` and
        // routed every approval to the operator. That made the field mean
        // two different things depending on whether someone happened to be
        // watching. Now `auto_approve` / `auto_reject` are always honored;
        // `block` retains the legacy "ask operator if attached, else queue"
        // path. Operators who want manual review with a fallback should use
        // `[[approval_policy]]` rules (which already short-circuit) and
        // leave `default_approval_policy` at its default of `block`.
        match self.state.root.approval_policy {
            ApprovalPolicy::AutoApprove => {
                let _ = tx.send(ApprovalResponse {
                    approved: true,
                    comment: None,
                    edited_summary: None,
                    reason: None,
                    from_ttl: false,
                });
                self.state
                    .root
                    .worker_counters
                    .write()
                    .await
                    .entry(task_id_for_counter.clone())
                    .or_default()
                    .approvals_requested += 1;
                return rx.await.map_err(|_| ApprovalError::Cancelled);
            }
            ApprovalPolicy::AutoReject => {
                let _ = tx.send(ApprovalResponse {
                    approved: false,
                    comment: Some("auto-rejected by default_approval_policy".into()),
                    edited_summary: None,
                    reason: None,
                    from_ttl: false,
                });
                self.state
                    .root
                    .worker_counters
                    .write()
                    .await
                    .entry(task_id_for_counter.clone())
                    .or_default()
                    .approvals_requested += 1;
                return rx.await.map_err(|_| ApprovalError::Cancelled);
            }
            ApprovalPolicy::Block => {
                // Fall through to TUI/queue routing below.
            }
        }

        // Hold the control_writer lock across the `bridge.insert` and the
        // `w.send(ev)`. A previous version released the writer lock between
        // the "is it present?" check and the insert, so a TUI that
        // disconnected in that window would leave the responder orphaned
        // in `approval_bridge` — stuck until the bridge timeout.
        let writer_guard = self.state.root.control_writer.lock().await;

        if let Some(w) = writer_guard.as_ref() {
            self.state.root.approval_bridge.lock().await.insert(
                request_id.clone(),
                crate::dispatch::state::BridgeEntry {
                    responder: tx,
                    task_id: task_id.clone(),
                    summary: summary.clone(),
                    plan: plan.clone(),
                    kind,
                    ttl_secs,
                    fallback,
                    created_at: chrono::Utc::now(),
                },
            );
            let ev = crate::control::protocol::ControlEvent::ApprovalRequest {
                request_id: request_id.clone(),
                task_id,
                summary,
                plan: plan.map(approval_plan_to_wire),
                kind,
            };
            // Best-effort send. A full queue used to silently drop the
            // event AND leave the bridge entry alive, so the caller
            // waited the full timeout for an ApprovalRequest the TUI
            // would never see. Now we evict the bridge entry on
            // try_send failure and route through the policy's
            // fallback path (timeout/auto-allow/reject) immediately
            // — same outcome as a permanently-disconnected TUI but
            // without the bridge-timeout stall. (#151 M4)
            if let Err(e) = w.sender.try_send(ev) {
                tracing::warn!(
                    request_id = %request_id,
                    error = %e,
                    "control writer queue full; evicting bridge entry and falling through \
                     to policy fallback"
                );
                // Remove the bridge entry we just inserted so the
                // responder isn't orphaned.
                self.state
                    .root
                    .approval_bridge
                    .lock()
                    .await
                    .remove(&request_id);
                drop(writer_guard);
                // Block policy + queue full + no TUI can decide → safe
                // rejection. (AutoApprove / AutoReject already
                // short-circuited at the top of `request`, so policy is
                // guaranteed to be Block here.) Pre-fix the caller would
                // block until bridge timeout for the same outcome.
                return Ok(ApprovalResponse {
                    approved: false,
                    comment: None,
                    edited_summary: None,
                    reason: Some(
                        "control writer queue full and approval_policy=block — no TUI \
                         can decide; auto-rejected"
                            .into(),
                    ),
                    from_ttl: false,
                });
            }
            drop(writer_guard);
        } else {
            drop(writer_guard);
            // No TUI attached and policy is `Block`: queue the request and
            // drain when a TUI connects (see control/server.rs). AutoApprove
            // / AutoReject already short-circuited above, so this branch is
            // reachable only under `block`.
            self.state
                .root
                .approval_queue
                .lock()
                .await
                .push_back(QueuedApproval {
                    request_id: request_id.clone(),
                    task_id: task_id.clone(),
                    summary: summary.clone(),
                    plan,
                    kind,
                    responder: tx,
                    ttl_secs,
                    fallback,
                    created_at: chrono::Utc::now(),
                });

            // Fire approval_pending notification.
            if let Some(router) = self.state.root.notification_router.clone() {
                let envelope = crate::notify::NotificationEnvelope::new(
                    &self.state.root.run_id.to_string(),
                    crate::notify::Severity::Warning,
                    crate::notify::PitbossEvent::ApprovalPending {
                        request_id: request_id.clone(),
                        task_id: task_id.clone(),
                        summary: summary.clone(),
                    },
                    chrono::Utc::now(),
                );
                let _ = router.dispatch(envelope).await;
            }
        }

        // task_id may have been moved into the ControlEvent or QueuedApproval
        // above; use the request_id as the lookup key is not right — we need
        // the caller id. Both branches clone task_id before consuming it, so
        // we capture a clone here at the top of the function instead.
        // (The clone is taken at function entry via task_id_for_counter below.)
        self.state
            .root
            .worker_counters
            .write()
            .await
            .entry(task_id_for_counter.clone())
            .or_default()
            .approvals_requested += 1;

        // When a per-request TTL is set, the TTL watcher fires the response
        // with from_ttl=true before the bridge timeout. Add 60 s of buffer so
        // the TTL watcher always wins the race; the bridge timeout then becomes
        // a safety net only.
        let effective_timeout = match ttl_secs {
            Some(t) => Duration::from_secs(t + 60),
            None => timeout,
        };

        match tokio::time::timeout(effective_timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(ApprovalError::Cancelled),
            Err(_) => {
                // Timeout: remove the pending entry so a late respond doesn't panic.
                self.state
                    .root
                    .approval_bridge
                    .lock()
                    .await
                    .remove(&request_id); // removes BridgeEntry; responder dropped
                                          // Also evict from the TUI-drain queue (Block policy, no TUI connected).
                                          // Without this, a TUI that connects after the timeout fires sees a stale
                                          // approval modal for a request that has already resolved.
                self.state
                    .root
                    .approval_queue
                    .lock()
                    .await
                    .retain(|q| q.request_id != request_id);
                Err(ApprovalError::Timeout)
            }
        }
    }

    /// Respond to a pending request from the control side.
    pub async fn respond(
        &self,
        request_id: &str,
        resp: ApprovalResponse,
    ) -> Result<(), ApprovalError> {
        let bridge_entry = self
            .state
            .root
            .approval_bridge
            .lock()
            .await
            .remove(request_id)
            .ok_or(ApprovalError::ControlDisconnected)?;
        let _ = crate::dispatch::events::append_event(
            &self.state.root.run_subdir,
            &self.state.root.lead_id,
            &crate::dispatch::events::TaskEvent::ApprovalResponse {
                at: chrono::Utc::now(),
                request_id: request_id.to_string(),
                approved: resp.approved,
                edited: resp.edited_summary.is_some(),
            },
        )
        .await;
        let approved = resp.approved;
        let caller_id = bridge_entry.task_id.clone();
        bridge_entry
            .responder
            .send(resp)
            .map_err(|_| ApprovalError::Cancelled)?;
        {
            let mut guard = self.state.root.worker_counters.write().await;
            let entry = guard.entry(caller_id).or_default();
            if approved {
                entry.approvals_approved += 1;
            } else {
                entry.approvals_rejected += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::DispatchState;
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use pitboss_core::process::{ProcessSpawner, TokioSpawner};
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
    use std::path::PathBuf;
    use tempfile::TempDir;
    use uuid::Uuid;

    async fn mk_state(policy: ApprovalPolicy) -> Arc<DispatchState> {
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
            budget_usd: Some(1.0),
            lead_timeout_secs: None,
            default_approval_policy: Some(policy),
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
        let run_id = Uuid::now_v7();
        std::mem::forget(dir);
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            PathBuf::from("/tmp"),
            policy,
            None,
            std::sync::Arc::new(crate::shared_store::SharedStore::new()),
        ))
    }

    #[tokio::test]
    async fn auto_approve_returns_immediately() {
        let state = mk_state(ApprovalPolicy::AutoApprove).await;
        let bridge = ApprovalBridge::new(state);
        let resp = bridge
            .request(
                "lead".into(),
                "spawn 2".into(),
                None,
                crate::control::protocol::ApprovalKind::Action,
                Duration::from_secs(1),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(resp.approved);
    }

    #[tokio::test]
    async fn auto_reject_returns_immediately() {
        let state = mk_state(ApprovalPolicy::AutoReject).await;
        let bridge = ApprovalBridge::new(state);
        let resp = bridge
            .request(
                "lead".into(),
                "spawn 2".into(),
                None,
                crate::control::protocol::ApprovalKind::Action,
                Duration::from_secs(1),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(!resp.approved);
        assert_eq!(
            resp.comment.as_deref(),
            Some("auto-rejected by default_approval_policy")
        );
    }

    /// Regression for the v0.9.2 semantic change: `auto_approve` MUST
    /// short-circuit even when a TUI/web console is attached. Pre-fix
    /// the bridge routed every approval to the operator whenever the
    /// control writer was connected, silently bypassing the policy.
    #[tokio::test]
    async fn auto_approve_short_circuits_with_tui_attached() {
        use crate::control::protocol::ControlEvent;
        use tokio::sync::mpsc;

        let state = mk_state(ApprovalPolicy::AutoApprove).await;
        // Simulate a connected TUI by registering a control writer.
        let (tx, mut rx) = mpsc::channel::<ControlEvent>(8);
        *state.root.control_writer.lock().await = Some(crate::dispatch::layer::ControlWriterSlot {
            id: Uuid::now_v7(),
            sender: tx,
        });

        let bridge = ApprovalBridge::new(state);
        let resp = bridge
            .request(
                "lead".into(),
                "spawn 2".into(),
                None,
                crate::control::protocol::ApprovalKind::Action,
                Duration::from_secs(1),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            resp.approved,
            "auto_approve must short-circuit unconditionally"
        );
        // The TUI must NOT have received an ApprovalRequest event — that
        // was the pre-fix bug, where a connected console silently overrode
        // the policy.
        assert!(
            rx.try_recv().is_err(),
            "auto_approve must not route the request to the TUI"
        );
    }

    /// Regression for the v0.9.2 semantic change: `auto_reject` MUST
    /// short-circuit even when a TUI/web console is attached.
    #[tokio::test]
    async fn auto_reject_short_circuits_with_tui_attached() {
        use crate::control::protocol::ControlEvent;
        use tokio::sync::mpsc;

        let state = mk_state(ApprovalPolicy::AutoReject).await;
        let (tx, mut rx) = mpsc::channel::<ControlEvent>(8);
        *state.root.control_writer.lock().await = Some(crate::dispatch::layer::ControlWriterSlot {
            id: Uuid::now_v7(),
            sender: tx,
        });

        let bridge = ApprovalBridge::new(state);
        let resp = bridge
            .request(
                "lead".into(),
                "spawn 2".into(),
                None,
                crate::control::protocol::ApprovalKind::Action,
                Duration::from_secs(1),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(
            !resp.approved,
            "auto_reject must short-circuit unconditionally"
        );
        assert!(
            rx.try_recv().is_err(),
            "auto_reject must not route the request to the TUI"
        );
    }

    #[tokio::test]
    async fn block_policy_times_out_with_no_tui() {
        let state = mk_state(ApprovalPolicy::Block).await;
        let bridge = ApprovalBridge::new(state);
        let err = bridge
            .request(
                "lead".into(),
                "spawn 2".into(),
                None,
                crate::control::protocol::ApprovalKind::Action,
                Duration::from_millis(50),
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ApprovalError::Timeout));
    }
}
