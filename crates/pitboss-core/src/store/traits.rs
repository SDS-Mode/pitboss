use std::path::Path;

use async_trait::async_trait;
use uuid::Uuid;

use super::record::{RunMeta, RunSummary, TaskRecord};
use crate::error::StoreError;

#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Open (or create) a store rooted at `path`. Required so callers can
    /// uniformly construct either backend through the trait — pre-fix,
    /// `SqliteStore::new` returned `Result<Self, StoreError>` and
    /// `JsonFileStore::new` returned `Self` infallibly, so a generic
    /// constructor was impossible. (#188 M3)
    ///
    /// Trait methods cannot return `Self` and still be object-safe, so
    /// the return is `Box<dyn SessionStore>` — code that wants to hand a
    /// store to something parameterized over `Arc<dyn SessionStore>` can
    /// `Arc::from(store)` the boxed value directly.
    fn open(path: &Path) -> Result<Box<dyn SessionStore>, StoreError>
    where
        Self: Sized;

    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError>;
    async fn append_record(&self, run_id: Uuid, record: &TaskRecord) -> Result<(), StoreError>;
    async fn finalize_run(&self, summary: &RunSummary) -> Result<(), StoreError>;
    async fn load_run(&self, run_id: Uuid) -> Result<RunSummary, StoreError>;
}
