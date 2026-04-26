//! Approval bridge: wires an in-dispatcher MCP `request_approval` call to
//! either (a) a connected TUI via the control socket, or (b) an auto-resolve
//! per `ApprovalPolicy` when no TUI is attached.

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
            // Best-effort send. A full queue drops the event; the bridge
            // timeout will then expire and the caller sees a timeout —
            // same end state as a permanently-disconnected TUI (#110).
            if let Err(e) = w.sender.try_send(ev) {
                tracing::warn!(
                    request_id = %request_id,
                    error = %e,
                    "control writer queue full; TUI will not see ApprovalRequest until reconnect"
                );
            }
            drop(writer_guard);
        } else {
            drop(writer_guard);
            // No TUI attached: policy decides.
            match self.state.root.approval_policy {
                ApprovalPolicy::AutoApprove => {
                    let _ = tx.send(ApprovalResponse {
                        approved: true,
                        comment: None,
                        edited_summary: None,
                        reason: None,
                        from_ttl: false,
                    });
                }
                ApprovalPolicy::AutoReject => {
                    let _ = tx.send(ApprovalResponse {
                        approved: false,
                        comment: Some("no operator available".into()),
                        edited_summary: None,
                        reason: None,
                        from_ttl: false,
                    });
                }
                ApprovalPolicy::Block => {
                    // Queue; drain when a TUI connects (see control/server.rs).
                    // Pass ttl_secs + fallback from the request so the TTL watcher
                    // can expire and fire a from_ttl=true response before the
                    // bridge timeout fires (which would return a generic error).
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

                    // Fire approval_pending notification
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
        assert_eq!(resp.comment.as_deref(), Some("no operator available"));
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
