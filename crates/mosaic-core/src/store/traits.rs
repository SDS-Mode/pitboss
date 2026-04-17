use async_trait::async_trait;
use uuid::Uuid;

use crate::error::StoreError;
use super::record::{RunMeta, RunSummary, TaskRecord};

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError>;
    async fn append_record(&self, run_id: Uuid, record: &TaskRecord) -> Result<(), StoreError>;
    async fn finalize_run(&self, summary: &RunSummary) -> Result<(), StoreError>;
    async fn load_run(&self, run_id: Uuid) -> Result<RunSummary, StoreError>;
}
