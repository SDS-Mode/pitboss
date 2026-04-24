//! Wire protocol for the per-run control socket.
//!
//! Messages are **one JSON object per line** (UTF-8, LF-terminated).
//! Client → server messages are `ControlOp`; server → client are `ControlEvent`.
//! Serialization uses `#[serde(tag = "op")]` / `#[serde(tag = "event")]` so the
//! discriminator lives in the same object as the payload fields.

#![allow(dead_code)] // Wired up by control::server in Task 4+.

use serde::{Deserialize, Serialize};

use crate::dispatch::actor::ActorPath;

/// Wrapper that adds tree-lineage metadata to every outbound control-plane
/// event. The `actor_path` field is omitted from the wire when the path is
/// empty (`skip_serializing_if = "ActorPath::is_empty"`), which preserves
/// exact backward-compatibility with v0.5 TUI clients that parse
/// `ControlEvent` lines directly: their JSON remains unchanged.
///
/// v0.6+ TUI clients that understand `EventEnvelope` deserialize the full
/// envelope; v0.5 clients that only understand `ControlEvent` can still
/// parse the flattened event fields because `#[serde(flatten)]` inlines
/// all `ControlEvent` fields into the same JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Tree path from root to the actor that produced this event.
    /// Absent on the wire (and thus backward-compatible) when the path is
    /// empty (e.g. run-level events with no actor context).
    #[serde(default, skip_serializing_if = "ActorPath::is_empty")]
    pub actor_path: ActorPath,
    /// The actual event payload, inlined into the same JSON object.
    #[serde(flatten)]
    pub event: ControlEvent,
}

/// Wire-format mirror of `mcp::tools::ApprovalPlan`. Duplicated here
/// so `control::protocol` doesn't depend on the MCP tool module.
/// Same field names + serde layout so the two types round-trip
/// through JSON identically.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalPlanWire {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<String>,
}

/// Pause-mode selector shared with the MCP tool schema. Duplicated here
/// so the control protocol doesn't depend on `mcp::tools`. Kept in sync
/// by hand; if these diverge you'll notice because the wire values stop
/// round-tripping.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PauseMode {
    #[default]
    Cancel,
    Freeze,
}

fn is_default_pause_mode(m: &PauseMode) -> bool {
    matches!(m, PauseMode::Cancel)
}

/// Discriminator on an approval request: does the operator's y/n gate
/// a single in-flight action, or the whole run's pre-flight plan?
///
/// `Action` (default) = `request_approval` tool. Mid-run, per-action.
/// `Plan` = `propose_plan` tool. Pre-flight, gates `spawn_worker` when
/// `[run].require_plan_approval = true`.
///
/// Field is `#[serde(default)]` so pre-v0.4.5 TUI clients that don't
/// know about `kind` still parse `ApprovalRequest` events and render
/// the modal exactly as before (as `Action`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalKind {
    #[default]
    Action,
    Plan,
}

fn is_default_approval_kind(k: &ApprovalKind) -> bool {
    matches!(k, ApprovalKind::Action)
}

/// An operation sent from the TUI (client) to the dispatcher (server).
///
/// Note: `Eq` is NOT derived — `UpdatePolicy` carries `Vec<ApprovalRule>`,
/// whose `ApprovalMatch.cost_over: Option<f64>` does not implement `Eq`.
/// Use `PartialEq` comparisons; `assert_eq!` continues to work correctly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlOp {
    Hello {
        client_version: String,
    },
    CancelWorker {
        task_id: String,
    },
    CancelRun,
    PauseWorker {
        task_id: String,
        /// Optional pause mode: `"cancel"` (default, backward-compat) or
        /// `"freeze"`. Absent on the wire for pre-v0.4.5 TUI clients —
        /// `#[serde(default)]` yields `Cancel`.
        #[serde(default, skip_serializing_if = "is_default_pause_mode")]
        mode: PauseMode,
    },
    ContinueWorker {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    RepromptWorker {
        task_id: String,
        prompt: String,
    },
    Approve {
        request_id: String,
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        comment: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edited_summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    ListWorkers,
    /// Replace the dispatcher's live `[[approval_policy]]` rule set. Takes
    /// effect immediately: the next `evaluate_policy` call uses these rules.
    /// An empty `rules` vec removes all declarative rules (every approval
    /// falls through to the legacy `ApprovalPolicy` / operator-queue path).
    UpdatePolicy {
        rules: Vec<crate::mcp::policy::ApprovalRule>,
    },
}

/// An event pushed from the dispatcher (server) to the TUI (client).
///
/// Note: `Eq` is intentionally NOT derived — the `SubleadSpawned` and
/// `SubleadTerminated` variants carry `f64` fields (budget/spend amounts)
/// which do not implement `Eq`. Use `PartialEq` comparisons instead.
/// `assert_eq!` in tests only requires `PartialEq + Debug` and continues to
/// work correctly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ControlEvent {
    Hello {
        server_version: String,
        run_id: String,
        run_kind: String,
        workers: Vec<String>,
        /// Current `[[approval_policy]]` rule set at connect time. Empty when
        /// no declarative rules are configured. `#[serde(default)]` keeps the
        /// field backward-compatible with pre-v0.8 dispatchers that don't send
        /// it — they simply see no rules (the TUI editor starts from blank).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        policy_rules: Vec<crate::mcp::policy::ApprovalRule>,
    },
    OpAcked {
        op: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    OpFailed {
        op: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
        error: String,
    },
    OpUnknown {
        op: String,
    },
    OpUnknownState {
        op: String,
        task_id: String,
        current_state: String,
    },
    ApprovalRequest {
        request_id: String,
        task_id: String,
        summary: String,
        /// Typed approval plan with rationale / resources / risks /
        /// rollback. `None` for requests that only sent a bare summary —
        /// the TUI falls back to rendering `summary` in that case, so
        /// pre-v0.4.5 dispatchers + simple approvals still work.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan: Option<ApprovalPlanWire>,
        /// Action (in-flight) vs Plan (pre-flight). Defaults to Action
        /// on the wire when absent, so pre-v0.4.5 dispatchers roundtrip.
        #[serde(default, skip_serializing_if = "is_default_approval_kind")]
        kind: ApprovalKind,
    },
    WorkersSnapshot {
        workers: Vec<WorkerSnapshotEntry>,
    },
    Superseded,
    RunFinished {
        summary: RunFinishedSummary,
    },
    /// Periodic broadcast of per-actor shared-store activity. Emitted
    /// once per `STORE_ACTIVITY_INTERVAL` (~1 s) while there's an active
    /// TUI connection. TUI uses this to render `kv:N lease:M` inside
    /// each grid tile so operators can see store utilization at a glance.
    StoreActivity {
        counters: Vec<ActorActivityEntry>,
    },
    /// A sub-lead was successfully spawned and its LayerState is registered.
    /// Emitted by `dispatch::sublead::spawn_sublead` after the sub-tree is
    /// fully initialised and inserted into `state.subleads`.
    SubleadSpawned {
        sublead_id: String,
        /// Sub-lead's own budget cap (USD). `None` for shared-pool mode
        /// (read_down=true, no explicit allocation).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_usd: Option<f64>,
        /// Maximum workers the sub-lead may spawn concurrently. `None`
        /// for shared-pool mode.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_workers: Option<u32>,
        /// Whether the sub-lead was spawned with read_down=true (can read
        /// root-layer KV keys).
        read_down: bool,
    },
    /// A worker, sub-lead, or lead claude subprocess exited non-zero and
    /// was classified with a structured `FailureReason`. Emitted from the
    /// per-actor completion site (runner / sublead / hierarchical lead /
    /// run_worker / continue_worker resume path) alongside the
    /// `TaskRecord` persist. The TUI renders this in the tile footer so
    /// operators see *why* something failed without opening logs; parent
    /// leads consume it to back off on `RateLimit` or fail fast on
    /// `AuthFailure`. Success exits never emit this event.
    WorkerFailed {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_task_id: Option<String>,
        reason: pitboss_core::store::FailureReason,
    },
    /// A sub-lead has terminated (success, cancel, timeout, or error).
    /// Emitted by `dispatch::sublead::reconcile_terminated_sublead` after
    /// the sub-tree LayerState is removed and budget is reconciled.
    SubleadTerminated {
        sublead_id: String,
        /// USD actually spent by the sub-lead's workers.
        spent_usd: f64,
        /// USD returned to the root pool (original_reservation - spent).
        unspent_usd: f64,
        /// Terminal outcome: `"success"` | `"cancel"` | `"timeout"` | `"error"`.
        outcome: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActorActivityEntry {
    pub actor_id: String,
    pub kv_ops: u64,
    pub lease_ops: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSnapshotEntry {
    pub task_id: String,
    pub state: String,
    pub prompt_preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunFinishedSummary {
    pub tasks_total: usize,
    pub tasks_failed: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_op(op: &ControlOp) -> ControlOp {
        let s = serde_json::to_string(op).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn roundtrip_event(ev: &ControlEvent) -> ControlEvent {
        let s = serde_json::to_string(ev).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn hello_op_parses() {
        let raw = r#"{"op":"hello","client_version":"0.4.0"}"#;
        let op: ControlOp = serde_json::from_str(raw).unwrap();
        assert_eq!(
            op,
            ControlOp::Hello {
                client_version: "0.4.0".into()
            }
        );
    }

    #[test]
    fn cancel_worker_roundtrips() {
        let op = ControlOp::CancelWorker {
            task_id: "w-1".into(),
        };
        assert_eq!(roundtrip_op(&op), op);
    }

    #[test]
    fn cancel_run_roundtrips() {
        let op = ControlOp::CancelRun;
        let s = serde_json::to_string(&op).unwrap();
        assert_eq!(s, r#"{"op":"cancel_run"}"#);
        assert_eq!(roundtrip_op(&op), op);
    }

    #[test]
    fn pause_continue_reprompt_roundtrip() {
        for op in [
            ControlOp::PauseWorker {
                task_id: "w".into(),
                mode: PauseMode::default(),
            },
            ControlOp::PauseWorker {
                task_id: "w".into(),
                mode: PauseMode::Freeze,
            },
            ControlOp::ContinueWorker {
                task_id: "w".into(),
                prompt: None,
            },
            ControlOp::ContinueWorker {
                task_id: "w".into(),
                prompt: Some("go".into()),
            },
            ControlOp::RepromptWorker {
                task_id: "w".into(),
                prompt: "new plan".into(),
            },
        ] {
            assert_eq!(roundtrip_op(&op), op);
        }
    }

    #[test]
    fn approve_roundtrips_with_all_fields() {
        let op = ControlOp::Approve {
            request_id: "req-1".into(),
            approved: true,
            comment: Some("LGTM".into()),
            edited_summary: Some("spawn 2 workers".into()),
            reason: None,
        };
        assert_eq!(roundtrip_op(&op), op);
    }

    #[test]
    fn list_workers_has_no_payload() {
        let op = ControlOp::ListWorkers;
        assert_eq!(
            serde_json::to_string(&op).unwrap(),
            r#"{"op":"list_workers"}"#
        );
        assert_eq!(roundtrip_op(&op), op);
    }

    #[test]
    fn hello_event_roundtrips() {
        let ev = ControlEvent::Hello {
            server_version: "0.4.0".into(),
            run_id: "019d...".into(),
            run_kind: "hierarchical".into(),
            workers: vec!["triage".into(), "w-1".into()],
            policy_rules: vec![],
        };
        assert_eq!(roundtrip_event(&ev), ev);
    }

    #[test]
    fn hello_event_policy_rules_omitted_when_empty() {
        // Empty rules must be omitted on the wire (skip_serializing_if = Vec::is_empty).
        let ev = ControlEvent::Hello {
            server_version: "0.4.0".into(),
            run_id: "r".into(),
            run_kind: "flat".into(),
            workers: vec![],
            policy_rules: vec![],
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(
            !s.contains("policy_rules"),
            "empty rules must be elided: {s}"
        );
    }

    #[test]
    fn hello_event_policy_rules_absent_deserializes_to_empty() {
        // Pre-v0.8 dispatcher omits the field; TUI must default to empty vec.
        let raw = r#"{"event":"hello","server_version":"0.4.0","run_id":"r","run_kind":"flat","workers":[]}"#;
        let ev: ControlEvent = serde_json::from_str(raw).unwrap();
        match ev {
            ControlEvent::Hello { policy_rules, .. } => {
                assert!(policy_rules.is_empty(), "should default to empty vec");
            }
            _ => panic!("expected Hello"),
        }
    }

    #[test]
    fn update_policy_roundtrips() {
        use crate::mcp::policy::{ApprovalAction, ApprovalMatch, ApprovalRule};
        let op = ControlOp::UpdatePolicy {
            rules: vec![ApprovalRule {
                r#match: ApprovalMatch {
                    category: Some(crate::mcp::approval::ApprovalCategory::ToolUse),
                    ..Default::default()
                },
                action: ApprovalAction::AutoApprove,
            }],
        };
        let s = serde_json::to_string(&op).unwrap();
        assert!(s.contains("\"op\":\"update_policy\""), "{s}");
        let op2: ControlOp = serde_json::from_str(&s).unwrap();
        assert_eq!(op, op2);
    }

    #[test]
    fn op_acked_and_failed_roundtrip() {
        let acked = ControlEvent::OpAcked {
            op: "pause_worker".into(),
            task_id: Some("w".into()),
        };
        assert_eq!(roundtrip_event(&acked), acked);

        let failed = ControlEvent::OpFailed {
            op: "pause_worker".into(),
            task_id: Some("w".into()),
            error: "unknown task_id".into(),
        };
        assert_eq!(roundtrip_event(&failed), failed);
    }

    #[test]
    fn op_unknown_and_unknown_state_roundtrip() {
        let unk = ControlEvent::OpUnknown {
            op: "wibble".into(),
        };
        assert_eq!(roundtrip_event(&unk), unk);

        let bad_state = ControlEvent::OpUnknownState {
            op: "pause_worker".into(),
            task_id: "w".into(),
            current_state: "paused".into(),
        };
        assert_eq!(roundtrip_event(&bad_state), bad_state);
    }

    #[test]
    fn approval_request_roundtrips() {
        // Bare-summary form (pre-v0.4.5 shape).
        let ev = ControlEvent::ApprovalRequest {
            request_id: "req-1".into(),
            task_id: "lead".into(),
            summary: "spawn 3 workers".into(),
            plan: None,
            kind: ApprovalKind::default(),
        };
        assert_eq!(roundtrip_event(&ev), ev);

        // Structured form — rationale / resources / risks / rollback.
        let ev2 = ControlEvent::ApprovalRequest {
            request_id: "req-2".into(),
            task_id: "lead".into(),
            summary: "delete staging index".into(),
            plan: Some(ApprovalPlanWire {
                summary: "delete staging index".into(),
                rationale: Some("obsolete since v2".into()),
                resources: vec!["pg://staging/idx_foo".into()],
                risks: vec!["slow to rebuild if live reads hit it".into()],
                rollback: Some("restore from nightly snapshot".into()),
            }),
            kind: ApprovalKind::Action,
        };
        assert_eq!(roundtrip_event(&ev2), ev2);

        // Plan-kind form — pre-flight approval.
        let ev3 = ControlEvent::ApprovalRequest {
            request_id: "req-3".into(),
            task_id: "lead".into(),
            summary: "phase-1 migration plan".into(),
            plan: Some(ApprovalPlanWire {
                summary: "phase-1 migration plan".into(),
                rationale: Some("prep before workers fan out".into()),
                resources: vec!["3 worktrees".into()],
                risks: vec![],
                rollback: Some("no changes land yet".into()),
            }),
            kind: ApprovalKind::Plan,
        };
        assert_eq!(roundtrip_event(&ev3), ev3);
    }

    #[test]
    fn approval_kind_defaults_and_omits_on_serialize() {
        // Absence of `kind` on the wire must deserialize to Action.
        let raw = r#"{"event":"approval_request","request_id":"r","task_id":"lead","summary":"s"}"#;
        let ev: ControlEvent = serde_json::from_str(raw).unwrap();
        match ev {
            ControlEvent::ApprovalRequest { kind, .. } => {
                assert_eq!(kind, ApprovalKind::Action);
            }
            _ => panic!("expected ApprovalRequest"),
        }

        // Default Action must be omitted on serialize (backward compat).
        let ev = ControlEvent::ApprovalRequest {
            request_id: "r".into(),
            task_id: "lead".into(),
            summary: "s".into(),
            plan: None,
            kind: ApprovalKind::Action,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(!s.contains("\"kind\""), "default kind must be elided: {s}");

        // Non-default Plan must appear.
        let ev = ControlEvent::ApprovalRequest {
            request_id: "r".into(),
            task_id: "lead".into(),
            summary: "s".into(),
            plan: None,
            kind: ApprovalKind::Plan,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(
            s.contains("\"kind\":\"plan\""),
            "plan kind must be set: {s}"
        );
    }

    #[test]
    fn workers_snapshot_roundtrips() {
        let ev = ControlEvent::WorkersSnapshot {
            workers: vec![WorkerSnapshotEntry {
                task_id: "w-1".into(),
                state: "running".into(),
                prompt_preview: "investigate bug".into(),
                started_at: Some("2026-04-17T00:00:00Z".into()),
                parent_task_id: Some("lead".into()),
                session_id: Some("sess-abc".into()),
            }],
        };
        assert_eq!(roundtrip_event(&ev), ev);
    }

    #[test]
    fn superseded_and_run_finished_roundtrip() {
        let sup = ControlEvent::Superseded;
        assert_eq!(
            serde_json::to_string(&sup).unwrap(),
            r#"{"event":"superseded"}"#
        );
        assert_eq!(roundtrip_event(&sup), sup);

        let rf = ControlEvent::RunFinished {
            summary: RunFinishedSummary {
                tasks_total: 5,
                tasks_failed: 1,
            },
        };
        assert_eq!(roundtrip_event(&rf), rf);
    }

    #[test]
    fn worker_failed_roundtrips() {
        use pitboss_core::store::FailureReason;
        let ev = ControlEvent::WorkerFailed {
            task_id: "w-1".into(),
            parent_task_id: Some("lead".into()),
            reason: FailureReason::RateLimit { resets_at: None },
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"event\":\"worker_failed\""));
        assert!(s.contains("\"kind\":\"rate_limit\""));
        assert_eq!(roundtrip_event(&ev), ev);
    }

    #[test]
    fn worker_failed_without_parent_elides_field() {
        // Root-lead failures have no parent; ensure the field is omitted
        // on the wire rather than serialized as `null`.
        use pitboss_core::store::FailureReason;
        let ev = ControlEvent::WorkerFailed {
            task_id: "lead".into(),
            parent_task_id: None,
            reason: FailureReason::AuthFailure,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(!s.contains("parent_task_id"));
    }

    #[test]
    fn store_activity_roundtrips() {
        let ev = ControlEvent::StoreActivity {
            counters: vec![
                ActorActivityEntry {
                    actor_id: "worker-A".into(),
                    kv_ops: 42,
                    lease_ops: 3,
                },
                ActorActivityEntry {
                    actor_id: "lead".into(),
                    kv_ops: 7,
                    lease_ops: 0,
                },
            ],
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"event\":\"store_activity\""));
        assert!(s.contains("\"worker-A\""));
        assert_eq!(roundtrip_event(&ev), ev);
    }
}
