#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use pitboss_core::store::RunSummary;

use crate::manifest::resolve::{ResolvedManifest, ResolvedTask};

/// Max length for a `claude --resume <id>` value. Real Claude session IDs
/// are UUIDs (36 chars); we allow a generous upper bound while still refusing
/// anything that could plausibly be crafted to blow past a sane arg length.
const MAX_SESSION_ID_LEN: usize = 128;

/// Reject session ids that are empty, too long, start with `-` (would be
/// mis-parsed by the `claude` CLI as a flag), or contain characters outside
/// a conservative set. `summary.json` is on-disk state any local writer
/// could tamper with; a malformed or adversarial value there would otherwise
/// flow straight into `claude --resume <ID>`.
fn validate_session_id(sid: &str) -> Result<()> {
    if sid.is_empty() {
        bail!("claude_session_id is empty");
    }
    if sid.len() > MAX_SESSION_ID_LEN {
        bail!(
            "claude_session_id is {} chars, max {}",
            sid.len(),
            MAX_SESSION_ID_LEN
        );
    }
    if sid.starts_with('-') {
        bail!("claude_session_id must not start with '-': {sid:?}");
    }
    if !sid
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("claude_session_id contains disallowed characters: {sid:?}");
    }
    Ok(())
}

/// Given a prior run directory (e.g. `~/.local/share/pitboss/runs/<run-id>/`),
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
    let summary: pitboss_core::store::RunSummary = serde_json::from_slice(&summary_bytes)
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
                    "pitboss resume: skipping task '{}' (no claude_session_id; prior status: {})",
                    task.id, prior_status
                );
            }
            Some(Some(sid)) => {
                validate_session_id(sid).with_context(|| {
                    format!(
                        "rejecting tampered claude_session_id for task {:?} in summary.json",
                        task.id
                    )
                })?;
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
             use 'pitboss dispatch' to run from scratch."
        );
    }

    Ok(ResolvedManifest {
        tasks: resumed_tasks,
        ..base
    })
}

/// Entry in `subleads.jsonl` — one line per terminated sub-lead that had a
/// session id. Written by `spawn_sublead_session` at sub-lead termination.
#[derive(serde::Deserialize)]
struct SubleadSessionEntry {
    sublead_id: String,
    session_id: String,
}

/// Hierarchical-mode counterpart to `build_resume_manifest`. Reads
/// `resolved.json` and `summary.json` from a prior hierarchical run, extracts
/// the lead's `claude_session_id`, and returns a `ResolvedManifest` whose
/// `lead.resume_session_id` is set so the caller can re-spawn the lead with
/// `--resume`. Workers are NOT resumed — the lead decides what work to
/// dispatch.
///
/// Also reads `subleads.jsonl` (if present) and returns a map of
/// `sublead_id → claude_session_id` so the caller can seed the shared store
/// for the root lead to discover prior sub-lead sessions.
///
/// Errors if:
/// - `resolved.json` / `summary.json` missing or unparseable
/// - the prior run was not hierarchical (`lead.is_none()` in `resolved.json`)
/// - the lead's record is not in `summary.json`
/// - the lead has no `claude_session_id` (e.g. SpawnFailed before any output)
pub fn build_resume_hierarchical(
    run_dir: &Path,
) -> Result<(ResolvedManifest, HashMap<String, String>)> {
    let resolved_path = run_dir.join("resolved.json");
    let bytes = std::fs::read(&resolved_path)
        .with_context(|| format!("reading {}", resolved_path.display()))?;
    let mut resolved: ResolvedManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", resolved_path.display()))?;

    let lead = resolved
        .lead
        .as_mut()
        .ok_or_else(|| anyhow!("run is not hierarchical (no lead in resolved.json)"))?;

    let summary_path = run_dir.join("summary.json");
    let summary_bytes = std::fs::read(&summary_path)
        .with_context(|| format!("reading {}", summary_path.display()))?;
    let summary: RunSummary = serde_json::from_slice(&summary_bytes)
        .with_context(|| format!("parsing {}", summary_path.display()))?;

    let lead_record = summary
        .tasks
        .iter()
        .find(|r| r.task_id == lead.id)
        .ok_or_else(|| anyhow!("no lead TaskRecord in summary"))?;

    let session_id = lead_record
        .claude_session_id
        .clone()
        .ok_or_else(|| anyhow!("lead has no claude_session_id — cannot resume"))?;
    validate_session_id(&session_id)
        .context("rejecting tampered claude_session_id for lead in summary.json")?;

    // Guard: if the prior run used a worktree that's since been cleaned,
    // `claude --resume` will fail to find its session data (claude keys
    // session storage by cwd hash). Fail fast with a clear message that
    // points at `worktree_cleanup = "never"` — otherwise the user would
    // see a cryptic claude-level error after we've already re-spawned.
    if let Some(wt) = &lead_record.worktree_path {
        if !wt.exists() {
            anyhow::bail!(
                "cannot resume: the lead's worktree was cleaned ({}).\n\
                 claude --resume needs the original worktree directory to\n\
                 find its session data. Re-run the original manifest with\n\
                 `[run] worktree_cleanup = \"never\"` so future resumes\n\
                 stay possible.",
                wt.display()
            );
        }
    }

    lead.resume_session_id = Some(session_id);

    // Workers are dispatched dynamically by the lead — the `tasks` vec
    // shouldn't carry flat-mode tasks anyway. Clear it defensively so a
    // downstream flat-mode code path can't accidentally pick them up.
    resolved.tasks.clear();

    // Read subleads.jsonl if present; ignore missing / corrupt lines
    // (best-effort — a partial file from an interrupted run is OK).
    let mut sublead_sessions: HashMap<String, String> = HashMap::new();
    let subleads_jsonl = run_dir.join("subleads.jsonl");
    if subleads_jsonl.exists() {
        if let Ok(content) = std::fs::read_to_string(&subleads_jsonl) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<SubleadSessionEntry>(line) {
                    Ok(entry) => {
                        if let Err(e) = validate_session_id(&entry.session_id) {
                            tracing::warn!(
                                sublead_id = %entry.sublead_id,
                                error = %e,
                                "skipping sublead entry with invalid session_id in subleads.jsonl"
                            );
                        } else {
                            sublead_sessions.insert(entry.sublead_id, entry.session_id);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "skipping unparseable line in subleads.jsonl"
                        );
                    }
                }
            }
        }
    }

    Ok((resolved, sublead_sessions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pitboss_core::parser::TokenUsage;
    use pitboss_core::store::{RunSummary, TaskRecord, TaskStatus};
    use tempfile::TempDir;
    use uuid::Uuid;

    #[test]
    fn validate_session_id_accepts_uuid() {
        assert!(validate_session_id("019da1bb-7820-7d73-92ea-146e21f77dd8").is_ok());
        assert!(validate_session_id("sess_abc-123_DEF").is_ok());
    }

    #[test]
    fn validate_session_id_rejects_dangerous_values() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("--dangerous-flag").is_err());
        assert!(validate_session_id("-p").is_err());
        assert!(validate_session_id("has space").is_err());
        assert!(validate_session_id("has/slash").is_err());
        assert!(validate_session_id("has;semi").is_err());
        assert!(validate_session_id("null\0byte").is_err());
        let too_long = "a".repeat(MAX_SESSION_ID_LEN + 1);
        assert!(validate_session_id(&too_long).is_err());
    }

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
                pause_count: 0,
                reprompt_count: 0,
                approvals_requested: 0,
                approvals_approved: 0,
                approvals_rejected: 0,
                model: None,
                failure_reason: None,
            })
            .collect();

        let summary = RunSummary {
            run_id,
            manifest_path: std::path::PathBuf::from("/tmp/pitboss.toml"),
            pitboss_version: "0.1.0".into(),
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

    #[test]
    fn build_resume_hierarchical_populates_session_id() {
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use pitboss_core::store::TaskRecord;
        use pitboss_core::store::TaskStatus;
        use std::path::PathBuf;

        let dir = TempDir::new().unwrap();
        let run_dir = dir.path();

        // Synthesize resolved.json with a lead.
        let mut resolved = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: run_dir.to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(ResolvedLead {
                id: "triage".into(),
                directory: PathBuf::from("/tmp"),
                prompt: "original".into(),
                branch: None,
                model: "claude-haiku-4-5".into(),
                effort: Effort::High,
                tools: vec![],
                timeout_secs: 600,
                use_worktree: false,
                env: Default::default(),
                resume_session_id: None,
                permission_routing: Default::default(),
                allow_subleads: false,
                max_subleads: None,
                max_sublead_budget_usd: None,
                max_workers_across_tree: None,
                sublead_defaults: None,
            }),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::to_vec_pretty(&resolved).unwrap(),
        )
        .unwrap();

        // Synthesize summary.json with the lead's record including a session id.
        let lead_record = TaskRecord {
            task_id: "triage".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 0,
            worktree_path: None,
            log_path: PathBuf::new(),
            token_usage: Default::default(),
            claude_session_id: Some("session-abc-123".into()),
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
        };
        let summary = RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::new(),
            pitboss_version: "0.3.0".into(),
            claude_version: None,
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            total_duration_ms: 0,
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            tasks: vec![lead_record],
        };
        std::fs::write(
            run_dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();

        let (resumed, sublead_sessions) = build_resume_hierarchical(run_dir).unwrap();
        let lead = resumed.lead.unwrap();
        assert_eq!(lead.resume_session_id.as_deref(), Some("session-abc-123"));
        assert!(sublead_sessions.is_empty(), "no subleads.jsonl → empty map");

        // Silence unused warning on the un-reserialized resolved.
        let _ = resolved.lead.take();
    }

    /// Regression: when the lead's worktree has been cleaned, resume must
    /// fail fast with a clear error rather than respawning claude in a
    /// new directory and letting `claude --resume <session>` mysteriously
    /// fail to find its session data.
    #[test]
    fn resume_fails_clearly_when_lead_worktree_missing() {
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use std::path::PathBuf;
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path();

        // Write a bare-bones resolved.json.
        let resolved = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: run_dir.to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(ResolvedLead {
                id: "triage".into(),
                directory: PathBuf::from("/tmp"),
                prompt: "original".into(),
                branch: None,
                model: "claude-haiku-4-5".into(),
                effort: Effort::High,
                tools: vec![],
                timeout_secs: 600,
                use_worktree: true,
                env: Default::default(),
                resume_session_id: None,
                permission_routing: Default::default(),
                allow_subleads: false,
                max_subleads: None,
                max_sublead_budget_usd: None,
                max_workers_across_tree: None,
                sublead_defaults: None,
            }),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::to_vec_pretty(&resolved).unwrap(),
        )
        .unwrap();

        // Lead record points at a worktree path that does NOT exist — as
        // if cleanup fired and removed it.
        let missing_wt = tmp.path().join("already-gone");
        assert!(!missing_wt.exists());
        let lead_record = TaskRecord {
            task_id: "triage".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 0,
            worktree_path: Some(missing_wt.clone()),
            log_path: PathBuf::new(),
            token_usage: Default::default(),
            claude_session_id: Some("session-gone".into()),
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
        };
        let summary = RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::new(),
            pitboss_version: "0.4.3".into(),
            claude_version: None,
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            total_duration_ms: 0,
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            tasks: vec![lead_record],
        };
        std::fs::write(
            run_dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();

        let err = build_resume_hierarchical(run_dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("worktree was cleaned"),
            "expected worktree-cleaned guidance, got: {msg}"
        );
        assert!(
            msg.contains("worktree_cleanup = \"never\""),
            "expected remediation hint, got: {msg}"
        );
    }

    #[test]
    fn build_resume_hierarchical_reads_subleads_jsonl() {
        use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
        use crate::manifest::schema::{Effort, WorktreeCleanup};
        use std::io::Write;
        use std::path::PathBuf;

        let dir = TempDir::new().unwrap();
        let run_dir = dir.path();

        let resolved = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: run_dir.to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(ResolvedLead {
                id: "root-lead".into(),
                directory: PathBuf::from("/tmp"),
                prompt: "root".into(),
                branch: None,
                model: "claude-haiku-4-5".into(),
                effort: Effort::High,
                tools: vec![],
                timeout_secs: 600,
                use_worktree: false,
                env: Default::default(),
                resume_session_id: None,
                permission_routing: Default::default(),
                allow_subleads: true,
                max_subleads: None,
                max_sublead_budget_usd: None,
                max_workers_across_tree: None,
                sublead_defaults: None,
            }),
            max_workers: Some(4),
            budget_usd: Some(10.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
        };
        std::fs::write(
            run_dir.join("resolved.json"),
            serde_json::to_vec_pretty(&resolved).unwrap(),
        )
        .unwrap();

        let lead_record = TaskRecord {
            task_id: "root-lead".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            duration_ms: 0,
            worktree_path: None,
            log_path: PathBuf::new(),
            token_usage: Default::default(),
            claude_session_id: Some("root-sess-001".into()),
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
        };
        let summary = RunSummary {
            run_id: Uuid::now_v7(),
            manifest_path: PathBuf::new(),
            pitboss_version: "0.8.0".into(),
            claude_version: None,
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            total_duration_ms: 0,
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            tasks: vec![lead_record],
        };
        std::fs::write(
            run_dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();

        // Write a subleads.jsonl with two entries (one valid, one corrupt).
        let subleads_jsonl = run_dir.join("subleads.jsonl");
        let mut f = std::fs::File::create(&subleads_jsonl).unwrap();
        writeln!(
            f,
            r#"{{"sublead_id":"sublead-aaa","session_id":"sub-sess-aaa"}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"sublead_id":"sublead-bbb","session_id":"sub-sess-bbb"}}"#
        )
        .unwrap();
        writeln!(f, "{{not valid json}}").unwrap(); // should be skipped gracefully

        let (resumed, sessions) = build_resume_hierarchical(run_dir).unwrap();
        assert_eq!(
            resumed.lead.unwrap().resume_session_id.as_deref(),
            Some("root-sess-001")
        );
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.get("sublead-aaa").map(String::as_str), Some("sub-sess-aaa"));
        assert_eq!(sessions.get("sublead-bbb").map(String::as_str), Some("sub-sess-bbb"));
    }
}
