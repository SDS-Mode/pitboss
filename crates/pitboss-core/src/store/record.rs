use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::parser::TokenUsage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Success,
    Failed,
    TimedOut,
    Cancelled,
    SpawnFailed,
    /// Terminal state for a task that called `request_approval` or
    /// `propose_plan` and received `{approved: false}` from an operator
    /// (or a policy rule that mapped to `auto_reject`), then exited
    /// without doing meaningful subsequent work. Previously lumped into
    /// `Success` because claude exited 0; distinguished as of v0.7+.
    ApprovalRejected,
    /// Terminal state for a task whose pending approval aged past its
    /// `ttl_secs` and was auto-resolved via its `fallback` (typically
    /// `auto_reject`), then exited. Distinguished from `ApprovalRejected`
    /// because no operator attention reached the approval — it timed out.
    ApprovalTimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_ms: i64,
    pub worktree_path: Option<PathBuf>,
    pub log_path: PathBuf,
    pub token_usage: TokenUsage,
    pub claude_session_id: Option<String>,
    pub final_message_preview: Option<String>,
    /// Task id of the lead that spawned this worker, or `None` for flat-mode
    /// tasks and the lead itself.
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub pause_count: u32,
    #[serde(default)]
    pub reprompt_count: u32,
    #[serde(default)]
    pub approvals_requested: u32,
    #[serde(default)]
    pub approvals_approved: u32,
    #[serde(default)]
    pub approvals_rejected: u32,
    /// Resolved model string (e.g. `"claude-opus-4-7"`). Populated at
    /// spawn time for both lead and workers; `None` for pre-v0.4.2
    /// records or when the model can't be resolved (spawn-failed
    /// tasks with no model yet chosen). Consumers should fall back to
    /// scanning the log if they need this for an older run.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    pub run_id: Uuid,
    pub manifest_path: PathBuf,
    pub pitboss_version: String,
    pub claude_version: Option<String>,
    pub started_at: DateTime<Utc>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: Uuid,
    pub manifest_path: PathBuf,
    pub pitboss_version: String,
    pub claude_version: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub total_duration_ms: i64,
    pub tasks_total: usize,
    pub tasks_failed: usize,
    pub was_interrupted: bool,
    pub tasks: Vec<TaskRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn task_record_round_trips_json() {
        let rec = TaskRecord {
            task_id: "t1".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 4, 16, 0, 0, 30).unwrap(),
            duration_ms: 30_000,
            worktree_path: Some(PathBuf::from("/tmp/wt")),
            log_path: PathBuf::from("/tmp/log"),
            token_usage: TokenUsage {
                input: 1,
                output: 2,
                cache_read: 3,
                cache_creation: 4,
            },
            claude_session_id: Some("sess".into()),
            final_message_preview: Some("ok".into()),
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        let back: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task_id, "t1");
        assert!(matches!(back.status, TaskStatus::Success));
    }

    #[test]
    fn task_record_with_parent_round_trips() {
        let rec = TaskRecord {
            task_id: "worker-1".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 17, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 4, 17, 0, 0, 30).unwrap(),
            duration_ms: 30_000,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: Some("lead-abc".into()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("parent_task_id"));
        let back: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.parent_task_id.as_deref(), Some("lead-abc"));
    }

    #[test]
    fn task_status_approval_variants_round_trip() {
        for (variant, expected_json) in &[
            (TaskStatus::ApprovalRejected, "\"ApprovalRejected\""),
            (TaskStatus::ApprovalTimedOut, "\"ApprovalTimedOut\""),
        ] {
            let s = serde_json::to_string(variant).unwrap();
            assert_eq!(&s, expected_json, "serialize {variant:?}");
            let back: TaskStatus = serde_json::from_str(&s).unwrap();
            assert_eq!(&back, variant, "round-trip {variant:?}");
        }
    }

    #[test]
    fn task_record_without_parent_deserializes_from_old_json() {
        let old_json = r#"{
            "task_id": "t1",
            "status": "Success",
            "exit_code": 0,
            "started_at": "2026-04-17T00:00:00Z",
            "ended_at":   "2026-04-17T00:00:30Z",
            "duration_ms": 30000,
            "worktree_path": null,
            "log_path": "/dev/null",
            "token_usage": {"input":0,"output":0,"cache_read":0,"cache_creation":0},
            "claude_session_id": null,
            "final_message_preview": null
        }"#;
        let rec: TaskRecord = serde_json::from_str(old_json).unwrap();
        assert!(rec.parent_task_id.is_none());
    }

    #[test]
    fn task_record_round_trips_model_field() {
        let rec = TaskRecord {
            task_id: "w1".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 4, 18, 0, 0, 30).unwrap(),
            duration_ms: 30_000,
            worktree_path: None,
            log_path: PathBuf::from("/tmp/log"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: Some("claude-opus-4-7".into()),
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("claude-opus-4-7"));
        let back: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn task_record_missing_model_deserializes_as_none() {
        // Pre-v0.4.2 records didn't have a `model` field; must still parse.
        let old_json = r#"{
            "task_id": "t1",
            "status": "Success",
            "exit_code": 0,
            "started_at": "2026-04-17T00:00:00Z",
            "ended_at":   "2026-04-17T00:00:30Z",
            "duration_ms": 30000,
            "worktree_path": null,
            "log_path": "/dev/null",
            "token_usage": {"input":0,"output":0,"cache_read":0,"cache_creation":0},
            "claude_session_id": null,
            "final_message_preview": null
        }"#;
        let rec: TaskRecord = serde_json::from_str(old_json).unwrap();
        assert!(rec.model.is_none());
    }

    #[test]
    fn task_record_new_counter_fields_default_to_zero() {
        let old_json = r#"{
            "task_id": "t1",
            "status": "Success",
            "exit_code": 0,
            "started_at": "2026-04-17T00:00:00Z",
            "ended_at":   "2026-04-17T00:00:30Z",
            "duration_ms": 30000,
            "worktree_path": null,
            "log_path": "/dev/null",
            "token_usage": {"input":0,"output":0,"cache_read":0,"cache_creation":0},
            "claude_session_id": null,
            "final_message_preview": null
        }"#;
        let rec: TaskRecord = serde_json::from_str(old_json).unwrap();
        assert_eq!(rec.pause_count, 0);
        assert_eq!(rec.reprompt_count, 0);
        assert_eq!(rec.approvals_requested, 0);
        assert_eq!(rec.approvals_approved, 0);
        assert_eq!(rec.approvals_rejected, 0);
    }

    #[test]
    fn task_record_roundtrips_counters() {
        let rec = TaskRecord {
            task_id: "w".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 17, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 4, 17, 0, 0, 30).unwrap(),
            duration_ms: 30_000,
            worktree_path: None,
            log_path: PathBuf::from("/tmp/log"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 2,
            reprompt_count: 1,
            approvals_requested: 3,
            approvals_approved: 2,
            approvals_rejected: 1,
            model: None,
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: TaskRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back.pause_count, 2);
        assert_eq!(back.approvals_rejected, 1);
    }
}
