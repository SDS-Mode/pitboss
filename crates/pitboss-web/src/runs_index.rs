//! Run discovery + JSON DTO mapping. Wraps `pitboss_core::runs` so we
//! reuse the canonical `RunStatus` classifier and avoid a second source
//! of truth for what counts as `Running` vs `Stale`. Lifted out of
//! `pitboss-cli` in #484.

use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;

use pitboss_core::runs::{collect_run_entries, RunEntry, RunStatus};

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
    /// Peak memory utilization for the run (#553). Fractional value
    /// in [0, 1+]. `None` when the resource watcher was disabled,
    /// unsupported on the host, or for pre-#553 runs — the SPA
    /// renders `—` in that case. `skip_serializing_if = "Option::is_none"`
    /// keeps the JSON shape byte-identical for pre-#553 clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_utilization_pct: Option<f32>,
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
            peak_utilization_pct: e.peak_utilization_pct,
        }
    }
}

pub fn list_runs(base: &Path) -> Vec<RunDto> {
    collect_run_entries(base).iter().map(RunDto::from).collect()
}
