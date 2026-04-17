//! Wire protocol for the per-run control socket.
//!
//! Messages are **one JSON object per line** (UTF-8, LF-terminated).
//! Client → server messages are `ControlOp`; server → client are `ControlEvent`.
//! Serialization uses `#[serde(tag = "op")]` / `#[serde(tag = "event")]` so the
//! discriminator lives in the same object as the payload fields.

#![allow(dead_code)] // Wired up by control::server in Task 4+.

use serde::{Deserialize, Serialize};

/// An operation sent from the TUI (client) to the dispatcher (server).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    },
    ListWorkers,
}

/// An event pushed from the dispatcher (server) to the TUI (client).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ControlEvent {
    Hello {
        server_version: String,
        run_id: String,
        run_kind: String,
        workers: Vec<String>,
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
    },
    WorkersSnapshot {
        workers: Vec<WorkerSnapshotEntry>,
    },
    Superseded,
    RunFinished {
        summary: RunFinishedSummary,
    },
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
        };
        assert_eq!(roundtrip_event(&ev), ev);
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
        let ev = ControlEvent::ApprovalRequest {
            request_id: "req-1".into(),
            task_id: "lead".into(),
            summary: "spawn 3 workers".into(),
        };
        assert_eq!(roundtrip_event(&ev), ev);
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
}
