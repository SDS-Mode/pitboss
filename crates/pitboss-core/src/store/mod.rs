//! Persistence — trait and file-backed implementation.

pub mod record;
pub mod traits;

pub use record::{RunMeta, RunSummary, TaskRecord, TaskStatus};
pub use traits::SessionStore;

pub mod json_file;
pub use json_file::JsonFileStore;

pub mod sqlite;
pub use sqlite::SqliteStore;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn meta(run_id: Uuid, root: &Path) -> RunMeta {
        RunMeta {
            run_id,
            manifest_path: root.join("pitboss.toml"),
            pitboss_version: "0.1.0".into(),
            claude_version: Some("1.0.0".into()),
            started_at: Utc::now(),
            env: HashMap::new(),
        }
    }

    fn rec(task_id: &str, status: TaskStatus) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            task_id: task_id.into(),
            status,
            exit_code: Some(0),
            started_at: now,
            ended_at: now,
            duration_ms: 0,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: crate::parser::TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
        }
    }

    #[tokio::test]
    async fn init_and_append_and_finalize_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        store
            .append_record(run_id, &rec("a", TaskStatus::Success))
            .await
            .unwrap();
        store
            .append_record(run_id, &rec("b", TaskStatus::Failed))
            .await
            .unwrap();

        let summary = RunSummary {
            run_id,
            manifest_path: dir.path().join("pitboss.toml"),
            pitboss_version: "0.1.0".into(),
            claude_version: None,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            total_duration_ms: 0,
            tasks_total: 2,
            tasks_failed: 1,
            was_interrupted: false,
            tasks: vec![rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
        };
        store.finalize_run(&summary).await.unwrap();

        let back = store.load_run(run_id).await.unwrap();
        assert_eq!(back.tasks.len(), 2);
        assert_eq!(back.tasks_failed, 1);
    }

    #[tokio::test]
    async fn load_orphan_run_marks_interrupted() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        store
            .append_record(run_id, &rec("only", TaskStatus::Success))
            .await
            .unwrap();

        let loaded = store.load_run(run_id).await.unwrap();
        assert!(loaded.was_interrupted);
        assert_eq!(loaded.tasks.len(), 1);
    }
}
