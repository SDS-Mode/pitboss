use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::parser::TokenUsage;

/// Structured classification of *why* a claude subprocess failed, derived by
/// scanning its stdout/stderr after a non-zero exit. Populated on the
/// `TaskRecord` so callers (TUI, parent lead, spawn gater) can react without
/// re-parsing logs. Exit code 0 never produces a `FailureReason` — a successful
/// response that happens to mention "rate limit" in body text is not a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureReason {
    /// API rate/usage limit hit. `resets_at` is the parsed reset time when the
    /// CLI emitted one (e.g., "resets Apr 23, 3pm"); `None` when the marker was
    /// detected but no timestamp was parseable.
    RateLimit { resets_at: Option<DateTime<Utc>> },
    /// DNS/connection errors: ENOTFOUND, ETIMEDOUT, ECONNRESET, etc.
    NetworkError { message: String },
    /// 401 / `invalid_api_key` from the API.
    AuthFailure,
    /// Model refused the prompt due to context-length exceeded.
    ContextExceeded,
    /// 400 `invalid_request_error` surfaced by the CLI.
    InvalidArgument { message: String },
    /// Non-zero exit with no recognized marker. `message` is a short excerpt
    /// from the tail of stderr/stdout for triage.
    Unknown { message: String },
}

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
    /// First ~200 chars of the assistant's chosen final message, with an
    /// ellipsis if truncated. Suitable for table cells and chat embeds where
    /// a long blob would overflow the layout. Consumers that need the
    /// complete text should read `final_message`.
    pub final_message_preview: Option<String>,
    /// Untruncated assistant final message. Same source as
    /// `final_message_preview` (longest non-trivial assistant turn) without
    /// the 200-char cap. Added in v0.10 — `#[serde(default)]` makes pre-v0.10
    /// `summary.json` files still parse with this field as `None`.
    #[serde(default)]
    pub final_message: Option<String>,
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
    /// Structured failure classification for non-zero exits. `None` for
    /// successful tasks, cancelled tasks, and pre-v0.7.1 records.
    #[serde(default)]
    pub failure_reason: Option<FailureReason>,
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
    /// Human-readable label from `[run].name`. Lets the operational console
    /// group related runs without re-reading `manifest.snapshot.toml` per
    /// digest. `None` for pre-name-field runs and for manifests that omit
    /// `[run].name`. Added in v0.10.
    #[serde(default)]
    pub manifest_name: Option<String>,
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
            final_message: Some("ok".into()),
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
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
            final_message: None,
            parent_task_id: Some("lead-abc".into()),
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
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
            final_message: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: Some("claude-opus-4-7".into()),
            failure_reason: None,
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
    fn task_record_missing_final_message_deserializes_as_none() {
        // Pre-v0.10 records didn't have a `final_message` field. The
        // back-compat test guards against breaking existing summary.json
        // files when the dispatcher is upgraded.
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
            "final_message_preview": "snippet…"
        }"#;
        let rec: TaskRecord = serde_json::from_str(old_json).unwrap();
        assert_eq!(rec.final_message_preview.as_deref(), Some("snippet…"));
        assert!(rec.final_message.is_none());
    }

    #[test]
    fn task_record_round_trips_full_final_message() {
        let full = "a".repeat(500);
        let rec = TaskRecord {
            task_id: "t".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 26, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 4, 26, 0, 0, 1).unwrap(),
            duration_ms: 1_000,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: Some(format!("{}…", &full[..200])),
            final_message: Some(full.clone()),
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: TaskRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back.final_message.as_deref(), Some(full.as_str()));
        assert_eq!(
            back.final_message_preview.as_ref().unwrap().chars().count(),
            201
        );
    }

    #[test]
    fn run_summary_round_trips_manifest_name() {
        let summary = RunSummary {
            run_id: uuid::Uuid::now_v7(),
            manifest_path: PathBuf::from("/x.toml"),
            manifest_name: Some("nightly".into()),
            pitboss_version: "0.10".into(),
            claude_version: None,
            started_at: Utc.with_ymd_and_hms(2026, 4, 27, 0, 0, 0).unwrap(),
            ended_at: Utc.with_ymd_and_hms(2026, 4, 27, 0, 1, 0).unwrap(),
            total_duration_ms: 60_000,
            tasks_total: 0,
            tasks_failed: 0,
            was_interrupted: false,
            tasks: vec![],
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("nightly"));
        let back: RunSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.manifest_name.as_deref(), Some("nightly"));
    }

    #[test]
    fn pre_v0_10_run_summary_back_compat() {
        // Old summary.json without manifest_name must still parse.
        let old = r#"{
            "run_id": "01950abc-1234-7def-8000-000000000000",
            "manifest_path": "/x.toml",
            "pitboss_version": "0.9.0",
            "claude_version": null,
            "started_at": "2026-04-26T00:00:00Z",
            "ended_at":   "2026-04-26T00:01:00Z",
            "total_duration_ms": 60000,
            "tasks_total": 0,
            "tasks_failed": 0,
            "was_interrupted": false,
            "tasks": []
        }"#;
        let s: RunSummary = serde_json::from_str(old).unwrap();
        assert!(s.manifest_name.is_none());
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
            final_message: None,
            parent_task_id: None,
            pause_count: 2,
            reprompt_count: 1,
            approvals_requested: 3,
            approvals_approved: 2,
            approvals_rejected: 1,
            model: None,
            failure_reason: None,
        };
        let s = serde_json::to_string(&rec).unwrap();
        let back: TaskRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back.pause_count, 2);
        assert_eq!(back.approvals_rejected, 1);
    }
}
