#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::manifest::resolve::{ResolvedManifest, ResolvedTask};

/// Given a prior run directory (e.g. `~/.local/share/shire/runs/<run-id>/`),
/// read `resolved.json` and `summary.json` and build a fresh `ResolvedManifest`
/// whose tasks have `resume_session_id` populated from the prior run's
/// `claude_session_id` fields.
///
/// Tasks that completed in the prior run with a `claude_session_id` are
/// included with `resume_session_id` set.
///
/// Tasks that never ran (e.g. cut off by `halt_on_failure`) are excluded.
///
/// Tasks that ran but have no `claude_session_id` (e.g. `SpawnFailed`) are
/// skipped with a warning — the caller asked to *resume*, not retry.
///
/// Returns an error if no tasks can be resumed (nothing to do).
pub fn build_resume_manifest(run_dir: &Path) -> Result<ResolvedManifest> {
    // --- load resolved.json ------------------------------------------------
    let resolved_path = run_dir.join("resolved.json");
    let resolved_bytes = std::fs::read(&resolved_path).with_context(|| {
        format!(
            "resolved.json not found at {}; run may predate v0.1.0 or was never started",
            resolved_path.display()
        )
    })?;
    let mut base: ResolvedManifest = serde_json::from_slice(&resolved_bytes)
        .with_context(|| format!("parsing resolved.json at {}", resolved_path.display()))?;

    // --- load summary.json -------------------------------------------------
    let summary_path = run_dir.join("summary.json");
    let summary_bytes = std::fs::read(&summary_path).with_context(|| {
        format!(
            "summary.json not found at {}; the prior run may not have finished",
            summary_path.display()
        )
    })?;
    let summary: mosaic_core::store::RunSummary = serde_json::from_slice(&summary_bytes)
        .with_context(|| format!("parsing summary.json at {}", summary_path.display()))?;

    // Build lookup: task_id → claude_session_id (if any)
    let session_ids: HashMap<String, Option<String>> = summary
        .tasks
        .iter()
        .map(|r| (r.task_id.clone(), r.claude_session_id.clone()))
        .collect();

    // Filter and annotate tasks.
    let mut resumed_tasks: Vec<ResolvedTask> = Vec::new();

    for task in base.tasks.drain(..) {
        match session_ids.get(&task.id) {
            None => {
                // Task was in resolved.json but never ran (halt_on_failure cascade).
                // Skip silently — it was never started, resuming makes no sense.
                tracing::debug!(
                    task_id = %task.id,
                    "skipping task: not present in prior summary (was never run)"
                );
            }
            Some(None) => {
                // Task ran but produced no session id (SpawnFailed or similar).
                // Warn: the user asked to resume, not retry.
                let prior_status = summary
                    .tasks
                    .iter()
                    .find(|r| r.task_id == task.id)
                    .map(|r| format!("{:?}", r.status))
                    .unwrap_or_default();
                eprintln!(
                    "shire resume: skipping task '{}' (no claude_session_id; prior status: {})",
                    task.id, prior_status
                );
            }
            Some(Some(sid)) => {
                resumed_tasks.push(ResolvedTask {
                    resume_session_id: Some(sid.clone()),
                    ..task
                });
            }
        }
    }

    if resumed_tasks.is_empty() {
        bail!(
            "no tasks with a claude_session_id found in the prior run; nothing to resume.\n\
             Tasks that SpawnFailed or were cancelled before starting cannot be resumed — \
             use 'shire dispatch' to run from scratch."
        );
    }

    Ok(ResolvedManifest {
        tasks: resumed_tasks,
        ..base
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mosaic_core::parser::TokenUsage;
    use mosaic_core::store::{RunSummary, TaskRecord, TaskStatus};
    use tempfile::TempDir;
    use uuid::Uuid;

    fn write_resolved(dir: &Path, tasks: &[(&str, &str)]) {
        // tasks: [(id, prompt)]
        let resolved_tasks: Vec<serde_json::Value> = tasks
            .iter()
            .map(|(id, prompt)| {
                serde_json::json!({
                    "id": id,
                    "directory": "/tmp",
                    "prompt": prompt,
                    "branch": null,
                    "model": "claude-test",
                    "effort": "high",
                    "tools": [],
                    "timeout_secs": 30,
                    "use_worktree": false,
                    "env": {}
                })
            })
            .collect();

        let resolved = serde_json::json!({
            "max_parallel": 2,
            "halt_on_failure": false,
            "run_dir": "/tmp/runs",
            "worktree_cleanup": "on_success",
            "emit_event_stream": false,
            "tasks": resolved_tasks
        });
        std::fs::write(
            dir.join("resolved.json"),
            serde_json::to_vec_pretty(&resolved).unwrap(),
        )
        .unwrap();
    }

    fn write_summary(dir: &Path, task_records: &[(&str, Option<&str>, TaskStatus)]) {
        let now = Utc::now();
        let run_id = Uuid::now_v7();
        let tasks: Vec<TaskRecord> = task_records
            .iter()
            .map(|(id, session_id, status)| TaskRecord {
                task_id: id.to_string(),
                status: status.clone(),
                exit_code: Some(0),
                started_at: now,
                ended_at: now,
                duration_ms: 1000,
                worktree_path: None,
                log_path: std::path::PathBuf::from("/tmp/log"),
                token_usage: TokenUsage::default(),
                claude_session_id: session_id.map(str::to_string),
                final_message_preview: None,
                parent_task_id: None,
            })
            .collect();

        let summary = RunSummary {
            run_id,
            manifest_path: std::path::PathBuf::from("/tmp/shire.toml"),
            shire_version: "0.1.0".into(),
            claude_version: None,
            started_at: now,
            ended_at: now,
            total_duration_ms: 1000,
            tasks_total: tasks.len(),
            tasks_failed: 0,
            was_interrupted: false,
            tasks,
        };
        std::fs::write(
            dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn happy_path_populates_resume_session_ids() {
        let tmp = TempDir::new().unwrap();
        write_resolved(tmp.path(), &[("a", "do A"), ("b", "do B")]);
        write_summary(
            tmp.path(),
            &[
                ("a", Some("sess_a_123"), TaskStatus::Success),
                ("b", Some("sess_b_456"), TaskStatus::Success),
            ],
        );

        let manifest = build_resume_manifest(tmp.path()).unwrap();
        assert_eq!(manifest.tasks.len(), 2);

        let a = manifest.tasks.iter().find(|t| t.id == "a").unwrap();
        assert_eq!(a.resume_session_id.as_deref(), Some("sess_a_123"));
        assert_eq!(a.prompt, "do A");

        let b = manifest.tasks.iter().find(|t| t.id == "b").unwrap();
        assert_eq!(b.resume_session_id.as_deref(), Some("sess_b_456"));
    }

    #[test]
    fn skips_tasks_with_no_session_id_and_warns() {
        let tmp = TempDir::new().unwrap();
        write_resolved(tmp.path(), &[("ok", "prompt ok"), ("fail", "prompt fail")]);
        write_summary(
            tmp.path(),
            &[
                ("ok", Some("sess_ok"), TaskStatus::Success),
                ("fail", None, TaskStatus::SpawnFailed),
            ],
        );

        let manifest = build_resume_manifest(tmp.path()).unwrap();
        // Only 'ok' should be resumed
        assert_eq!(manifest.tasks.len(), 1);
        assert_eq!(manifest.tasks[0].id, "ok");
        assert_eq!(
            manifest.tasks[0].resume_session_id.as_deref(),
            Some("sess_ok")
        );
    }

    #[test]
    fn skips_tasks_never_run_halt_on_failure() {
        let tmp = TempDir::new().unwrap();
        // resolved.json has 3 tasks; summary only has 1 (others cut by halt_on_failure)
        write_resolved(tmp.path(), &[("t1", "p1"), ("t2", "p2"), ("t3", "p3")]);
        write_summary(tmp.path(), &[("t1", Some("sess_t1"), TaskStatus::Failed)]);

        let manifest = build_resume_manifest(tmp.path()).unwrap();
        // Only t1 resumes; t2 and t3 were never run
        assert_eq!(manifest.tasks.len(), 1);
        assert_eq!(manifest.tasks[0].id, "t1");
    }

    #[test]
    fn errors_when_no_sessions_available() {
        let tmp = TempDir::new().unwrap();
        write_resolved(tmp.path(), &[("a", "p")]);
        write_summary(tmp.path(), &[("a", None, TaskStatus::SpawnFailed)]);

        let err = build_resume_manifest(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("nothing to resume"),
            "expected 'nothing to resume' in error: {err}"
        );
    }

    #[test]
    fn errors_when_resolved_json_missing() {
        let tmp = TempDir::new().unwrap();
        // No resolved.json written
        let err = build_resume_manifest(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("resolved.json not found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolved_json_without_resume_field_deserializes_ok() {
        let tmp = TempDir::new().unwrap();
        // Simulate an old resolved.json without resume_session_id field
        let old_resolved = serde_json::json!({
            "max_parallel": 1,
            "halt_on_failure": false,
            "run_dir": "/tmp/runs",
            "worktree_cleanup": "on_success",
            "emit_event_stream": false,
            "tasks": [{
                "id": "x",
                "directory": "/tmp",
                "prompt": "old prompt",
                "branch": null,
                "model": "claude-test",
                "effort": "high",
                "tools": [],
                "timeout_secs": 30,
                "use_worktree": false,
                "env": {}
                // no resume_session_id field
            }]
        });
        std::fs::write(
            tmp.path().join("resolved.json"),
            serde_json::to_vec_pretty(&old_resolved).unwrap(),
        )
        .unwrap();
        write_summary(tmp.path(), &[("x", Some("sess_x"), TaskStatus::Success)]);

        let manifest = build_resume_manifest(tmp.path()).unwrap();
        assert_eq!(manifest.tasks.len(), 1);
        assert_eq!(
            manifest.tasks[0].resume_session_id.as_deref(),
            Some("sess_x")
        );
    }
}
