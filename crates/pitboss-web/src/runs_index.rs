//! Run discovery + JSON DTO mapping. Wraps `pitboss_cli::runs` so we
//! reuse the canonical `RunStatus` classifier and avoid a second source
//! of truth for what counts as `Running` vs `Stale`.

use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;

use pitboss_cli::runs::{collect_run_entries, RunEntry, RunStatus};

/// JSON shape for `GET /api/runs`. Compact summary — clients fetch
/// `GET /api/runs/:id` for the full `summary.json`.
#[derive(Debug, Serialize)]
pub struct RunDto {
    pub run_id: String,
    pub status: RunStatus,
    pub status_label: &'static str,
    pub mtime_unix: u64,
    pub tasks_total: usize,
    pub tasks_failed: usize,
}

impl From<&RunEntry> for RunDto {
    fn from(e: &RunEntry) -> Self {
        Self {
            run_id: e.run_id.clone(),
            status: e.status,
            status_label: e.status.label(),
            mtime_unix: e
                .mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            tasks_total: e.tasks_total,
            tasks_failed: e.tasks_failed,
        }
    }
}

pub fn list_runs(base: &Path) -> Vec<RunDto> {
    collect_run_entries(base).iter().map(RunDto::from).collect()
}
