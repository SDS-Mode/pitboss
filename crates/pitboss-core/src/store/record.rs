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
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("parent_task_id"));
        let back: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.parent_task_id.as_deref(), Some("lead-abc"));
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
}
