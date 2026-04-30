use std::path::{Path, PathBuf};

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
    fn open(path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        // JsonFileStore::new is infallible; treat any IO during root
        // creation as a soft setup step, not a precondition.
        Ok(Box::new(JsonFileStore::new(path.to_path_buf())))
    }

    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError> {
        let dir = self.run_dir(meta.run_id);
        fs::create_dir_all(&dir).await?;
        let bytes = serde_json::to_vec_pretty(meta)?;
        crate::atomic_write::write_atomic_async(&self.meta_json(meta.run_id), &bytes).await?;
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
        crate::atomic_write::write_atomic_async(&path, &bytes).await?;
        Ok(())
    }

    async fn iter_runs(&self) -> Result<Vec<RunMeta>, StoreError> {
        // Missing root is a valid empty inventory — the dispatcher
        // creates it lazily on first `init_run`, so callers that hit
        // `iter_runs` before a single run was registered should see
        // [] rather than an error. (#149 L8)
        let mut rd = match fs::read_dir(&self.root).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::Io(e)),
        };
        let mut metas: Vec<RunMeta> = Vec::new();
        while let Some(entry) = rd.next_entry().await? {
            let Ok(ft) = entry.file_type().await else {
                continue;
            };
            if !ft.is_dir() {
                continue;
            }
            let meta_path = entry.path().join("meta.json");
            let Ok(bytes) = fs::read(&meta_path).await else {
                continue; // run dir without meta.json — skip silently
            };
            if let Ok(meta) = serde_json::from_slice::<RunMeta>(&bytes) {
                metas.push(meta);
            }
        }
        metas.sort_by_key(|m| std::cmp::Reverse(m.started_at));
        Ok(metas)
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
        let mut cost_by_provider = std::collections::HashMap::new();
        for task in &tasks {
            if let Some(cost) = task.cost_usd {
                *cost_by_provider
                    .entry(task.provider.as_key())
                    .or_insert(0.0) += cost;
            }
        }
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
            cost_by_provider,
        })
    }
}

#[cfg(test)]
mod iter_runs_tests {
    use super::*;
    use chrono::Duration;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn meta_with_started(run_id: Uuid, started: chrono::DateTime<chrono::Utc>) -> RunMeta {
        RunMeta {
            run_id,
            manifest_path: PathBuf::from("/x.toml"),
            pitboss_version: "0.9.1".into(),
            claude_version: None,
            started_at: started,
            env: HashMap::new(),
        }
    }

    /// #149 L8: empty runs root yields an empty inventory rather than
    /// erroring. The dispatcher creates the root lazily on first
    /// `init_run`, so an operational console asking "what runs do
    /// you have?" before any have started should see [].
    #[tokio::test]
    async fn iter_runs_returns_empty_for_missing_root() {
        let tmp = TempDir::new().unwrap();
        // sub-directory under tmp does not exist yet
        let store = JsonFileStore::new(tmp.path().join("runs"));
        let metas = store.iter_runs().await.unwrap();
        assert!(metas.is_empty());
    }

    #[tokio::test]
    async fn iter_runs_returns_empty_for_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let store = JsonFileStore::new(tmp.path().to_path_buf());
        let metas = store.iter_runs().await.unwrap();
        assert!(metas.is_empty());
    }

    /// Newest-first ordering by `started_at`. Three runs initialised
    /// with explicitly-skewed timestamps so the ordering check is
    /// independent of filesystem mtime.
    #[tokio::test]
    async fn iter_runs_orders_newest_first() {
        let tmp = TempDir::new().unwrap();
        let store = JsonFileStore::new(tmp.path().to_path_buf());
        let now = chrono::Utc::now();

        let mid_id = Uuid::now_v7();
        let oldest_id = Uuid::now_v7();
        let newest_id = Uuid::now_v7();

        store
            .init_run(&meta_with_started(mid_id, now - Duration::minutes(30)))
            .await
            .unwrap();
        store
            .init_run(&meta_with_started(oldest_id, now - Duration::hours(2)))
            .await
            .unwrap();
        store
            .init_run(&meta_with_started(newest_id, now))
            .await
            .unwrap();

        let metas = store.iter_runs().await.unwrap();
        let ids: Vec<Uuid> = metas.iter().map(|m| m.run_id).collect();
        assert_eq!(
            ids,
            vec![newest_id, mid_id, oldest_id],
            "iter_runs sorts newest-first by started_at"
        );
    }

    /// A subdir without `meta.json` (e.g. half-created run, or
    /// unrelated directory dropped under the runs root) is silently
    /// skipped — the rest of the inventory still lands.
    #[tokio::test]
    async fn iter_runs_skips_dirs_without_meta_json() {
        let tmp = TempDir::new().unwrap();
        let store = JsonFileStore::new(tmp.path().to_path_buf());
        let real_id = Uuid::now_v7();
        store
            .init_run(&meta_with_started(real_id, chrono::Utc::now()))
            .await
            .unwrap();
        // A directory under the root that has no meta.json.
        std::fs::create_dir_all(tmp.path().join("not-a-run")).unwrap();

        let metas = store.iter_runs().await.unwrap();
        let ids: Vec<Uuid> = metas.iter().map(|m| m.run_id).collect();
        assert_eq!(ids, vec![real_id]);
    }
}
