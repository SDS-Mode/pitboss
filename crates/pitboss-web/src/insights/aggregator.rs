//! Cross-run aggregator. Walks the runs directory, reads `summary.json`
//! (or falls back to `summary.jsonl` for in-progress runs), classifies
//! status via [`pitboss_cli::runs::collect_run_entries`], and builds
//! flat [`RunDigest`] / [`TaskFailureDigest`] lists. The aggregator is
//! the single source of truth that every `/api/insights/*` endpoint
//! consumes.

use std::path::{Path, PathBuf};

use pitboss_cli::runs::{collect_run_entries, RunStatus};
use pitboss_core::store::{FailureReason, RunSummary, TaskStatus};
use serde::Deserialize;

use super::digest::{RunDigest, TaskFailureDigest};
use super::tokenizer::canonicalize;

/// Filter knobs honored by [`AggregateSet::apply_filter`]. All None →
/// no filtering. `since` / `until` are inclusive Unix-second bounds on
/// `started_at`. `manifest` and `kind` match exactly (case-sensitive).
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub manifest: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub status: Option<String>,
    pub kind: Option<String>,
}

/// Digested view of every run discovered under `runs_dir`. Built once
/// per cache miss; subsequent endpoint requests filter into projections
/// of this set.
#[derive(Debug, Clone)]
pub struct AggregateSet {
    pub runs: Vec<RunDigest>,
    pub failures: Vec<TaskFailureDigest>,
}

impl AggregateSet {
    /// Walk `runs_dir`, build the full unfiltered set. Cost: ~one
    /// `read_dir` + per-run `summary.json` read. Quiet failure on any
    /// individual run dir — a malformed `summary.json` shouldn't
    /// poison the entire dashboard.
    pub fn build(runs_dir: &Path) -> Self {
        let entries = collect_run_entries(runs_dir);
        let mut runs = Vec::with_capacity(entries.len());
        let mut failures = Vec::new();

        for entry in entries {
            let summary_path = entry.run_dir.join("summary.json");
            let summary = match std::fs::read(&summary_path) {
                Ok(bytes) => serde_json::from_slice::<RunSummary>(&bytes).ok(),
                Err(_) => None,
            };

            // For in-progress / aborted runs without a summary.json,
            // attempt to read the (newer) name field from resolved.json.
            let manifest_name = summary
                .as_ref()
                .and_then(|s| s.manifest_name.clone())
                .or_else(|| read_resolved_name(&entry.run_dir));

            let manifest_path = summary
                .as_ref()
                .map(|s| s.manifest_path.display().to_string())
                .or_else(|| read_meta_manifest_path(&entry.run_dir));

            let resolved_name =
                resolve_manifest_name(manifest_name.as_deref(), manifest_path.as_deref());

            let started_at = summary
                .as_ref()
                .map(|s| s.started_at.timestamp())
                .or_else(|| {
                    Some(
                        entry
                            .mtime
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()?
                            .as_secs() as i64,
                    )
                });
            let ended_at = summary.as_ref().map(|s| s.ended_at.timestamp());
            let duration_ms = summary.as_ref().map(|s| s.total_duration_ms);

            let mut failure_kinds: Vec<String> = Vec::new();
            if let Some(s) = &summary {
                for record in &s.tasks {
                    if let Some(reason) = &record.failure_reason {
                        let kind = failure_kind_str(reason);
                        if !failure_kinds.contains(&kind) {
                            failure_kinds.push(kind.clone());
                        }
                        let (msg, template) = failure_message_and_template(reason);
                        failures.push(TaskFailureDigest {
                            run_id: entry.run_id.clone(),
                            manifest_name: resolved_name.clone(),
                            task_id: record.task_id.clone(),
                            parent_task_id: record.parent_task_id.clone(),
                            failure_kind: kind,
                            error_message: msg,
                            error_template: template,
                            model: record.model.clone(),
                            duration_ms: Some(record.duration_ms),
                            occurred_at: Some(record.ended_at.timestamp()),
                        });
                    } else if !is_success(&record.status) {
                        // Non-zero exit with no FailureReason populated
                        // (older runs, or terminal states like
                        // ApprovalRejected). Surface them under a
                        // synthetic kind so they still appear in the
                        // failures dashboard.
                        let kind = synthetic_kind(&record.status);
                        if !failure_kinds.contains(&kind) {
                            failure_kinds.push(kind.clone());
                        }
                        failures.push(TaskFailureDigest {
                            run_id: entry.run_id.clone(),
                            manifest_name: resolved_name.clone(),
                            task_id: record.task_id.clone(),
                            parent_task_id: record.parent_task_id.clone(),
                            failure_kind: kind,
                            error_message: None,
                            error_template: None,
                            model: record.model.clone(),
                            duration_ms: Some(record.duration_ms),
                            occurred_at: Some(record.ended_at.timestamp()),
                        });
                    }
                }
            }

            let outcome = compute_outcome(&entry.status, entry.tasks_failed, entry.tasks_total);
            runs.push(RunDigest {
                run_id: entry.run_id.clone(),
                manifest_name: resolved_name,
                manifest_path,
                status: entry.status.label().to_string(),
                outcome,
                started_at,
                ended_at,
                duration_ms,
                tasks_total: entry.tasks_total,
                tasks_failed: entry.tasks_failed,
                failure_kinds,
            });
        }

        Self { runs, failures }
    }

    /// Apply a filter and return a NEW set containing only matching
    /// runs + failures. Cheap because both vectors are flat.
    pub fn apply_filter(&self, filter: &Filter) -> Self {
        let runs: Vec<RunDigest> = self
            .runs
            .iter()
            .filter(|r| match_run(r, filter))
            .cloned()
            .collect();
        let kept_run_ids: std::collections::HashSet<&str> =
            runs.iter().map(|r| r.run_id.as_str()).collect();
        let failures: Vec<TaskFailureDigest> = self
            .failures
            .iter()
            .filter(|f| {
                kept_run_ids.contains(f.run_id.as_str())
                    && filter
                        .kind
                        .as_deref()
                        .map(|k| f.failure_kind == k)
                        .unwrap_or(true)
            })
            .cloned()
            .collect();
        Self { runs, failures }
    }
}

fn match_run(r: &RunDigest, f: &Filter) -> bool {
    if let Some(m) = &f.manifest {
        if &r.manifest_name != m {
            return false;
        }
    }
    if let Some(s) = &f.status {
        if &r.status != s {
            return false;
        }
    }
    if let Some(since) = f.since {
        match r.started_at {
            Some(t) if t >= since => {}
            _ => return false,
        }
    }
    if let Some(until) = f.until {
        match r.started_at {
            Some(t) if t <= until => {}
            _ => return false,
        }
    }
    if let Some(k) = &f.kind {
        if !r.failure_kinds.iter().any(|kk| kk == k) {
            return false;
        }
    }
    true
}

/// Resolve the human-readable manifest name from the identity cascade:
///
/// 1. Explicit `[run].name` (carried in `summary.json::manifest_name`).
/// 2. basename of the manifest path, minus `.toml`.
/// 3. `"<unnamed>"`.
pub fn resolve_manifest_name(name: Option<&str>, path: Option<&str>) -> String {
    if let Some(n) = name.map(str::trim).filter(|s| !s.is_empty()) {
        return n.to_string();
    }
    if let Some(p) = path {
        let pb = PathBuf::from(p);
        if let Some(stem) = pb.file_stem().and_then(|s| s.to_str()) {
            if !stem.is_empty() {
                return stem.to_string();
            }
        }
    }
    "<unnamed>".into()
}

fn compute_outcome(status: &RunStatus, failed: usize, total: usize) -> String {
    match status {
        RunStatus::Running => "running".into(),
        RunStatus::Stale => "stale".into(),
        RunStatus::Aborted => "aborted".into(),
        RunStatus::Complete => {
            if failed == 0 {
                "success".into()
            } else if failed == total {
                "failed".into()
            } else {
                "partial".into()
            }
        }
    }
}

fn failure_kind_str(reason: &FailureReason) -> String {
    match reason {
        FailureReason::RateLimit { .. } => "rate_limit",
        FailureReason::NetworkError { .. } => "network_error",
        FailureReason::AuthFailure => "auth_failure",
        FailureReason::ContextExceeded => "context_exceeded",
        FailureReason::InvalidArgument { .. } => "invalid_argument",
        FailureReason::Unknown { .. } => "unknown",
    }
    .into()
}

fn failure_message_and_template(reason: &FailureReason) -> (Option<String>, Option<String>) {
    let msg = match reason {
        FailureReason::NetworkError { message }
        | FailureReason::InvalidArgument { message }
        | FailureReason::Unknown { message } => Some(message.clone()),
        _ => None,
    };
    let template = msg.as_deref().map(canonicalize);
    (msg, template)
}

fn synthetic_kind(status: &TaskStatus) -> String {
    match status {
        TaskStatus::Failed => "failed_no_reason",
        TaskStatus::TimedOut => "timed_out",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::SpawnFailed => "spawn_failed",
        TaskStatus::ApprovalRejected => "approval_rejected",
        TaskStatus::ApprovalTimedOut => "approval_timed_out",
        TaskStatus::Success => "success", // unreachable in caller
    }
    .into()
}

fn is_success(status: &TaskStatus) -> bool {
    matches!(status, TaskStatus::Success)
}

#[derive(Deserialize)]
struct ResolvedNameOnly {
    name: Option<String>,
}

fn read_resolved_name(run_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(run_dir.join("resolved.json")).ok()?;
    serde_json::from_slice::<ResolvedNameOnly>(&bytes)
        .ok()
        .and_then(|r| r.name)
}

#[derive(Deserialize)]
struct MetaPathOnly {
    manifest_path: Option<String>,
}

fn read_meta_manifest_path(run_dir: &Path) -> Option<String> {
    let bytes = std::fs::read(run_dir.join("meta.json")).ok()?;
    serde_json::from_slice::<MetaPathOnly>(&bytes)
        .ok()
        .and_then(|m| m.manifest_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_cascade_prefers_explicit() {
        let n = resolve_manifest_name(Some("nightly"), Some("/abs/foo.toml"));
        assert_eq!(n, "nightly");
    }

    #[test]
    fn name_cascade_falls_back_to_basename() {
        let n = resolve_manifest_name(None, Some("/abs/path/foo.toml"));
        assert_eq!(n, "foo");
    }

    #[test]
    fn name_cascade_empty_string_falls_through() {
        let n = resolve_manifest_name(Some("   "), Some("/abs/bar.toml"));
        assert_eq!(n, "bar");
    }

    #[test]
    fn name_cascade_falls_back_to_unnamed() {
        let n = resolve_manifest_name(None, None);
        assert_eq!(n, "<unnamed>");
    }

    #[test]
    fn outcome_complete_no_failures_is_success() {
        assert_eq!(compute_outcome(&RunStatus::Complete, 0, 3), "success");
    }

    #[test]
    fn outcome_complete_all_failed_is_failed() {
        assert_eq!(compute_outcome(&RunStatus::Complete, 3, 3), "failed");
    }

    #[test]
    fn outcome_complete_some_failed_is_partial() {
        assert_eq!(compute_outcome(&RunStatus::Complete, 1, 3), "partial");
    }
}
