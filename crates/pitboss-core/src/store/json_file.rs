use std::path::PathBuf;

use async_trait::async_trait;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uuid::Uuid;

use crate::error::StoreError;

use super::record::{RunMeta, RunSummary, TaskRecord};
use super::traits::SessionStore;

pub struct JsonFileStore {
    root: PathBuf,
}

impl JsonFileStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn run_dir(&self, run_id: Uuid) -> PathBuf {
        self.root.join(run_id.to_string())
    }
    fn summary_jsonl(&self, run_id: Uuid) -> PathBuf {
        self.run_dir(run_id).join("summary.jsonl")
    }
    fn summary_json(&self, run_id: Uuid) -> PathBuf {
        self.run_dir(run_id).join("summary.json")
    }
    fn meta_json(&self, run_id: Uuid) -> PathBuf {
        self.run_dir(run_id).join("meta.json")
    }
}

#[async_trait]
impl SessionStore for JsonFileStore {
    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError> {
        let dir = self.run_dir(meta.run_id);
        fs::create_dir_all(&dir).await?;
        let bytes = serde_json::to_vec_pretty(meta)?;
        fs::write(self.meta_json(meta.run_id), bytes).await?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.summary_jsonl(meta.run_id))
            .await?;
        Ok(())
    }

    async fn append_record(&self, run_id: Uuid, record: &TaskRecord) -> Result<(), StoreError> {
        let line = serde_json::to_string(record)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.summary_jsonl(run_id))
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.sync_all().await?;
        Ok(())
    }

    async fn finalize_run(&self, summary: &RunSummary) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(summary)?;
        let path = self.summary_json(summary.run_id);
        fs::write(path, bytes).await?;
        Ok(())
    }

    async fn load_run(&self, run_id: Uuid) -> Result<RunSummary, StoreError> {
        let fin = self.summary_json(run_id);
        if fs::try_exists(&fin).await.unwrap_or(false) {
            let bytes = fs::read(&fin).await?;
            return Ok(serde_json::from_slice(&bytes)?);
        }
        let meta_bytes = fs::read(self.meta_json(run_id)).await?;
        let meta: RunMeta = serde_json::from_slice(&meta_bytes)?;
        let jsonl = self.summary_jsonl(run_id);
        let file = tokio::fs::File::open(&jsonl).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut tasks = Vec::new();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let r: TaskRecord = serde_json::from_str(&line)?;
            tasks.push(r);
        }
        let tasks_failed = tasks
            .iter()
            .filter(|t| !matches!(t.status, super::TaskStatus::Success))
            .count();
        let started = meta.started_at;
        let ended = tasks.last().map_or(started, |t| t.ended_at);
        Ok(RunSummary {
            run_id: meta.run_id,
            manifest_path: meta.manifest_path,
            manifest_name: None,
            pitboss_version: meta.pitboss_version,
            claude_version: meta.claude_version,
            started_at: started,
            ended_at: ended,
            total_duration_ms: (ended - started).num_milliseconds(),
            tasks_total: tasks.len(),
            tasks_failed,
            was_interrupted: true,
            tasks,
        })
    }
}
