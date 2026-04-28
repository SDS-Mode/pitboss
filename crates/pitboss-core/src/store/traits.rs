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

    /// Enumerate runs as `RunMeta` records, newest-first by
    /// `started_at`. Unlike calling [`load_run`] per run, this never
    /// materialises a per-run task list — the caller pays for the
    /// metadata only. Suitable for run-list dashboards, prune-sweep
    /// scans, and similar "give me every run id" workflows that
    /// previously had to walk the filesystem out-of-band. (#149 L8)
    ///
    /// Skipped entries (unreadable subdir, malformed `meta.json`,
    /// row missing required column) are silently dropped rather
    /// than aborting the whole iteration — operational consoles
    /// expect partial inventories during a live run.
    ///
    /// [`load_run`]: SessionStore::load_run
    async fn iter_runs(&self) -> Result<Vec<RunMeta>, StoreError>;
}
